//! Noise XX handshake and transport for the TCP control channel (`snow`).
//!
//! The control channel frames each handshake message with a `u16` BE length prefix (done by
//! `hfa-core`); after the handshake every [`NoiseTransport`] message carries one
//! [`crate::encode_frame`] payload.
//!
//! Details that are part of the wire contract:
//! - Protocol name [`NOISE_PATTERN`], prologue [`NOISE_PROLOGUE`] (both sides must use the
//!   same prologue or the handshake fails, which binds the session to protocol version 0).
//! - A Noise message is at most [`NOISE_MAX_MESSAGE`] bytes including the
//!   [`NOISE_TAG_LEN`]-byte tag, so a transport message carries at most
//!   [`NOISE_MAX_PLAINTEXT`] bytes of plaintext.
//! - A remote static key of small order (whose Diffie-Hellman output is predictable) is
//!   rejected, so no one can hold an identity without its private key.

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{ProtoError, Result};

/// The Noise protocol name used by every hfa peer.
pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Prologue mixed into the handshake hash by both sides.
pub const NOISE_PROLOGUE: &[u8] = b"hfa-v0 control";
/// Largest Noise message on the wire (handshake or transport), tag included.
pub const NOISE_MAX_MESSAGE: usize = 65_535;
/// Length of the ChaCha20-Poly1305 tag Noise appends to every encrypted payload.
pub const NOISE_TAG_LEN: usize = 16;
/// Largest plaintext one [`NoiseTransport`] message can carry.
pub const NOISE_MAX_PLAINTEXT: usize = NOISE_MAX_MESSAGE - NOISE_TAG_LEN;

// A control frame of the maximum size always fits into one transport message.
const _: () =
    assert!(crate::MAX_CONTROL_FRAME + crate::control::FRAME_PREFIX_LEN <= NOISE_MAX_PLAINTEXT);

/// X25519 public keys of small order (and their encodings with the ignored top bit set).
/// Their shared secret with any private key is all zeros. Same list as libsodium.
const SMALL_ORDER_KEYS: [[u8; 32]; 7] = [
    // 0 (order 4)
    [0; 32],
    // 1 (order 1)
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // order 8
    [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x00,
    ],
    // order 8
    [
        0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef,
        0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f,
        0x11, 0x57,
    ],
    // p - 1 (order 2)
    [
        0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    // p (= 0)
    [
        0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    // p + 1 (= 1)
    [
        0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
];

/// `true` if `key` is a small-order X25519 point (X25519 ignores the top bit of the last byte).
pub(crate) fn is_small_order(key: &[u8; 32]) -> bool {
    SMALL_ORDER_KEYS
        .iter()
        .any(|bad| bad[..31] == key[..31] && bad[31] == key[31] & 0x7f)
}

/// Parsed [`NOISE_PATTERN`].
fn params() -> Result<snow::params::NoiseParams> {
    NOISE_PATTERN.parse().map_err(noise_err)
}

/// Maps a `snow` error to [`ProtoError::Noise`].
fn noise_err(e: snow::Error) -> ProtoError {
    ProtoError::Noise(e.to_string())
}

/// Copies a 32-byte key out of a `snow` slice.
fn key32(bytes: &[u8]) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| ProtoError::InvalidKey(format!("expected 32 bytes, got {}", bytes.len())))
}

/// A device's long-term X25519 static keypair. The public key identifies the device
/// (its fingerprint is the `device_id`). The private key is wiped on drop and never printed.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct StaticKeypair {
    /// X25519 private key.
    pub private: [u8; 32],
    /// X25519 public key.
    pub public: [u8; 32],
}

