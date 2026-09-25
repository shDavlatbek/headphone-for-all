//! Media encryption: ChaCha20-Poly1305 over each datagram.
//!
//! - Nonce (12 bytes) = `stream_id` BE ‖ `seq` BE ‖ `0u32`.
//! - AAD = the 16-byte encoded [`MediaHeader`].
//! - Output datagram = `header ‖ ciphertext ‖ tag` (tag = [`crate::AEAD_TAG_LEN`] bytes).
//!
//! Every stream uses a fresh [`MediaKey`] (sent inside the Noise-protected `StreamStart`).
//! `seq` is strictly increasing for the lifetime of a key: it never resets (not even with
//! [`crate::FLAG_RESET`]) and never wraps, so a `(key, nonce)` pair is never reused. A stream
//! that must restart at `seq = 0` gets a new `stream_id` *and* a new key.
//!
//! Replay protection: [`MediaOpener`] keeps a [`ReplayWindow`] and rejects datagrams whose
//! `seq` was already accepted or is older than [`crate::replay::REPLAY_WINDOW`] packets.

use std::fmt;

use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, Tag};
use rand::Rng;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::media::MediaHeader;
use crate::replay::ReplayWindow;
use crate::{ProtoError, Result, AEAD_TAG_LEN, MAX_DATAGRAM, MEDIA_HEADER_LEN};

/// Smallest valid sealed datagram: a header and a tag around an empty payload.
const MIN_SEALED_LEN: usize = MEDIA_HEADER_LEN + AEAD_TAG_LEN;

/// Builds the 96-bit AEAD nonce `stream_id BE ‖ seq BE ‖ 0u32`.
pub(crate) fn nonce_bytes(stream_id: u32, seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0..4].copy_from_slice(&stream_id.to_be_bytes());
    n[4..8].copy_from_slice(&seq.to_be_bytes());
    n
}

/// Builds the ChaCha20-Poly1305 instance for `key` (cheap: it only copies the key). The
/// instance is wiped on drop (`chacha20poly1305`'s `zeroize` feature) and lives only for one
/// seal/open call, so no long-lived copy of the key exists outside [`MediaKey`].
fn cipher(key: &MediaKey) -> ChaCha20Poly1305 {
    let key: &Key = (&key.0).into();
    ChaCha20Poly1305::new(key)
}

/// A 256-bit media key for one stream. The bytes are wiped on drop; `Debug` never prints them.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct MediaKey([u8; 32]);

impl MediaKey {
    /// Generates a new random key from the OS CSPRNG.
    ///
    /// Uses `rand`'s thread-local CSPRNG (ChaCha12, seeded and periodically reseeded from the
    /// OS entropy source).
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let key = Self(bytes);
        bytes.zeroize();
        key
    }

    /// Wraps existing key bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Builds a key from a slice (e.g. the `media_key` field of `StreamStart`).
    ///
    /// # Errors
    /// [`crate::ProtoError::InvalidKey`] if `bytes` is not exactly 32 bytes long.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            crate::ProtoError::InvalidKey(format!("expected 32 bytes, got {}", bytes.len()))
        })?;
        Ok(Self(arr))
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MediaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaKey(<redacted>)")
    }
}

/// Encrypts datagrams of one stream (sender side).
pub struct MediaSealer {
    key: MediaKey,
    stream_id: u32,
}

impl MediaSealer {
    /// Creates a sealer for `stream_id` using `key`.
    pub fn new(key: &MediaKey, stream_id: u32) -> Self {
        Self {
            key: key.clone(),
            stream_id,
        }
    }

