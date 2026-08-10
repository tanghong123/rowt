//! `plistlib.load` for binary plists — what Shadowrocket's server store is.
//!
//! The store is an NSKeyedArchiver archive: one big `$objects` array whose
//! entries are dicts of `CF$UID` references back into the same array. Nothing
//! here understands the archiver — `sr-import.py` walks `$objects` directly and
//! dereferences one level — so this is only the container format, followed
//! marker for marker from CPython's `_BinaryPlistParser`.
//!
//! ## Two deliberate limits, both named rather than hidden
//!
//!   * **XML plists are not read.** `plistlib.load` sniffs the header and
//!     accepts either format; this accepts only `bplist00`. Shadowrocket writes
//!     binary in both of its store locations and `bin/rowt` never passes
//!     `--store`, so the XML path is unreachable through rowt — but it IS a
//!     difference, and an XML file gets an explicit error instead of a silent
//!     wrong answer.
//!   * **Integers wider than 16 bytes** (marker `0x15`..`0x1f`) are rejected.
//!     Apple's writer emits 1, 2, 4, 8 and 16; a 32-byte integer means a
//!     hand-built file, and Python would hand back a bignum.
//!
//! Everything else matches, including the parts that look like bugs: a short
//! read inside an integer or a UID yields a SMALLER number rather than an
//! error, `0x0f` is the empty byte string, and almost every malformed file
//! collapses to one `InvalidFileException` because the Python wraps the whole
//! parse in `except (OSError, IndexError, struct.error, OverflowError,
//! ValueError)`.

use std::collections::HashSet;

/// A value out of a plist, in Python's types.
#[derive(Debug, Clone, PartialEq)]
pub enum PlVal {
    None,
    Bool(bool),
    Int(i128),
    Real(f64),
    /// A naive `datetime.datetime`, already resolved from the Apple epoch —
    /// the conversion can raise `OverflowError`, which the parser turns into an
    /// invalid file, so it has to happen while parsing.
    Date(Dt),
    Data(Vec<u8>),
    Str(String),
    Uid(u64),
    Array(Vec<PlVal>),
    /// Insertion-ordered, as Python's dict is. Keys can be any hashable value,
    /// which in a real archive means strings but in a hand-built file need not.
    Dict(Vec<(PlVal, PlVal)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dt {
    pub y: i64,
    pub mo: u32,
    pub d: u32,
    pub h: u32,
    pub mi: u32,
    pub s: u32,
    pub us: u32,
}

impl Dt {
    /// `str(datetime)` — ISO with a space, and microseconds only when nonzero.
    pub fn to_py_str(&self) -> String {
        let base = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.y, self.mo, self.d, self.h, self.mi, self.s
        );
        if self.us == 0 {
            base
        } else {
            format!("{base}.{:06}", self.us)
        }
    }
    /// `repr(datetime)` — the constructor call, with trailing zero arguments
    /// dropped down to hour and minute, which are always shown.
    pub fn to_py_repr(&self) -> String {
        let mut s = format!(
            "datetime.datetime({}, {}, {}, {}, {}",
            self.y, self.mo, self.d, self.h, self.mi
        );
        if self.s != 0 || self.us != 0 {
            s.push_str(&format!(", {}", self.s));
        }
        if self.us != 0 {
            s.push_str(&format!(", {}", self.us));
        }
        s.push(')');
        s
    }
}

/// Why a parse failed. `Invalid` is `plistlib.InvalidFileException`, which is
/// what nearly everything collapses to; `NotBinary` is this module's own limit.
#[derive(Debug, Clone, PartialEq)]
pub enum PlErr {
    Invalid,
    NotBinary,
}

/// `plistlib.load(fp)` for a file already in memory.
pub fn load(buf: &[u8]) -> Result<PlVal, PlErr> {
    // `load` sniffs before it parses, and an unrecognized header is an invalid
    // file rather than a parse attempt.
    if buf.starts_with(b"bplist00") {
        return parse(buf).ok_or(PlErr::Invalid);
    }
    if looks_like_xml(buf) {
        return Err(PlErr::NotBinary);
    }
    Err(PlErr::Invalid)
}

/// `_is_fmt_xml` — the prefixes, and the same prefixes behind a BOM.
fn looks_like_xml(buf: &[u8]) -> bool {
    let head = &buf[..buf.len().min(32)];
    for pfx in [b"<?xml".as_slice(), b"<plist".as_slice()] {
        if head.starts_with(pfx) {
            return true;
        }
    }
    for (bom, wide, le) in [
        (b"\xef\xbb\xbf".as_slice(), false, false),
        (b"\xfe\xff".as_slice(), true, false),
        (b"\xff\xfe".as_slice(), true, true),
    ] {
        if !head.starts_with(bom) {
            continue;
        }
        for pfx in ["<?xml", "<plist"] {
            let mut want = bom.to_vec();
            for c in pfx.bytes() {
                if wide && !le {
                    want.push(0);
                    want.push(c);
                } else if wide {
                    want.push(c);
                    want.push(0);
                } else {
                    want.push(c);
                }
            }
            if head.starts_with(&want) {
                return true;
            }
        }
    }
    false
}

struct P<'a> {
    buf: &'a [u8],
    pos: usize,
    ref_size: usize,
    offsets: Vec<u64>,
    /// Refs on the current path. The Python builds containers around a
    /// placeholder so a cycle becomes a self-referential list; a plain
    /// recursive reader would not return at all, so a cycle is an invalid file
    /// here. `$objects` is flat and UIDs are leaves, so no real archive has one.
    path: HashSet<u64>,
}