impl StaticKeypair {
    /// Generates a new random keypair.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the crypto backend fails (should not happen).
    pub fn generate() -> Result<Self> {
        let mut kp = snow::Builder::new(params()?)
            .generate_keypair()
            .map_err(noise_err)?;
        let result = match (key32(&kp.private), key32(&kp.public)) {
            (Ok(private), Ok(public)) => Ok(Self { private, public }),
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
        kp.private.zeroize();
        result
    }

    /// Rebuilds the keypair from its private key: the public key is derived
    /// (X25519 scalar multiplication of the base point, as the Noise handshake itself does).
    /// Use it to check a stored public key against the private key it belongs to.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the crypto backend is unavailable (should not happen);
    /// [`crate::ProtoError::InvalidKey`] for an all-zero private key.
    pub fn from_private(private: &[u8; 32]) -> Result<Self> {
        use snow::resolvers::CryptoResolver as _;
        if *private == [0; 32] {
            return Err(ProtoError::InvalidKey("all-zero private key".into()));
        }
        let mut dh = snow::resolvers::DefaultResolver
            .resolve_dh(&params()?.dh)
            .ok_or_else(|| ProtoError::Noise("no X25519 implementation".into()))?;
        dh.set(private);
        let public = key32(dh.pubkey());
        // Do not leave a copy of the private key in the backend's buffer.
        dh.set(&[0; 32]);
        Ok(Self {
            private: *private,
            public: public?,
        })
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticKeypair")
            .field("private", &"<redacted>")
            .field("public", &self.public)
            .finish()
    }
}

/// An in-progress Noise XX handshake (3 messages: `-> e`, `<- e, ee, s, es`, `-> s, se`).
pub struct NoiseHandshake {
    state: snow::HandshakeState,
}

impl NoiseHandshake {
    /// Starts a handshake as the initiator (the sender, which opens the TCP connection).
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the handshake state cannot be built.
    pub fn initiator(keys: &StaticKeypair) -> Result<Self> {
        Self::build(&keys.private, true)
    }

    fn build(private: &[u8; 32], initiator: bool) -> Result<Self> {
        let builder = snow::Builder::new(params()?)
            .prologue(NOISE_PROLOGUE)
            .map_err(noise_err)?
            .local_private_key(private)
            .map_err(noise_err)?;
        let state = if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(noise_err)?;
        Ok(Self { state })
    }

    /// Starts a handshake as the responder (the hub).
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the handshake state cannot be built.
    pub fn responder(keys: &StaticKeypair) -> Result<Self> {
        Self::build(&keys.private, false)
    }

    /// Writes the next handshake message carrying `payload`.
    ///
    /// The payload of the first XX message (`-> e`) is **not encrypted** and no payload is
    /// authenticated before the handshake finishes, so hfa sends empty handshake payloads and
    /// exchanges `Hello` over the transport.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] on protocol misuse (wrong turn, handshake finished),
    /// crypto failure, or if the message would exceed [`NOISE_MAX_MESSAGE`] bytes.
    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.len() > NOISE_MAX_PLAINTEXT {
            return Err(ProtoError::FrameTooLarge {
                len: payload.len(),
                max: NOISE_MAX_PLAINTEXT,
            });
        }
        let mut buf = vec![0u8; NOISE_MAX_MESSAGE];
        let n = self
            .state
            .write_message(payload, &mut buf)
            .map_err(noise_err)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Reads the peer's next handshake message and returns its payload.
    ///
    /// # Errors
    /// [`crate::ProtoError::FrameTooLarge`] for a message above [`NOISE_MAX_MESSAGE`],
    /// [`crate::ProtoError::Noise`] on protocol misuse or authentication failure, or
    /// [`crate::ProtoError::InvalidKey`] if the peer's static key has small order.
    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        if message.len() > NOISE_MAX_MESSAGE {
            return Err(ProtoError::FrameTooLarge {
                len: message.len(),
                max: NOISE_MAX_MESSAGE,
            });
        }
        let mut buf = vec![0u8; message.len()];
        let n = self
            .state
            .read_message(message, &mut buf)
            .map_err(noise_err)?;
        buf.truncate(n);
        if let Some(remote) = self.remote_static() {
            if is_small_order(&remote) {
                return Err(ProtoError::InvalidKey(
                    "peer static key has small order".into(),
                ));
            }
        }
        Ok(buf)
    }

    /// `true` once all handshake messages have been exchanged.
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// `true` if the next step is [`NoiseHandshake::write_message`] (else `read_message`).
    pub fn is_my_turn(&self) -> bool {
        self.state.is_my_turn()
    }

