//! Byte-rate formatters, tuned to reproduce the ground-truth renders exactly.

/// Header aggregate rate: `("2.9", "MB/s")`, `("144", "KB/s")`, `("5", "KB/s")`.
/// MB gets one decimal; KB/B are integer.
pub fn rate_parts(bytes_per_s: f64) -> (String, &'static str) {
    let b = bytes_per_s;
    if b >= 1_000_000.0 {
        (format!("{:.1}", b / 1_000_000.0), "MB/s")
    } else if b >= 1_000.0 {
        (format!("{:.0}", b / 1_000.0), "KB/s")
    } else {
        (format!("{:.0}", b), "B/s")
    }
}

/// Compact per-connection byte figure for the table column: `8.0M`, `352.2M`,
/// `915K`, `104K`. M gets one decimal; K/B are integer. No `/s`, no arrow.
pub fn compact(bytes_per_s: f64) -> String {
    let b = bytes_per_s;
    if b >= 1_000_000.0 {
        format!("{:.1}M", b / 1_000_000.0)
    } else if b >= 1_000.0 {
        format!("{:.0}K", b / 1_000.0)
    } else {
        format!("{:.0}B", b)
    }
}
