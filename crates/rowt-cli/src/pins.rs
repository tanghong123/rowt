//! Certificate pins on hysteria2 servers — the port of `_pins_resolve` and its
//! helpers in bin/rowt.
//!
//! A hysteria2 link can pin the server's certificate (`pinSHA256`, the SHA-256
//! of the whole certificate; Hysteria 2's own docs recommend it beside
//! `insecure=1` for a self-signed server; Shadowrocket keeps it as `hpkp`,
//! Clash as `fingerprint`). sing-box pins a PUBLIC KEY
//! (`tls.certificate_public_key_sha256`) and, at 1.13, has no field for a
//! certificate hash — and one hash cannot be made from the other without the
//! certificate. So at add time rowt fetches the certificate once over QUIC,
//! checks it against the pin, and records the public-key hash; a mismatch, or
//! a certificate it cannot fetch, refuses the server (Hong, rowt #s64).
//!
//! The parsers carry the pin as `_pin_sha256`, a key no sing-box config may
//! hold: this step consumes it on `server add` and `server import --apply`,
//! and `sharelink::combine` strips whatever no fetch consumed.
//!
//! Every external step is the shell's, run the same way, so both sides see the
//! same bytes and leave the same argv trace: the QUIC handshake through the
//! one openssl that can speak it (`quic_openssl`), the hashes through
//! `/usr/bin/openssl` — macOS's own LibreSSL, which has no QUIC but every
//! `x509`/`pkey`/`dgst` this needs — by absolute path, so the parity sandbox's
//! shim only ever has to answer `s_client`.

use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The hashes run through macOS's own openssl, by absolute path.
const LOCAL_OPENSSL: &str = "/usr/bin/openssl";

/// `PIN_TIMEOUT="${ROWT_PIN_TIMEOUT:-10}"` — seconds a QUIC handshake may take.
fn timeout_secs() -> u64 {
    crate::env_or("ROWT_PIN_TIMEOUT", "10").parse().unwrap_or(10)
}

fn is_executable(p: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// `have` — `command -v` over PATH.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(name).to_string_lossy())))
        .unwrap_or(false)
}

