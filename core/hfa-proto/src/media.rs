//! UDP media header.
//!
//! Byte layout (all integers big-endian), [`crate::MEDIA_HEADER_LEN`] = 16 bytes:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 2 | magic `"HF"` ([`crate::MAGIC`]) |
//! | 2 | 1 | version ([`crate::PROTOCOL_VERSION`]) |
//! | 3 | 1 | flags ([`FLAG_FEC`], [`FLAG_DTX`], [`FLAG_RESET`]) |
//! | 4 | 4 | `stream_id` |
//! | 8 | 4 | `seq` |
//! | 12 | 4 | `timestamp` (48 kHz sample clock) |

use crate::{ProtoError, Result, MAGIC, MEDIA_HEADER_LEN, PROTOCOL_VERSION};

/// The payload carries Opus in-band FEC data for the previous frame.
pub const FLAG_FEC: u8 = 0x01;
/// Silence keep-alive (discontinuous transmission). The payload is empty.
pub const FLAG_DTX: u8 = 0x02;
/// Timestamp / codec-state discontinuity (e.g. the sender restarted its capture or encoder):
/// the hub resets the jitter buffer and decoder of this stream.
///
/// It **never** resets `seq`. `seq` is part of the AEAD nonce, so it keeps increasing for the
/// lifetime of the stream's [`crate::MediaKey`], and the opener's replay window
/// ([`crate::ReplayWindow`]) is *not* reset by this flag (a replayed `FLAG_RESET` datagram is
/// rejected like any other replay). A sender that needs to start over at `seq = 0` announces
/// a new stream: a new `StreamStart` with a fresh `stream_id` and a fresh key.
pub const FLAG_RESET: u8 = 0x04;

/// The plaintext header in front of every media datagram. It is authenticated as AEAD
/// associated data but not encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MediaHeader {
    /// Bit set of `FLAG_*` values.
    pub flags: u8,
    /// Random per-stream identifier chosen by the sender.
    pub stream_id: u32,
    /// Packet counter, +1 per datagram, starting at 0. It is part of the AEAD nonce, so it is
    /// strictly increasing for the lifetime of a key: it never resets (not even with
    /// [`FLAG_RESET`]) and never wraps (after `u32::MAX` the sender must start a new stream).
    pub seq: u32,
    /// Media timestamp of the first sample in the packet, in 48 kHz samples.
    pub timestamp: u32,
}

impl MediaHeader {
    /// Returns `true` if every bit of `flag` is set in [`MediaHeader::flags`].
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flags & flag == flag
    }

    /// Serializes the header (magic, version, flags, stream_id, seq, timestamp).
    pub fn encode(&self) -> [u8; MEDIA_HEADER_LEN] {
        let mut out = [0u8; MEDIA_HEADER_LEN];
        out[0..2].copy_from_slice(&MAGIC);
        out[2] = PROTOCOL_VERSION;
        out[3] = self.flags;
        out[4..8].copy_from_slice(&self.stream_id.to_be_bytes());
        out[8..12].copy_from_slice(&self.seq.to_be_bytes());
        out[12..16].copy_from_slice(&self.timestamp.to_be_bytes());
        out
    }

    /// Parses a header from the start of `buf` and returns it together with the rest of the
    /// buffer (the payload).
    ///
    /// # Errors
    /// [`crate::ProtoError::Truncated`], [`crate::ProtoError::BadMagic`] or
    /// [`crate::ProtoError::UnsupportedVersion`].
    pub fn decode(buf: &[u8]) -> Result<(MediaHeader, &[u8])> {
        let (head, payload) = buf
            .split_first_chunk::<MEDIA_HEADER_LEN>()
            .ok_or(ProtoError::Truncated {
                needed: MEDIA_HEADER_LEN,
                got: buf.len(),
            })?;
        if head[0..2] != MAGIC {
            return Err(ProtoError::BadMagic);
        }
        if head[2] != PROTOCOL_VERSION {
            return Err(ProtoError::UnsupportedVersion(head[2]));
        }
        let header = MediaHeader {
            flags: head[3],
            stream_id: be_u32(head, 4),
            seq: be_u32(head, 8),
            timestamp: be_u32(head, 12),
        };
        Ok((header, payload))
    }
}

