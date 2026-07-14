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

/// Cumulative byte total for the metrics view, scaling all the way up (a year of
/// traffic reaches G/T): `1.2T`, `1.2G`, `48.6M`, `571K`, `0B`. One decimal from
/// M up; K/B integer. Kept separate from `compact` (which tops out at M for the
/// live per-connection column and must reproduce those exact strings).
pub fn bytes_total(bytes: f64) -> String {
    let b = bytes;
    if b >= 1_000_000_000_000.0 {
        format!("{:.1}T", b / 1_000_000_000_000.0)
    } else if b >= 1_000_000_000.0 {
        format!("{:.1}G", b / 1_000_000_000.0)
    } else if b >= 1_000_000.0 {
        format!("{:.1}M", b / 1_000_000.0)
    } else if b >= 1_000.0 {
        format!("{:.0}K", b / 1_000.0)
    } else {
        format!("{:.0}B", b)
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