/// `_pin_openssl`: the QUIC-capable openssl. `ROWT_OPENSSL` alone when set
/// (and nothing else — a wrong path fails clearly rather than falling back),
/// else `openssl` on PATH, else Homebrew's. Each candidate is asked
/// `s_client -help` and taken when the text names `-quic`: macOS's LibreSSL
/// does not, OpenSSL 3.2+ does. PATH before Homebrew on purpose: the parity
/// sandbox puts its shim on PATH, and the real Homebrew binary would otherwise
/// be reached from inside a gate.
fn quic_openssl() -> Option<String> {
    let candidates: Vec<String> = match std::env::var("ROWT_OPENSSL") {
        Ok(v) if !v.is_empty() => vec![v],
        _ => [
            "openssl",
            "/opt/homebrew/opt/openssl@3/bin/openssl",
            "/opt/homebrew/bin/openssl",
            "/usr/local/opt/openssl@3/bin/openssl",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    };
    for c in candidates {
        let present = if c.contains('/') { is_executable(&c) } else { on_path(&c) };
        if !present {
            continue;
        }
        let Ok(o) = Command::new(&c).args(["s_client", "-help"]).stdin(Stdio::null()).output() else { continue };
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        if text.contains("-quic") {
            return Some(c);
        }
    }
    None
}

/// `_pin_hex`: the pin with colons out and lowercased, when it is 64 hex digits.
pub fn pin_hex(pin: &str) -> Option<String> {
    let p: String = pin.chars().filter(|c| *c != ':').map(|c| c.to_ascii_lowercase()).collect();
    (p.len() == 64 && p.chars().all(|c| c.is_ascii_hexdigit())).then_some(p)
}

/// The first `-----BEGIN CERTIFICATE-----` … `-----END CERTIFICATE-----` block
/// of an `s_client -showcerts` transcript — the leaf — or None without one.
pub fn leaf_pem(transcript: &str) -> Option<String> {
    let mut out = String::new();
    let mut inside = false;
    for line in transcript.lines() {
        if !inside && line == "-----BEGIN CERTIFICATE-----" {
            inside = true;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
            if line == "-----END CERTIFICATE-----" {
                return Some(out);
            }
        }
    }
    None
}

/// Run a command with `input` on stdin and return its stdout, or None when it
/// could not run or failed.
fn run_stdin(cmd: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A failed write (the child died early) shows in the status below.
        let _ = stdin.write_all(input);
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// `_pin_cert_sha256`: the certificate's SHA-256 over its DER, as lowercase
/// hex — the form `pinSHA256` takes.
pub fn cert_sha256_hex(pem: &str) -> Option<String> {
    let der = run_stdin(LOCAL_OPENSSL, &["x509", "-outform", "der"], pem.as_bytes())?;
    let hex = run_stdin(LOCAL_OPENSSL, &["dgst", "-sha256", "-hex"], &der)?;
    // `awk '{print $NF}'` — `SHA256(stdin)= <hex>`, LibreSSL's spelling.
    String::from_utf8_lossy(&hex).split_whitespace().last().map(|s| s.to_string())
}

/// `_pin_spki_b64`: the SHA-256 of the certificate's DER SubjectPublicKeyInfo,
/// base64 — what `tls.certificate_public_key_sha256` holds and what sing-box
/// computes with `x509.MarshalPKIXPublicKey` on the leaf.
pub fn spki_sha256_b64(pem: &str) -> Option<String> {
    let pubkey = run_stdin(LOCAL_OPENSSL, &["x509", "-pubkey", "-noout"], pem.as_bytes())?;
    let der = run_stdin(LOCAL_OPENSSL, &["pkey", "-pubin", "-outform", "der"], &pubkey)?;
    let digest = run_stdin(LOCAL_OPENSSL, &["dgst", "-sha256", "-binary"], &der)?;
    (digest.len() == 32).then(|| rowt_core::foreign::b64encode(&digest))
}

/// `_pin_leaf_pem`: one QUIC handshake, under the timeout, and the leaf out of
/// its transcript. `-connect` LAST, because the parity recorder keys a canned
/// answer by a command's last argument; a bare IPv6 address is bracketed.
fn fetch_leaf(ossl: &str, host: &str, port: &str, sni: &str, alpn: &str) -> Option<String> {
    let hostport = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut child = Command::new(ossl)
        .args(["s_client", "-quic", "-alpn", alpn, "-servername", sni, "-showcerts", "-connect", &hostport])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read stdout on its own thread, so a transcript longer than the pipe
    // cannot stall a child the timeout is waiting on.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let limit = Duration::from_secs(timeout_secs());
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            // `_tmo`'s alarm: past the cap the handshake is killed and reads as no answer.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let transcript = reader.join().ok()?;
    leaf_pem(&String::from_utf8_lossy(&transcript))
}

fn gs(o: &Value, k: &str) -> String {
    match o.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

/// One outbound through the check. `Some(outbound)` with the pin consumed and
/// the public-key hash recorded, `None` when the server is refused — each
/// outcome said on stderr in the shell's words. `ossl` is the QUIC-capable
/// openssl, found once per command (None: there is none).
fn resolve_one(mut o: Value, ossl: Option<&str>) -> Option<Value> {
    let pin = gs(&o, "_pin_sha256");
    let tag = match o.get("tag") {
        Some(Value::String(t)) if !t.is_empty() => t.clone(),
        _ => "?".to_string(),
    };
    let refuse = |why: String| {
        eprintln!("refused: '{tag}' — {why}; not added");
        None
    };
    let Some(hex) = pin_hex(&pin) else {
        return refuse(format!("its certificate pin is not a SHA-256 ({pin})"));
    };
    let Some(ossl) = ossl else {
        return refuse("no QUIC-capable openssl to check its certificate pin with (brew install openssl@3, or set ROWT_OPENSSL)".into());
    };
    if o.get("obfs").is_some_and(|v| !v.is_null()) {
        return refuse("its certificate pin cannot be checked through Salamander obfs (a plain QUIC client cannot reach the server's certificate)".into());
    }
    let host = gs(&o, "server");
    let port = gs(&o, "server_port");
    let tls = o.get("tls").cloned().unwrap_or(Value::Null);
    let sni = match tls.get("server_name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => host.clone(),
    };
    let alpn = tls
        .get("alpn")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "h3".to_string());
    let Some(pem) = fetch_leaf(ossl, &host, &port, &sni, &alpn) else {
        return refuse(format!(
            "could not fetch the server's certificate over QUIC from {host}:{port} within {} s; its pin cannot be checked, so it is not added — retry when the server is reachable",
            timeout_secs()
        ));
    };
    let Some(got) = cert_sha256_hex(&pem) else {
        return refuse("could not read the certificate the server sent".into());
    };
    if got != hex {
        return refuse(format!("the server's certificate does not match its pin (certificate SHA-256 {got}, pinned {hex})"));
    }
    let Some(spki) = spki_sha256_b64(&pem) else {
        return refuse("could not read the server's public key from its certificate".into());
    };
    if let Some(m) = o.as_object_mut() {
        m.remove("_pin_sha256");
        let tls = m.entry("tls").or_insert_with(|| Value::Object(Default::default()));
        if let Some(t) = tls.as_object_mut() {
            t.insert("certificate_public_key_sha256".into(), Value::Array(vec![Value::String(spki)]));
        }
    }
    eprintln!("pinned: '{tag}' — the server's certificate matches its pin; its public key is recorded, and sing-box checks it on every connection");
    Some(o)
}

/// `_pins_resolve .`: every outbound that carries `_pin_sha256` goes through
/// the check; the refused ones are dropped, the rest pass through untouched.
/// The openssl is looked for once, and only when something carries a pin, so
/// a pool with none leaves no trace of the probe.
pub fn resolve(outbounds: Vec<Value>) -> Vec<Value> {
    if !outbounds.iter().any(|o| o.get("_pin_sha256").is_some()) {
        return outbounds;
    }
    let ossl = quic_openssl();
    outbounds
        .into_iter()
        .filter_map(|o| if o.get("_pin_sha256").is_some() { resolve_one(o, ossl.as_deref()) } else { Some(o) })
        .collect()
}

/// `_pins_resolve .o`: the same over import pairs, whose outbound sits beside
/// the source it came from.
pub fn resolve_pairs(pairs: Vec<(String, Value)>) -> Vec<(String, Value)> {
    if !pairs.iter().any(|(_, o)| o.get("_pin_sha256").is_some()) {
        return pairs;
    }
    let ossl = quic_openssl();
    pairs
        .into_iter()
        .filter_map(|(s, o)| {
            if o.get("_pin_sha256").is_some() {
                resolve_one(o, ossl.as_deref()).map(|o| (s, o))
            } else {
                Some((s, o))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parity fixture's certificate (CN=hy2.example, a documentation name),
    /// as `s_client -showcerts` prints it — the same bytes the sandbox's shim
    /// serves, so the hashes below are the ones the cli-diff cases rely on.
    fn transcript() -> String {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/parity/fixtures/scenarios/pinned-hy2/env/openssl_203_0_113_9_443");
        std::fs::read_to_string(p).expect("the pinned-hy2 fixture")
    }

    #[test]
    fn a_pin_is_colons_out_lowercase_and_sixty_four_hex_digits() {
        let hex = "98897FEC:BE07A01D537F3ECC63E465D0E1C376FE62BAFDF0E5660039492E9B41";
        assert_eq!(pin_hex(hex).as_deref(), Some("98897fecbe07a01d537f3ecc63e465d0e1c376fe62bafdf0e5660039492e9b41"));
        for bad in ["", "abc", "s64lWhnDpjsjTh8QS04Byf6xk2p+8ZeG4M6xonj7juE=", &"0".repeat(63), &"g".repeat(64)] {
            assert_eq!(pin_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_leaf_is_the_first_certificate_block_of_the_transcript() {
        let pem = leaf_pem(&transcript()).expect("a certificate");
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n") && pem.ends_with("-----END CERTIFICATE-----\n"));
        assert_eq!(pem.matches("BEGIN CERTIFICATE").count(), 1);
        assert_eq!(leaf_pem("CONNECTED(00000006)\nno certificate here\n"), None);
    }

    #[test]
    fn the_fixture_certificate_hashes_to_the_pin_and_public_key_the_cases_use() {
        let pem = leaf_pem(&transcript()).unwrap();
        assert_eq!(cert_sha256_hex(&pem).as_deref(), Some("98897fecbe07a01d537f3ecc63e465d0e1c376fe62bafdf0e5660039492e9b41"));
        assert_eq!(spki_sha256_b64(&pem).as_deref(), Some("s64lWhnDpjsjTh8QS04Byf6xk2p+8ZeG4M6xonj7juE="));
    }

    #[test]
    fn a_pool_without_a_pin_passes_through_untouched() {
        let v = vec![serde_json::json!({"type": "vless", "tag": "a", "server": "h", "server_port": 443, "uuid": "u"})];
        assert_eq!(resolve(v.clone()), v);
        let pairs = vec![("file:x".to_string(), v[0].clone())];
        assert_eq!(resolve_pairs(pairs.clone()), pairs);
    }
}