/// Reads a big-endian `u32` at `at` from the fixed-size header.
fn be_u32(head: &[u8; MEDIA_HEADER_LEN], at: usize) -> u32 {
    u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn known_layout() {
        let h = MediaHeader {
            flags: FLAG_FEC | FLAG_RESET,
            stream_id: 0x0102_0304,
            seq: 0xA0B0_C0D0,
            timestamp: 0xDEAD_BEEF,
        };
        assert_eq!(
            h.encode(),
            [
                b'H', b'F', 0, 0x05, 0x01, 0x02, 0x03, 0x04, 0xA0, 0xB0, 0xC0, 0xD0, 0xDE, 0xAD,
                0xBE, 0xEF
            ]
        );
        assert!(h.has_flag(FLAG_FEC) && h.has_flag(FLAG_RESET) && !h.has_flag(FLAG_DTX));
        assert!(h.has_flag(FLAG_FEC | FLAG_RESET));
        assert!(!h.has_flag(FLAG_FEC | FLAG_DTX));
    }

    #[test]
    fn decode_returns_the_payload() {
        let h = MediaHeader {
            flags: FLAG_DTX,
            stream_id: 9,
            seq: 1,
            timestamp: 480,
        };
        let mut buf = h.encode().to_vec();
        buf.extend_from_slice(b"payload");
        let (got, rest) = MediaHeader::decode(&buf).unwrap();
        assert_eq!(got, h);
        assert_eq!(rest, b"payload");
        let bare = h.encode();
        let (_, empty) = MediaHeader::decode(&bare).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn rejects_short_buffers() {
        let buf = MediaHeader::default().encode();
        for len in 0..MEDIA_HEADER_LEN {
            assert_eq!(
                MediaHeader::decode(&buf[..len]),
                Err(ProtoError::Truncated {
                    needed: MEDIA_HEADER_LEN,
                    got: len
                })
            );
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = MediaHeader::default().encode();
        buf[0] = b'X';
        assert_eq!(MediaHeader::decode(&buf), Err(ProtoError::BadMagic));
        let mut buf = MediaHeader::default().encode();
        buf[1] = b'f';
        assert_eq!(MediaHeader::decode(&buf), Err(ProtoError::BadMagic));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut buf = MediaHeader::default().encode();
        buf[2] = PROTOCOL_VERSION + 1;
        assert_eq!(
            MediaHeader::decode(&buf),
            Err(ProtoError::UnsupportedVersion(PROTOCOL_VERSION + 1))
        );
    }

    proptest! {
        #[test]
        fn roundtrip(flags: u8, stream_id: u32, seq: u32, timestamp: u32,
                     payload in proptest::collection::vec(any::<u8>(), 0..64)) {
            let h = MediaHeader { flags, stream_id, seq, timestamp };
            let mut buf = h.encode().to_vec();
            buf.extend_from_slice(&payload);
            let (got, rest) = MediaHeader::decode(&buf).unwrap();
            prop_assert_eq!(got, h);
            prop_assert_eq!(rest, &payload[..]);
        }

        /// Decoding arbitrary bytes never panics, and succeeds exactly when the prefix is valid.
        #[test]
        fn decode_never_panics(buf in proptest::collection::vec(any::<u8>(), 0..40)) {
            let valid = buf.len() >= MEDIA_HEADER_LEN
                && buf[0..2] == MAGIC
                && buf[2] == PROTOCOL_VERSION;
            match MediaHeader::decode(&buf) {
                Ok((h, rest)) => {
                    prop_assert!(valid);
                    prop_assert_eq!(&h.encode()[..], &buf[..MEDIA_HEADER_LEN]);
                    prop_assert_eq!(rest.len(), buf.len() - MEDIA_HEADER_LEN);
                }
                Err(_) => prop_assert!(!valid),
            }
        }
    }
}