    /// The stream this sealer belongs to.
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    /// Seals `payload` and writes `header ‖ ciphertext ‖ tag` into `out` (cleared first).
    ///
    /// # Errors
    /// [`crate::ProtoError::StreamMismatch`] if `header.stream_id` differs from this sealer's,
    /// [`crate::ProtoError::FrameTooLarge`] if the datagram would exceed [`crate::MAX_DATAGRAM`],
    /// [`crate::ProtoError::Crypto`] on AEAD failure.
    ///
    /// Does not allocate when `out` already has room for the datagram (reuse one buffer per
    /// stream; at most [`crate::MAX_DATAGRAM`] bytes), so it can run on the encoder thread.
    pub fn seal(&self, header: &MediaHeader, payload: &[u8], out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        if header.stream_id != self.stream_id {
            return Err(ProtoError::StreamMismatch {
                expected: self.stream_id,
                got: header.stream_id,
            });
        }
        let len = MIN_SEALED_LEN.saturating_add(payload.len());
        if len > MAX_DATAGRAM {
            return Err(ProtoError::FrameTooLarge {
                len,
                max: MAX_DATAGRAM,
            });
        }
        let aad = header.encode();
        out.reserve(len);
        out.extend_from_slice(&aad);
        out.extend_from_slice(payload);
        let nonce = Nonce::from(nonce_bytes(header.stream_id, header.seq));
        let tag = cipher(&self.key)
            .encrypt_inout_detached(&nonce, &aad, out[MEDIA_HEADER_LEN..].as_mut().into())
            .map_err(|_| {
                out.clear();
                ProtoError::Crypto
            })?;
        out.extend_from_slice(tag.as_slice());
        Ok(())
    }
}

/// Decrypts and authenticates datagrams of one stream (hub side), with replay protection.
pub struct MediaOpener {
    key: MediaKey,
    stream_id: u32,
    replay: ReplayWindow,
}

impl MediaOpener {
    /// Creates an opener for `stream_id` using `key`.
    pub fn new(key: &MediaKey, stream_id: u32) -> Self {
        Self {
            key: key.clone(),
            stream_id,
            replay: ReplayWindow::new(),
        }
    }

    /// The stream this opener belongs to.
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    /// Parses the header, checks the stream id, rejects replays, authenticates and decrypts
    /// the payload.
    ///
    /// Order: decode header → stream id → [`ReplayWindow::check`] (cheap, before decryption)
    /// → AEAD open → [`ReplayWindow::accept`] (only after authentication succeeded, so forged
    /// datagrams can never advance or poison the window). `FLAG_RESET` does not reset the
    /// window.
    ///
    /// # Errors
    /// Header errors from [`MediaHeader::decode`], [`crate::ProtoError::StreamMismatch`],
    /// [`crate::ProtoError::Replay`] for a duplicate or too old `seq`, or
    /// [`crate::ProtoError::Crypto`] if authentication fails.
    ///
    /// Datagrams shorter than header + tag fail with [`crate::ProtoError::Truncated`], longer
    /// than [`crate::MAX_DATAGRAM`] with [`crate::ProtoError::FrameTooLarge`].
    pub fn open(&mut self, datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)> {
        let (header, sealed) = MediaHeader::decode(datagram)?;
        if header.stream_id != self.stream_id {
            return Err(ProtoError::StreamMismatch {
                expected: self.stream_id,
                got: header.stream_id,
            });
        }
        if datagram.len() < MIN_SEALED_LEN {
            return Err(ProtoError::Truncated {
                needed: MIN_SEALED_LEN,
                got: datagram.len(),
            });
        }
        if datagram.len() > MAX_DATAGRAM {
            return Err(ProtoError::FrameTooLarge {
                len: datagram.len(),
                max: MAX_DATAGRAM,
            });
        }
        if !self.replay.check(header.seq) {
            return Err(ProtoError::Replay { seq: header.seq });
        }
        let (ciphertext, tag) = sealed.split_at(sealed.len() - AEAD_TAG_LEN);
        let tag = Tag::try_from(tag).map_err(|_| ProtoError::Crypto)?;
        let aad = &datagram[..MEDIA_HEADER_LEN];
        let nonce = Nonce::from(nonce_bytes(header.stream_id, header.seq));
        let mut plaintext = ciphertext.to_vec();
        cipher(&self.key)
            .decrypt_inout_detached(&nonce, aad, plaintext.as_mut_slice().into(), &tag)
            .map_err(|_| ProtoError::Crypto)?;
        if !self.replay.accept(header.seq) {
            // Unreachable in practice (`check` passed and `&mut self` excludes races), but
            // never hand out a packet the window did not record.
            return Err(ProtoError::Replay { seq: header.seq });
        }
        Ok((header, plaintext))
    }

