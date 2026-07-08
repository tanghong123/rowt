//! Clipboard transport: OSC 52 first (works over SSH, no display server), with
//! an `arboard` fallback for terminals that block OSC 52. Paste is out of scope
//! for this read-only monitor.

use std::io::Write;

/// Copy `text` to the system clipboard. Best-effort — never panics.
pub fn copy(text: &str) {
    if osc52(text).is_ok() {
        // OSC 52 emitted; also try arboard so local pastes work even if the
        // terminal swallowed the escape. Ignore any failure.
        let _ = arboard_set(text);
        return;
    }
    let _ = arboard_set(text);
}

fn osc52(text: &str) -> std::io::Result<()> {
    // ESC ] 52 ; c ; <base64> BEL  — write straight to the controlling tty.
    let payload = base64(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", payload);
    // Prefer /dev/tty so this works even mid-alt-screen; fall back to stdout.
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        tty.write_all(seq.as_bytes())?;
        tty.flush()?;
    } else {
        let mut out = std::io::stdout();
        out.write_all(seq.as_bytes())?;
        out.flush()?;
    }
    Ok(())
}

fn arboard_set(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_string())?;
    Ok(())
}

/// Minimal standard base64 encoder (no dependency).
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;
    #[test]
    fn b64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"api.anthropic.com:443"), "YXBpLmFudGhyb3BpYy5jb206NDQz");
    }
}
