//! POSIX `cksum` — the CRC-32 the shell's discovery journal keys its state on.
//!
//! `_discovery_journal` dedupes with `printf '%s' "$line" | cksum`, and stores
//! the number in `state` as `discovery_sig`. So this is not a hash chosen for
//! its properties; it is a specific number that has to come out the same, or
//! the two implementations write different state files and every tick
//! re-journals a network that has not changed.
//!
//! It is NOT zlib's `crc32`, which is the trap here: same polynomial, opposite
//! bit order, no length fold. POSIX cksum feeds each byte most-significant-bit
//! first, then folds in the message LENGTH as base-256 digits least-significant
//! first, and complements the result.

/// `cksum`'s checksum of `data` — the first field of its output.
pub fn posix(data: &[u8]) -> u32 {
    const POLY: u32 = 0x04C1_1DB7;
    fn feed(crc: &mut u32, b: u8) {
        *crc ^= (b as u32) << 24;
        for _ in 0..8 {
            *crc = if *crc & 0x8000_0000 != 0 { (*crc << 1) ^ POLY } else { *crc << 1 };
        }
    }
    let mut crc: u32 = 0;
    for &b in data {
        feed(&mut crc, b);
    }
    let mut n = data.len() as u64;
    while n > 0 {
        feed(&mut crc, (n & 0xff) as u8);
        n >>= 8;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checked against `/usr/bin/cksum` on macOS. The empty input is the one
    /// that catches a missing final complement (it must be 0xFFFFFFFF, not 0),
    /// and the JSON line is the actual shape the discovery journal hashes.
    #[test]
    fn it_agrees_with_usr_bin_cksum() {
        assert_eq!(posix(b""), 4294967295);
        assert_eq!(posix(b"a"), 1220704766);
        assert_eq!(posix(b"hello\n"), 3015617425);
        assert_eq!(posix(b"{\"net\":\"x\",\"vpn_iface\":\"utun9\"}"), 1645102258);
        assert_eq!(posix(b"The quick brown fox\n"), 4037379161);
    }

    /// The length fold is what separates this from a plain CRC-32/MPEG-2: two
    /// inputs of different length cannot collide just because one is a prefix.
    #[test]
    fn the_message_length_is_part_of_the_checksum() {
        assert_ne!(posix(b"ab"), posix(b"ab\0"));
        // A length that crosses a base-256 digit still folds in both bytes.
        let long = vec![b'x'; 300];
        let longer = vec![b'x'; 301];
        assert_ne!(posix(&long), posix(&longer));
    }
}