    /// Highest `seq` accepted so far, if any (for statistics).
    pub fn highest_seq(&self) -> Option<u32> {
        self.replay.highest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FLAG_DTX, FLAG_RESET};
    use proptest::prelude::*;

    fn header(stream_id: u32, seq: u32) -> MediaHeader {
        MediaHeader {
            flags: 0,
            stream_id,
            seq,
            timestamp: seq.wrapping_mul(480),
        }
    }

    fn sealed(key: &MediaKey, h: &MediaHeader, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        MediaSealer::new(key, h.stream_id)
            .seal(h, payload, &mut out)
            .unwrap();
        out
    }

    #[test]
    fn nonce_layout() {
        assert_eq!(
            nonce_bytes(0x0102_0304, 0x0A0B_0C0D),
            [1, 2, 3, 4, 0x0A, 0x0B, 0x0C, 0x0D, 0, 0, 0, 0]
        );
    }

    /// Pins the exact construction (nonce, AAD, output layout) against an independent
    /// ChaCha20-Poly1305 computation, so a wire change cannot slip in unnoticed.
    #[test]
    fn output_is_header_ciphertext_tag() {
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::aead::Payload;
        let key = MediaKey::from_bytes([7; 32]);
        let h = MediaHeader {
            flags: FLAG_RESET,
            stream_id: 0xABCD_0001,
            seq: 42,
            timestamp: 20_160,
        };
        let out = sealed(&key, &h, b"opus frame");
        assert_eq!(out.len(), MEDIA_HEADER_LEN + 10 + AEAD_TAG_LEN);
        assert_eq!(out[..MEDIA_HEADER_LEN], h.encode());

        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&0xABCD_0001u32.to_be_bytes());
        nonce[4..8].copy_from_slice(&42u32.to_be_bytes());
        let reference = ChaCha20Poly1305::new(&Key::from([7; 32]))
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: b"opus frame",
                    aad: &h.encode(),
                },
            )
            .unwrap();
        assert_eq!(out[MEDIA_HEADER_LEN..], reference[..]);
    }

    #[test]
    fn roundtrip_and_empty_payload() {
        let key = MediaKey::generate();
        let mut opener = MediaOpener::new(&key, 5);
        let h = header(5, 0);
        let (got, payload) = opener.open(&sealed(&key, &h, b"hello")).unwrap();
        assert_eq!((got, payload.as_slice()), (h, &b"hello"[..]));

        let dtx = MediaHeader {
            flags: FLAG_DTX,
            ..header(5, 1)
        };
        let (got, payload) = opener.open(&sealed(&key, &dtx, &[])).unwrap();
        assert_eq!(got, dtx);
        assert!(payload.is_empty());
        assert_eq!(opener.highest_seq(), Some(1));
    }

    #[test]
    fn seal_clears_out_and_reuses_it() {
        let key = MediaKey::generate();
        let sealer = MediaSealer::new(&key, 1);
        let mut out = vec![0xFF; 3];
        sealer.seal(&header(1, 0), b"abc", &mut out).unwrap();
        assert_eq!(out.len(), MEDIA_HEADER_LEN + 3 + AEAD_TAG_LEN);
        assert_eq!(out[..MEDIA_HEADER_LEN], header(1, 0).encode());
        let cap = out.capacity();
        let ptr = out.as_ptr();
        sealer.seal(&header(1, 1), b"xyz", &mut out).unwrap();
        assert_eq!(
            (out.capacity(), out.as_ptr()),
            (cap, ptr),
            "no reallocation"
        );
    }

    #[test]
    fn seal_rejects_foreign_stream_and_oversize() {
        let key = MediaKey::generate();
        let sealer = MediaSealer::new(&key, 1);
        let mut out = vec![1, 2, 3];
        assert_eq!(
            sealer.seal(&header(2, 0), b"x", &mut out),
            Err(ProtoError::StreamMismatch {
                expected: 1,
                got: 2
            })
        );
        assert!(out.is_empty(), "out is cleared even on error");

        let max_payload = MAX_DATAGRAM - MIN_SEALED_LEN;
        sealer
            .seal(&header(1, 0), &vec![0; max_payload], &mut out)
            .unwrap();
        assert_eq!(out.len(), MAX_DATAGRAM);
        assert_eq!(
            sealer.seal(&header(1, 1), &vec![0; max_payload + 1], &mut out),
            Err(ProtoError::FrameTooLarge {
                len: MAX_DATAGRAM + 1,
                max: MAX_DATAGRAM
            })
        );
    }

    #[test]
    fn wrong_key_is_rejected_without_moving_the_window() {
        let dg = sealed(&MediaKey::generate(), &header(3, 10), b"secret");
        let mut opener = MediaOpener::new(&MediaKey::generate(), 3);
        assert_eq!(opener.open(&dg), Err(ProtoError::Crypto));
        assert_eq!(
            opener.highest_seq(),
            None,
            "forgery must not advance the window"
        );
    }

    #[test]
    fn wrong_stream_is_rejected() {
        let key = MediaKey::generate();
        let dg = sealed(&key, &header(3, 0), b"x");
        let mut opener = MediaOpener::new(&key, 4);
        assert_eq!(
            opener.open(&dg),
            Err(ProtoError::StreamMismatch {
                expected: 4,
                got: 3
            })
        );
    }

    /// Rewriting the header's stream id (to that of the opener) breaks authentication: the
    /// nonce and the AAD both change.
    #[test]
    fn relabelled_stream_id_fails_authentication() {
        let key = MediaKey::generate();
        let mut dg = sealed(&key, &header(3, 0), b"x");
        dg[4..8].copy_from_slice(&4u32.to_be_bytes());
        assert_eq!(MediaOpener::new(&key, 4).open(&dg), Err(ProtoError::Crypto));
    }

    #[test]
    fn every_tampered_byte_is_detected() {
        let key = MediaKey::generate();
        let h = MediaHeader {
            flags: FLAG_DTX,
            ..header(9, 77)
        };
        let dg = sealed(&key, &h, b"some opus payload");
        for i in 0..dg.len() {
            for bit in [0x01u8, 0x80] {
                let mut bad = dg.clone();
                bad[i] ^= bit;
                let mut opener = MediaOpener::new(&key, 9);
                let err = opener.open(&bad).unwrap_err();
                match i {
                    0 | 1 => assert_eq!(err, ProtoError::BadMagic),
                    2 => assert!(matches!(err, ProtoError::UnsupportedVersion(_))),
                    4..=7 => assert!(matches!(err, ProtoError::StreamMismatch { .. })),
                    _ => assert_eq!(err, ProtoError::Crypto, "byte {i} bit {bit:#x}"),
                }
                assert_eq!(opener.highest_seq(), None);
            }
        }
        // The untouched datagram still opens.
        assert!(MediaOpener::new(&key, 9).open(&dg).is_ok());
    }

    #[test]
    fn truncation_is_rejected() {
        let key = MediaKey::generate();
        let dg = sealed(&key, &header(1, 0), b"payload");
        for len in 0..dg.len() {
            let err = MediaOpener::new(&key, 1).open(&dg[..len]).unwrap_err();
            if len < MEDIA_HEADER_LEN {
                assert!(
                    matches!(err, ProtoError::Truncated { .. }),
                    "{len}: {err:?}"
                );
            } else if len < MIN_SEALED_LEN {
                assert_eq!(
                    err,
                    ProtoError::Truncated {
                        needed: MIN_SEALED_LEN,
                        got: len
                    }
                );
            } else {
                assert_eq!(err, ProtoError::Crypto, "{len}");
            }
        }
    }

    #[test]
    fn oversize_datagram_is_rejected() {
        let key = MediaKey::generate();
        let mut dg = sealed(&key, &header(1, 0), &[0; MAX_DATAGRAM - MIN_SEALED_LEN]);
        dg.push(0);
        assert_eq!(
            MediaOpener::new(&key, 1).open(&dg),
            Err(ProtoError::FrameTooLarge {
                len: MAX_DATAGRAM + 1,
                max: MAX_DATAGRAM
            })
        );
    }

    #[test]
    fn replays_are_rejected_even_with_reset_flag() {
        let key = MediaKey::generate();
        let mut opener = MediaOpener::new(&key, 1);
        let reset = MediaHeader {
            flags: FLAG_RESET,
            ..header(1, 0)
        };
        let first = sealed(&key, &reset, b"a");
        assert!(opener.open(&first).is_ok());
        assert_eq!(opener.open(&first), Err(ProtoError::Replay { seq: 0 }));
        let late = sealed(&key, &header(1, 5), b"b");
        let later = sealed(&key, &header(1, 200), b"c");
        assert!(opener.open(&later).is_ok());
        assert_eq!(opener.open(&late), Err(ProtoError::Replay { seq: 5 }));
    }

    #[test]
    fn out_of_order_inside_window_is_accepted() {
        let key = MediaKey::generate();
        let mut opener = MediaOpener::new(&key, 1);
        for seq in [3, 1, 2, 0, 10, 4] {
            let (h, _) = opener.open(&sealed(&key, &header(1, seq), b"x")).unwrap();
            assert_eq!(h.seq, seq);
        }
        assert_eq!(opener.highest_seq(), Some(10));
    }

    #[test]
    fn media_key_slices_and_debug() {
        let key = MediaKey::from_bytes([0xAB; 32]);
        assert_eq!(MediaKey::from_slice(&[0xAB; 32]).unwrap(), key);
        assert!(matches!(
            MediaKey::from_slice(&[0; 31]),
            Err(ProtoError::InvalidKey(_))
        ));
        assert!(MediaKey::from_slice(&[0; 33]).is_err());
        let text = format!("{key:?}");
        assert_eq!(text, "MediaKey(<redacted>)");
        assert_ne!(MediaKey::generate(), MediaKey::generate());
    }

    #[test]
    fn media_key_zeroize_wipes_bytes() {
        let mut key = MediaKey::from_bytes([0x5A; 32]);
        key.zeroize();
        assert_eq!(key.as_bytes(), &[0u8; 32]);
    }

    proptest! {
        #[test]
        fn seal_open_roundtrip(stream_id: u32, seq: u32, flags: u8, timestamp: u32,
                               key: [u8; 32],
                               payload in proptest::collection::vec(any::<u8>(), 0..=MAX_DATAGRAM - MIN_SEALED_LEN)) {
            let key = MediaKey::from_bytes(key);
            let h = MediaHeader { flags, stream_id, seq, timestamp };
            let dg = sealed(&key, &h, &payload);
            prop_assert_eq!(dg.len(), payload.len() + MIN_SEALED_LEN);
            let (got, plain) = MediaOpener::new(&key, stream_id).open(&dg).unwrap();
            prop_assert_eq!(got, h);
            prop_assert_eq!(plain, payload);
        }

        /// Distinct sequence numbers give distinct nonces and therefore unrelated keystreams:
        /// the same payload never encrypts to the same ciphertext.
        #[test]
        fn distinct_seq_distinct_nonce_and_ciphertext(stream_id: u32, a: u32, b: u32) {
            prop_assume!(a != b);
            prop_assert_ne!(nonce_bytes(stream_id, a), nonce_bytes(stream_id, b));
            let key = MediaKey::from_bytes([1; 32]);
            let payload = [0u8; 32];
            let da = sealed(&key, &header(stream_id, a), &payload);
            let db = sealed(&key, &header(stream_id, b), &payload);
            prop_assert_ne!(&da[MEDIA_HEADER_LEN..], &db[MEDIA_HEADER_LEN..]);
        }

        /// Distinct streams never share a nonce, whatever their sequence numbers.
        #[test]
        fn distinct_streams_distinct_nonce(s1: u32, s2: u32, seq1: u32, seq2: u32) {
            prop_assume!(s1 != s2);
            prop_assert_ne!(nonce_bytes(s1, seq1), nonce_bytes(s2, seq2));
        }

        /// Opening arbitrary bytes never panics and never succeeds without the key.
        #[test]
        fn open_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..1500), valid_prefix: bool) {
            let mut bytes = bytes;
            if valid_prefix && bytes.len() >= MEDIA_HEADER_LEN {
                bytes[..MEDIA_HEADER_LEN].copy_from_slice(&header(1, 0).encode());
            }
            let mut opener = MediaOpener::new(&MediaKey::from_bytes([3; 32]), 1);
            prop_assert!(opener.open(&bytes).is_err());
        }
    }
}