fn parse(buf: &[u8]) -> Option<PlVal> {
    // `seek(-32, SEEK_END)` on a shorter file is an OSError.
    if buf.len() < 32 {
        return None;
    }
    let t = &buf[buf.len() - 32..];
    let offset_size = t[6] as usize;
    let ref_size = t[7] as usize;
    let num_objects = u64::from_be_bytes(t[8..16].try_into().ok()?);
    let top_object = u64::from_be_bytes(t[16..24].try_into().ok()?);
    let offset_table_offset = u64::from_be_bytes(t[24..32].try_into().ok()?);

    let mut p = P { buf, pos: 0, ref_size, offsets: Vec::new(), path: HashSet::new() };
    // Seeking past the end is not an error in Python; the read that follows is
    // the one that comes up short.
    p.pos = offset_table_offset.min(buf.len() as u64) as usize;
    p.offsets = p.read_ints(num_objects, offset_size)?;
    p.read_object(top_object, num_objects)
}

impl P<'_> {
    /// The Python's bare `fp.read(n)`: a short read is not an error, it just
    /// returns less.
    fn read_upto(&mut self, n: usize) -> &[u8] {
        let end = self.pos.saturating_add(n).min(self.buf.len());
        let start = self.pos.min(self.buf.len());
        self.pos = end;
        &self.buf[start..end]
    }

    /// `_read(n)` — a short read IS an error.
    fn read_exact(&mut self, n: usize) -> Option<&[u8]> {
        if self.pos.checked_add(n)? > self.buf.len() {
            return None;
        }
        let s = self.pos;
        self.pos += n;
        Some(&self.buf[s..s + n])
    }

    fn read_ints(&mut self, n: u64, size: usize) -> Option<Vec<u64>> {
        let total = (size as u128).checked_mul(n as u128)?;
        if total > self.buf.len() as u128 {
            // `_read` would run off the end.
            return None;
        }
        let data = self.read_exact(total as usize)?.to_vec();
        if size == 0 {
            // `_read(0)` succeeds and then `if not size: raise`.
            return None;
        }
        Some(
            data.chunks(size)
                .map(|c| {
                    let mut v: u128 = 0;
                    for &b in c {
                        v = (v << 8) | b as u128;
                    }
                    v.min(u64::MAX as u128) as u64
                })
                .collect(),
        )
    }

    fn read_refs(&mut self, n: u64) -> Option<Vec<u64>> {
        self.read_ints(n, self.ref_size)
    }

    /// `_get_size(tokenL)` — the low nibble, or a following integer whose width
    /// comes from the next byte's low TWO bits.
    fn get_size(&mut self, token_l: u8) -> Option<u64> {
        if token_l != 0xF {
            return Some(token_l as u64);
        }
        let m = *self.read_upto(1).first()? & 0x3;
        let s = 1usize << m;
        let d = self.read_exact(s)?;
        let mut v: u64 = 0;
        for &b in d {
            v = (v << 8) | b as u64;
        }
        Some(v)
    }

    fn read_object(&mut self, r: u64, num_objects: u64) -> Option<PlVal> {
        if r >= num_objects || r as usize >= self.offsets.len() {
            return None; // IndexError
        }
        if !self.path.insert(r) {
            return None; // a cycle — see the note on `path`
        }
        let out = self.read_object_inner(r, num_objects);
        self.path.remove(&r);
        out
    }

    fn read_object_inner(&mut self, r: u64, num_objects: u64) -> Option<PlVal> {
        self.pos = self.offsets[r as usize].min(self.buf.len() as u64) as usize;
        let token = *self.read_upto(1).first()?;
        let (h, l) = (token & 0xF0, token & 0x0F);
        match token {
            0x00 => return Some(PlVal::None),
            0x08 => return Some(PlVal::Bool(false)),
            0x09 => return Some(PlVal::Bool(true)),
            0x0f => return Some(PlVal::Data(Vec::new())),
            0x22 => {
                let d = self.read_exact(4)?;
                return Some(PlVal::Real(f32::from_be_bytes(d.try_into().ok()?) as f64));
            }
            0x23 => {
                let d = self.read_exact(8)?;
                return Some(PlVal::Real(f64::from_be_bytes(d.try_into().ok()?)));
            }
            0x33 => {
                let d = self.read_exact(8)?;
                let f = f64::from_be_bytes(d.try_into().ok()?);
                return apple_epoch_plus(f).map(PlVal::Date);
            }
            _ => {}
        }
        match h {
            0x10 => {
                if l > 4 {
                    return None; // a bignum — see the module doc
                }
                let want = 1usize << l;
                let d = self.read_upto(want).to_vec();
                Some(PlVal::Int(from_be(&d, l >= 3)))
            }
            0x40 => {
                let s = self.get_size(l)?;
                Some(PlVal::Data(self.read_exact(usize::try_from(s).ok()?)?.to_vec()))
            }
            0x50 => {
                let s = self.get_size(l)?;
                let d = self.read_exact(usize::try_from(s).ok()?)?;
                if !d.is_ascii() {
                    return None; // .decode('ascii') is a ValueError
                }
                Some(PlVal::Str(String::from_utf8(d.to_vec()).ok()?))
            }
            0x60 => {
                let s = self.get_size(l)?.checked_mul(2)?;
                let d = self.read_exact(usize::try_from(s).ok()?)?.to_vec();
                utf16be(&d).map(PlVal::Str)
            }
            0x80 => {
                let d = self.read_upto(1 + l as usize).to_vec();
                let mut v: u64 = 0;
                for &b in &d {
                    v = (v << 8) | b as u64;
                }
                Some(PlVal::Uid(v))
            }
            0xA0 => {
                let s = self.get_size(l)?;
                let refs = self.read_refs(s)?;
                let mut out = Vec::with_capacity(refs.len());
                for x in refs {
                    out.push(self.read_object(x, num_objects)?);
                }
                Some(PlVal::Array(out))
            }
            0xD0 => {
                let s = self.get_size(l)?;
                let keys = self.read_refs(s)?;
                let vals = self.read_refs(s)?;
                let mut out: Vec<(PlVal, PlVal)> = Vec::new();
                for (k, o) in keys.into_iter().zip(vals) {
                    let key = self.read_object(k, num_objects)?;
                    if !hashable(&key) {
                        return None; // TypeError -> InvalidFileException
                    }
                    let val = self.read_object(o, num_objects)?;
                    // Assigning an existing key keeps its position, as a dict
                    // does; only the value is replaced.
                    match out.iter_mut().find(|(ek, _)| *ek == key) {
                        Some(slot) => slot.1 = val,
                        None => out.push((key, val)),
                    }
                }
                Some(PlVal::Dict(out))
            }
            _ => None,
        }
    }
}

