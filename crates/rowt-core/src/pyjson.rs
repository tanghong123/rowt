//! `json.dump(obj, fp, indent=2)` — Python's default, which is `ensure_ascii=True`.
//!
//! serde_json's pretty printer agrees on layout but not on strings: it emits
//! UTF-8, Python emits `\uXXXX` for everything outside printable ASCII. Share
//! links carry provider names in Chinese and Farsi, so the difference is not
//! theoretical — it shows up on the first real subscription.
//!
//! (`net-detect.py` passes `ensure_ascii=False` and so is served by plain
//! `to_string_pretty`. The two are deliberately different, not an oversight.)

use serde_json::Value;

/// The exact bytes `json.dump(v, sys.stdout, indent=2)` writes — no trailing
/// newline, which the caller adds, as the Python does.
pub fn dumps(v: &Value) -> String {
    let mut s = String::new();
    write(&mut s, v, 0);
    s
}

fn write(out: &mut String, v: &Value, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => escape(out, s),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                indent(out, level + 1);
                write(out, item, level + 1);
            }
            out.push('\n');
            indent(out, level);
            out.push(']');
        }
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                indent(out, level + 1);
                escape(out, k);
                out.push_str(": ");
                write(out, val, level + 1);
            }
            out.push('\n');
            indent(out, level);
            out.push('}');
        }
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// `py_encode_basestring_ascii` — escape anything outside ` `..`~`, plus the two
/// structural characters. Hex is lowercase, and an astral code point becomes a
/// surrogate PAIR, which is the one place this differs from just widening.
fn escape(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let n = c as u32;
                if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    let n = n - 0x10000;
                    out.push_str(&format!("\\u{:04x}", 0xd800 | ((n >> 10) & 0x3ff)));
                    out.push_str(&format!("\\u{:04x}", 0xdc00 | (n & 0x3ff)));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_ascii_is_escaped_and_astral_becomes_a_surrogate_pair() {
        // The expectations are assembled from a backslash rather than
        // written as escapes: what Python emits is the six ASCII characters
        // \u4e2d, and spelling that in a Rust literal reads as the
        // character it denotes instead of the text.
        let b = '\\';
        assert_eq!(dumps(&json!("\u{4e2d}/")), format!("\"{b}u4e2d/\""));
        assert_eq!(dumps(&json!("\u{1f600}")), format!("\"{b}ud83d{b}ude00\""));
        // DEL is outside printable ASCII, so Python escapes it too.
        assert_eq!(dumps(&json!("\u{7f}")), format!("\"{b}u007f\""));
        assert_eq!(
            dumps(&json!("\u{1}\u{1f}\t\n")),
            format!("\"{b}u0001{b}u001f{b}t{b}n\"")
        );
    }

    #[test]
    fn layout_matches_indent_two() {
        assert_eq!(dumps(&json!([])), "[]");
        assert_eq!(dumps(&json!({})), "{}");
        assert_eq!(dumps(&json!({"a": [1, 2]})), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
        // No space before the colon, one after — `separators=(',', ': ')`.
        assert_eq!(dumps(&json!({"a": 1.0})), "{\n  \"a\": 1.0\n}");
    }

    #[test]
    fn key_order_is_insertion_order_not_sorted() {
        let v: Value = serde_json::from_str(r#"{"z": 1, "a": 2}"#).unwrap();
        assert_eq!(dumps(&v), "{\n  \"z\": 1,\n  \"a\": 2\n}");
    }
}
