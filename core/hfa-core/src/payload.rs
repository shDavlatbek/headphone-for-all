//! The media payload container (the plaintext inside a sealed media datagram).
//!
//! # Wire format (protocol version 0)
//!
//! A datagram with [`hfa_proto::FLAG_DTX`] carries an **empty** payload (a silence
//! keep-alive). Every other media payload is a tiny container of Opus packets:
//!
//! ```text
//! count: u8                        number of entries, >= 1
//! count × { len: u16 BE, data: [u8; len] }   Opus packets, len >= 1
//! ```
//!
//! - Entry 0 is the Opus packet of the frame this datagram's `seq` stands for.
//! - Entry 1 (present only when the header has [`hfa_proto::FLAG_FEC`]) is a redundant copy
//!   of the **previous** frame (`seq − 1`). The hub decodes it when packet `seq − 1` was lost
//!   (full recovery, unlike Opus in-band FEC, which libopus does not produce for CELT/music
//!   at our bitrates; see `docs/CONTRACTS.md` §4.2).
//! - Further entries are reserved: a receiver ignores them (a newer sender may add them), but
//!   the whole container must still be well formed.
//!
//! The container adds 3 bytes (5 with redundancy) to the Opus data and must fit
//! [`crate::media::MAX_MEDIA_PAYLOAD`]; a sender that cannot fit the redundant copy sends the
//! packet without it (and without `FLAG_FEC`).

use crate::media::MAX_MEDIA_PAYLOAD;

/// Size of the entry count.
const COUNT_LEN: usize = 1;
/// Size of each entry's length prefix.
const LEN_PREFIX: usize = 2;

/// A parsed media payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaPayload<'a> {
    /// The Opus packet of this datagram's frame (entry 0).
    pub primary: &'a [u8],
    /// A redundant copy of the previous frame's Opus packet (entry 1), if present.
    pub redundant: Option<&'a [u8]>,
}

/// Bytes a container with these entries needs.
pub fn encoded_len(primary: &[u8], redundant: Option<&[u8]>) -> usize {
    COUNT_LEN + LEN_PREFIX + primary.len() + redundant.map_or(0, |r| LEN_PREFIX + r.len())
}

/// `true` if a container with these entries fits one media datagram.
pub fn fits(primary: &[u8], redundant: Option<&[u8]>) -> bool {
    encoded_len(primary, redundant) <= MAX_MEDIA_PAYLOAD
}

/// Writes a container into `out` (cleared first). Returns `false` and leaves `out` empty if
/// an entry is empty, longer than `u16::MAX`, or the container would not fit
/// [`MAX_MEDIA_PAYLOAD`]. Does not allocate when `out` has enough capacity.
pub fn write(primary: &[u8], redundant: Option<&[u8]>, out: &mut Vec<u8>) -> bool {
    out.clear();
    let entries_ok = |p: &[u8]| !p.is_empty() && u16::try_from(p.len()).is_ok();
    if !entries_ok(primary)
        || redundant.is_some_and(|r| !entries_ok(r))
        || !fits(primary, redundant)
    {
        return false;
    }
    let count: u8 = if redundant.is_some() { 2 } else { 1 };
    out.push(count);
    for entry in std::iter::once(primary).chain(redundant) {
        // Checked above: every entry length fits a u16.
        let len = u16::try_from(entry.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(entry);
    }
    true
}

/// Parses a container. Returns `None` for an empty payload (a DTX keep-alive) or a malformed
/// one (zero entries, a zero-length or truncated entry, trailing bytes).
pub fn parse(payload: &[u8]) -> Option<MediaPayload<'_>> {
    let (&count, mut rest) = payload.split_first()?;
    if count == 0 {
        return None;
    }
    let mut primary = None;
    let mut redundant = None;
    for i in 0..count {
        let (prefix, tail) = rest.split_first_chunk::<LEN_PREFIX>()?;
        let len = usize::from(u16::from_be_bytes(*prefix));
        if len == 0 || tail.len() < len {
            return None;
        }
        let (entry, tail) = tail.split_at(len);
        match i {
            0 => primary = Some(entry),
            1 => redundant = Some(entry),
            _ => {} // reserved for future use
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return None;
    }
    Some(MediaPayload {
        primary: primary?,
        redundant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_one_and_two_entries() {
        let mut out = Vec::new();
        assert!(write(b"abc", None, &mut out));
        assert_eq!(out, [1, 0, 3, b'a', b'b', b'c']);
        assert_eq!(
            parse(&out),
            Some(MediaPayload {
                primary: b"abc",
                redundant: None
            })
        );
        assert!(write(b"abc", Some(b"xy"), &mut out));
        assert_eq!(out, [2, 0, 3, b'a', b'b', b'c', 0, 2, b'x', b'y']);
        assert_eq!(encoded_len(b"abc", Some(b"xy")), out.len());
        let p = parse(&out).expect("valid");
        assert_eq!(p.primary, b"abc");
        assert_eq!(p.redundant, Some(&b"xy"[..]));
    }

    #[test]
    fn rejects_malformed_containers() {
        assert_eq!(parse(&[]), None, "empty = DTX keep-alive");
        assert_eq!(parse(&[0]), None, "zero entries");
        assert_eq!(parse(&[1, 0]), None, "truncated prefix");
        assert_eq!(parse(&[1, 0, 3, 1, 2]), None, "truncated entry");
        assert_eq!(parse(&[1, 0, 0]), None, "zero-length entry");
        assert_eq!(parse(&[1, 0, 1, 7, 9]), None, "trailing byte");
        assert_eq!(parse(&[2, 0, 1, 7]), None, "missing second entry");
    }

    #[test]
    fn ignores_reserved_entries() {
        let data = [3, 0, 1, 1, 0, 1, 2, 0, 2, 3, 3];
        let p = parse(&data).expect("valid");
        assert_eq!(p.primary, [1]);
        assert_eq!(p.redundant, Some(&[2u8][..]));
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let mut out = vec![9];
        assert!(!write(&[], None, &mut out));
        assert!(out.is_empty());
        let big = vec![1u8; MAX_MEDIA_PAYLOAD];
        assert!(!write(&big, None, &mut out));
        let half = vec![1u8; MAX_MEDIA_PAYLOAD / 2];
        assert!(!fits(&half, Some(&half)));
        assert!(!write(&half, Some(&half), &mut out));
        let max_single = vec![1u8; MAX_MEDIA_PAYLOAD - 3];
        assert!(write(&max_single, None, &mut out));
        assert_eq!(out.len(), MAX_MEDIA_PAYLOAD);
    }

    #[test]
    fn write_reuses_capacity() {
        let mut out = Vec::with_capacity(MAX_MEDIA_PAYLOAD);
        let ptr = out.as_ptr();
        for _ in 0..10 {
            assert!(write(&[5; 300], Some(&[6; 300]), &mut out));
        }
        assert_eq!(out.as_ptr(), ptr);
    }
}