fn from_be(bytes: &[u8], signed: bool) -> i128 {
    let mut u: u128 = 0;
    for &b in bytes {
        u = (u << 8) | b as u128;
    }
    if !signed {
        return u as i128;
    }
    if bytes.len() >= 16 {
        return u as i128; // two's complement, already
    }
    let v = u as i128;
    if !bytes.is_empty() && bytes[0] & 0x80 != 0 {
        v - (1i128 << (8 * bytes.len()))
    } else {
        v
    }
}

/// `bytes.decode('utf-16be')` — strict, so an odd length or a lone surrogate is
/// a `UnicodeDecodeError`, which the parser reports as an invalid file.
fn utf16be(d: &[u8]) -> Option<String> {
    if d.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = d.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    String::from_utf16(&units).ok()
}

/// Which values Python can use as a dict key. A list or a dict cannot, and the
/// `TypeError` that raises is caught and re-raised as an invalid file.
fn hashable(v: &PlVal) -> bool {
    !matches!(v, PlVal::Array(_) | PlVal::Dict(_))
}

/// `datetime.datetime(2001, 1, 1) + datetime.timedelta(seconds=f)`, or `None`
/// when that leaves the 1..=9999 range Python's datetime covers.
pub fn apple_epoch_plus(f: f64) -> Option<Dt> {
    if !f.is_finite() {
        return None;
    }
    // `timedelta` rounds to whole microseconds, half to even. Splitting first
    // keeps the fraction exact for the magnitudes a plist date has.
    let whole = f.trunc();
    let frac = f - whole;
    let mut us = (frac * 1e6).round_ties_even() as i64;
    let mut secs = whole as i64;
    if us < 0 {
        us += 1_000_000;
        secs -= 1;
    }
    if us >= 1_000_000 {
        us -= 1_000_000;
        secs += 1;
    }
    // 978307200 = 2001-01-01T00:00:00Z as a Unix timestamp.
    let unix = secs.checked_add(978_307_200)?;
    let days = unix.div_euclid(86400);
    let rem = unix.rem_euclid(86400);
    let (y, mo, d) = civil_from_days(days)?;
    Some(Dt {
        y,
        mo,
        d,
        h: (rem / 3600) as u32,
        mi: (rem % 3600 / 60) as u32,
        s: (rem % 60) as u32,
        us: us as u32,
    })
}