    /// The peer's static public key, once it has been received.
    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.state.get_remote_static().and_then(|k| key32(k).ok())
    }

    /// The handshake hash `h`. It changes with every message; it is final (unique per
    /// session, identical on both sides) once [`NoiseHandshake::is_finished`] is `true`, and
    /// pairing must bind to that final value.
    pub fn handshake_hash(&self) -> [u8; 32] {
        let h = self.state.get_handshake_hash();
        // BLAKE2s output is always 32 bytes; copy defensively instead of panicking.
        let mut out = [0u8; 32];
        let n = h.len().min(32);
        out[..n].copy_from_slice(&h[..n]);
        out
    }

    /// Converts a finished handshake into transport mode.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the handshake is not finished.
    pub fn into_transport(self) -> Result<NoiseTransport> {
        if !self.is_finished() {
            return Err(ProtoError::Noise("handshake not finished".into()));
        }
        let handshake_hash = self.handshake_hash();
        let remote_static = self
            .remote_static()
            .ok_or_else(|| ProtoError::Noise("no remote static key".into()))?;
        let state = self.state.into_transport_mode().map_err(noise_err)?;
        Ok(NoiseTransport {
            state,
            handshake_hash,
            remote_static,
        })
    }
}

impl fmt::Debug for NoiseHandshake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoiseHandshake")
            .field("initiator", &self.state.is_initiator())
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

/// Noise transport state after a finished handshake.
///
/// Messages must be decrypted in the order they were encrypted (Noise uses an implicit
/// counter nonce), which a TCP stream guarantees. After a failed [`NoiseTransport::decrypt`]
/// the channel must be closed.
pub struct NoiseTransport {
    state: snow::TransportState,
    handshake_hash: [u8; 32],
    remote_static: [u8; 32],
}

impl NoiseTransport {
    /// Encrypts one message (at most [`NOISE_MAX_PLAINTEXT`] = 65 535 − 16 bytes of
    /// plaintext). The result is `plaintext.len() + 16` bytes.
    ///
    /// # Errors
    /// [`crate::ProtoError::FrameTooLarge`] or [`crate::ProtoError::Noise`].
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if plaintext.len() > NOISE_MAX_PLAINTEXT {
            return Err(ProtoError::FrameTooLarge {
                len: plaintext.len(),
                max: NOISE_MAX_PLAINTEXT,
            });
        }
        let mut buf = vec![0u8; plaintext.len() + NOISE_TAG_LEN];
        let n = self
            .state
            .write_message(plaintext, &mut buf)
            .map_err(noise_err)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Decrypts and authenticates one message.
    ///
    /// # Errors
    /// [`crate::ProtoError::FrameTooLarge`] above [`NOISE_MAX_MESSAGE`] bytes,
    /// [`crate::ProtoError::Truncated`] below the tag length, or
    /// [`crate::ProtoError::Noise`] if authentication fails.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        if ciphertext.len() > NOISE_MAX_MESSAGE {
            return Err(ProtoError::FrameTooLarge {
                len: ciphertext.len(),
                max: NOISE_MAX_MESSAGE,
            });
        }
        if ciphertext.len() < NOISE_TAG_LEN {
            return Err(ProtoError::Truncated {
                needed: NOISE_TAG_LEN,
                got: ciphertext.len(),
            });
        }
        let mut buf = vec![0u8; ciphertext.len()];
        let n = self
            .state
            .read_message(ciphertext, &mut buf)
            .map_err(noise_err)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// The final handshake hash of this session (see [`NoiseHandshake::handshake_hash`]).
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.handshake_hash
    }

    /// The authenticated static public key of the peer.
    pub fn remote_static(&self) -> [u8; 32] {
        self.remote_static
    }
}

