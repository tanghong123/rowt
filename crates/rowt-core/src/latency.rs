//! One server's latency from several delay samples — the figure `rowt ping`
//! prints, by the same rule the monitor's prober uses (rowt-monitor
//! `probe_round`, a separate workspace with its own copy).
//!
//! A single delay test over a lossy cross-border link spikes or fails on its
//! own, so each server is sampled three times back to back and the middle value
//! is the one worth ranking by. The rule for fewer successes is the monitor's:
//! two give their mean (floored, as integer division does), one gives itself,
//! none means the server did not answer at all.
//!
//! Only the figures rowt SHOWS follow it. `auto` still switches on sing-box's
//! own stored figure, which every delay test overwrites with its one result —
//! there is no API to store a median (task #s2).

/// How many samples one server gets.
pub const SAMPLES: usize = 3;

/// The figure from the samples that succeeded, in any order. `None` when none did.
pub fn aggregate(ok: &[u64]) -> Option<u64> {
    let mut v = ok.to_vec();
    v.sort_unstable();
    match v.as_slice() {
        [] => None,
        [a, b] => Some((a + b) / 2),
        values => Some(values[values.len() / 2]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_successes_give_the_middle_value_whatever_their_order() {
        assert_eq!(aggregate(&[300, 100, 200]), Some(200));
        assert_eq!(aggregate(&[100, 100, 900]), Some(100));
    }

    #[test]
    fn two_successes_give_their_mean_floored() {
        assert_eq!(aggregate(&[351, 250]), Some(300));
        assert_eq!(aggregate(&[200, 200]), Some(200));
    }

    #[test]
    fn one_success_is_the_figure_and_none_is_unreachable() {
        assert_eq!(aggregate(&[120]), Some(120));
        assert_eq!(aggregate(&[]), None);
    }
}