/// Days since 1970-01-01 to a proleptic-Gregorian date (Hinnant).
fn civil_from_days(z: i64) -> Option<(i64, u32, u32)> {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    if !(1..=9999).contains(&y) {
        return None; // OverflowError
    }
    Some((y, m, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal writer, so the tests state what bytes they mean.
    fn bp(objects: Vec<Vec<u8>>, top: u64) -> Vec<u8> {
        let mut out = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(out.len() as u64);
            out.extend_from_slice(o);
        }
        let table = out.len() as u64;
        for o in &offsets {
            out.push(*o as u8);
        }
        out.extend_from_slice(&[0u8; 6]);
        out.push(1); // offset int size
        out.push(1); // ref size
        out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
        out.extend_from_slice(&top.to_be_bytes());
        out.extend_from_slice(&table.to_be_bytes());
        out
    }

    fn ascii(s: &str) -> Vec<u8> {
        let mut v = vec![0x50 | s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn a_dict_of_scalars_round_trips() {
        // {"a": 1, "b": True} — keys 1,3 and values 2,4.
        let d = vec![0xD2, 1, 3, 2, 4];
        let f = bp(vec![d, ascii("a"), vec![0x10, 1], ascii("b"), vec![0x09]], 0);
        assert_eq!(
            load(&f).unwrap(),
            PlVal::Dict(vec![
                (PlVal::Str("a".into()), PlVal::Int(1)),
                (PlVal::Str("b".into()), PlVal::Bool(true)),
            ])
        );
    }

    #[test]
    fn eight_byte_integers_are_signed_and_smaller_ones_are_not() {
        let neg = bp(vec![vec![0x13, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]], 0);
        assert_eq!(load(&neg).unwrap(), PlVal::Int(-1));
        let big = bp(vec![vec![0x11, 0xff, 0xff]], 0);
        assert_eq!(load(&big).unwrap(), PlVal::Int(65535));
    }

    #[test]
    fn the_fill_byte_is_the_empty_byte_string() {
        assert_eq!(load(&bp(vec![vec![0x0f]], 0)).unwrap(), PlVal::Data(Vec::new()));
    }

    #[test]
    fn a_uid_is_its_own_type_not_an_integer() {
        assert_eq!(load(&bp(vec![vec![0x80, 7]], 0)).unwrap(), PlVal::Uid(7));
    }

    #[test]
    fn an_integer_that_runs_off_the_end_is_whatever_was_there() {
        // `int.from_bytes` has no length check, so an integer marker near EOF
        // reads fewer bytes and yields a smaller number — including zero from
        // no bytes at all. Not an error, in either implementation.
        assert_eq!(from_be(&[], true), 0);
        assert_eq!(from_be(&[0x00, 0x00, 0x05], true), 5);
        assert_eq!(from_be(&[0xff], true), -1);
        assert_eq!(from_be(&[0xff], false), 255);
        assert_eq!(from_be(&[0xff; 16], true), -1);
    }

    #[test]
    fn a_string_claiming_more_than_the_file_holds_is_invalid() {
        // `0x5f` says "the length follows"; `0x11 ff ff` says 65535 characters.
        let f = bp(vec![vec![0x5f, 0x11, 0xff, 0xff]], 0);
        assert_eq!(load(&f), Err(PlErr::Invalid));
    }

    #[test]
    fn an_unknown_marker_and_a_missing_header_are_both_invalid() {
        assert_eq!(load(&bp(vec![vec![0xB0]], 0)), Err(PlErr::Invalid));
        assert_eq!(load(b"not a plist at all"), Err(PlErr::Invalid));
        assert_eq!(load(b""), Err(PlErr::Invalid));
    }

    #[test]
    fn an_xml_plist_is_refused_by_name() {
        assert_eq!(load(b"<?xml version=\"1.0\"?><plist/>"), Err(PlErr::NotBinary));
        assert_eq!(load(b"\xef\xbb\xbf<?xml version=\"1.0\"?>"), Err(PlErr::NotBinary));
    }

    #[test]
    fn a_non_hashable_key_is_an_invalid_file() {
        // {[]: 1} — a list cannot be a dict key.
        let f = bp(vec![vec![0xD1, 1, 2], vec![0xA0], vec![0x10, 1]], 0);
        assert_eq!(load(&f), Err(PlErr::Invalid));
    }

    #[test]
    fn a_cycle_does_not_recurse_forever() {
        let f = bp(vec![vec![0xA1, 0]], 0); // an array holding itself
        assert_eq!(load(&f), Err(PlErr::Invalid));
    }

    #[test]
    fn dates_land_on_the_apple_epoch() {
        assert_eq!(apple_epoch_plus(0.0).unwrap().to_py_str(), "2001-01-01 00:00:00");
        assert_eq!(apple_epoch_plus(86400.0).unwrap().to_py_str(), "2001-01-02 00:00:00");
        assert_eq!(apple_epoch_plus(-1.0).unwrap().to_py_str(), "2000-12-31 23:59:59");
        assert_eq!(apple_epoch_plus(0.5).unwrap().to_py_str(), "2001-01-01 00:00:00.500000");
        assert_eq!(apple_epoch_plus(1e18), None);
        assert_eq!(apple_epoch_plus(f64::NAN), None);
        assert_eq!(
            apple_epoch_plus(0.0).unwrap().to_py_repr(),
            "datetime.datetime(2001, 1, 1, 0, 0)"
        );
    }
}