impl fmt::Debug for NoiseTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoiseTransport")
            .field("initiator", &self.state.is_initiator())
            .field("remote_static", &self.remote_static)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Runs the three XX messages with the given payloads and returns both finished sides.
    fn handshake(
        a: &StaticKeypair,
        b: &StaticKeypair,
        payloads: [&[u8]; 3],
    ) -> (NoiseHandshake, NoiseHandshake) {
        let mut init = NoiseHandshake::initiator(a).unwrap();
        let mut resp = NoiseHandshake::responder(b).unwrap();
        assert!(init.is_my_turn() && !resp.is_my_turn());
        let m1 = init.write_message(payloads[0]).unwrap();
        assert_eq!(resp.read_message(&m1).unwrap(), payloads[0]);
        assert_eq!(resp.remote_static(), None);
        let m2 = resp.write_message(payloads[1]).unwrap();
        assert_eq!(init.read_message(&m2).unwrap(), payloads[1]);
        assert_eq!(init.remote_static(), Some(b.public));
        assert!(!init.is_finished());
        let m3 = init.write_message(payloads[2]).unwrap();
        assert_eq!(resp.read_message(&m3).unwrap(), payloads[2]);
        assert!(init.is_finished() && resp.is_finished());
        (init, resp)
    }

    #[test]
    fn from_private_derives_the_public_key() {
        let kp = StaticKeypair::generate().unwrap();
        assert_eq!(StaticKeypair::from_private(&kp.private).unwrap(), kp);
        // RFC 7748 §6.1 test vector (Alice).
        let hex = |s: &str| -> [u8; 32] {
            let v: Vec<u8> = (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect();
            v.try_into().unwrap()
        };
        let alice = StaticKeypair::from_private(&hex(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        ))
        .unwrap();
        assert_eq!(
            alice.public,
            hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        assert!(matches!(
            StaticKeypair::from_private(&[0; 32]),
            Err(ProtoError::InvalidKey(_))
        ));
    }

    fn transports() -> (NoiseTransport, NoiseTransport, StaticKeypair, StaticKeypair) {
        let a = StaticKeypair::generate().unwrap();
        let b = StaticKeypair::generate().unwrap();
        let (i, r) = handshake(&a, &b, [b"", b"", b""]);
        (
            i.into_transport().unwrap(),
            r.into_transport().unwrap(),
            a,
            b,
        )
    }

    #[test]
    fn full_xx_handshake_agrees() {
        let a = StaticKeypair::generate().unwrap();
        let b = StaticKeypair::generate().unwrap();
        assert_ne!(a.public, b.public);
        let (init, resp) = handshake(&a, &b, [b"one", b"two", b"three"]);
        assert_eq!(init.handshake_hash(), resp.handshake_hash());
        assert_ne!(init.handshake_hash(), [0; 32]);
        assert_eq!(init.remote_static(), Some(b.public));
        assert_eq!(resp.remote_static(), Some(a.public));

        let hash = init.handshake_hash();
        let ti = init.into_transport().unwrap();
        let tr = resp.into_transport().unwrap();
        assert_eq!(ti.handshake_hash(), hash);
        assert_eq!(tr.handshake_hash(), hash);
        assert_eq!(ti.remote_static(), b.public);
        assert_eq!(tr.remote_static(), a.public);
    }

    #[test]
    fn handshake_hash_is_unique_per_session() {
        let a = StaticKeypair::generate().unwrap();
        let b = StaticKeypair::generate().unwrap();
        let (i1, _) = handshake(&a, &b, [b"", b"", b""]);
        let (i2, _) = handshake(&a, &b, [b"", b"", b""]);
        assert_ne!(i1.handshake_hash(), i2.handshake_hash());
    }

    #[test]
    fn transport_roundtrip_both_ways() {
        let (mut ti, mut tr, _, _) = transports();
        for i in 1..=5u8 {
            let msg = vec![i; 100 * usize::from(i)];
            let ct = ti.encrypt(&msg).unwrap();
            assert_eq!(ct.len(), msg.len() + NOISE_TAG_LEN);
            assert_ne!(&ct[..msg.len()], &msg[..], "not encrypted");
            assert_eq!(tr.decrypt(&ct).unwrap(), msg);
            let back = tr.encrypt(b"reply").unwrap();
            assert_eq!(ti.decrypt(&back).unwrap(), b"reply");
        }
    }

    #[test]
    fn transport_size_limits() {
        let (mut ti, mut tr, _, _) = transports();
        let max = vec![7u8; NOISE_MAX_PLAINTEXT];
        let ct = ti.encrypt(&max).unwrap();
        assert_eq!(ct.len(), NOISE_MAX_MESSAGE);
        assert_eq!(tr.decrypt(&ct).unwrap(), max);
        assert_eq!(
            ti.encrypt(&[0; NOISE_MAX_PLAINTEXT + 1]),
            Err(ProtoError::FrameTooLarge {
                len: NOISE_MAX_PLAINTEXT + 1,
                max: NOISE_MAX_PLAINTEXT
            })
        );
        assert_eq!(
            tr.decrypt(&[0; NOISE_MAX_MESSAGE + 1]),
            Err(ProtoError::FrameTooLarge {
                len: NOISE_MAX_MESSAGE + 1,
                max: NOISE_MAX_MESSAGE
            })
        );
        assert_eq!(
            tr.decrypt(&[0; 15]),
            Err(ProtoError::Truncated {
                needed: NOISE_TAG_LEN,
                got: 15
            })
        );
    }

    #[test]
    fn tampered_or_reordered_transport_messages_fail() {
        let (mut ti, mut tr, _, _) = transports();
        let mut ct = ti.encrypt(b"hello").unwrap();
        ct[0] ^= 1;
        assert!(matches!(tr.decrypt(&ct), Err(ProtoError::Noise(_))));

        let (mut ti, mut tr, _, _) = transports();
        let first = ti.encrypt(b"first").unwrap();
        let second = ti.encrypt(b"second").unwrap();
        assert!(tr.decrypt(&second).is_err(), "out of order must fail");
        let _ = first;
    }

    #[test]
    fn tampered_handshake_fails() {
        let a = StaticKeypair::generate().unwrap();
        let b = StaticKeypair::generate().unwrap();
        let mut init = NoiseHandshake::initiator(&a).unwrap();
        let mut resp = NoiseHandshake::responder(&b).unwrap();
        let m1 = init.write_message(&[]).unwrap();
        resp.read_message(&m1).unwrap();
        let mut m2 = resp.write_message(&[]).unwrap();
        // Flip a bit in the encrypted static key.
        m2[40] ^= 1;
        assert!(matches!(init.read_message(&m2), Err(ProtoError::Noise(_))));
    }

    #[test]
    fn wrong_turn_and_early_transport_are_errors() {
        let a = StaticKeypair::generate().unwrap();
        let mut resp = NoiseHandshake::responder(&a).unwrap();
        assert!(matches!(resp.write_message(&[]), Err(ProtoError::Noise(_))));
        let init = NoiseHandshake::initiator(&a).unwrap();
        assert!(matches!(init.into_transport(), Err(ProtoError::Noise(_))));
        let mut init = NoiseHandshake::initiator(&a).unwrap();
        assert!(matches!(
            init.read_message(&[0; 32]),
            Err(ProtoError::Noise(_))
        ));
        assert!(matches!(
            init.write_message(&[0; NOISE_MAX_PLAINTEXT + 1]),
            Err(ProtoError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn small_order_static_keys_are_rejected() {
        for bad in SMALL_ORDER_KEYS {
            assert!(is_small_order(&bad));
            let mut high = bad;
            high[31] |= 0x80;
            assert!(is_small_order(&high), "top bit is ignored by X25519");
        }
        // Real keys are never flagged. (snow derives the public key from the private key,
        // so a small-order static key cannot be put into a snow handshake to exercise the
        // guard in `read_message` end to end.)
        for _ in 0..16 {
            let good = StaticKeypair::generate().unwrap();
            assert!(!is_small_order(&good.public));
        }
    }

    #[test]
    fn keypair_debug_redacts_private_key() {
        let kp = StaticKeypair {
            private: [0xAB; 32],
            public: [1; 32],
        };
        let text = format!("{kp:?}");
        assert!(
            text.contains("<redacted>") && !text.contains("171"),
            "{text}"
        );
    }

    #[test]
    fn keypair_zeroize() {
        let mut kp = StaticKeypair::generate().unwrap();
        kp.zeroize();
        assert_eq!((kp.private, kp.public), ([0; 32], [0; 32]));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Garbage handshake or transport input never panics and never authenticates.
        #[test]
        fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let kp = StaticKeypair::generate().unwrap();
            let mut resp = NoiseHandshake::responder(&kp).unwrap();
            let _ = resp.read_message(&bytes);
            let (_, mut tr, _, _) = transports();
            prop_assert!(tr.decrypt(&bytes).is_err());
        }
    }
}
