//! PIN / token pairing: symmetric SPAKE2 (Ed25519 group) bound to the Noise handshake hash,
//! followed by HMAC-SHA256 key confirmation.
//!
//! Flow (inside the Noise-encrypted control channel):
//! 1. Sender sends `PairStart{method}`.
//! 2. Both sides call [`PairingSession::start`] with the shared secret (PIN or token) and the
//!    Noise handshake hash, and exchange `PairSpake{msg}`.
//! 3. Both call [`PairingSession::finish`] with the peer's message and get a [`PairingKey`].
//! 4. Both exchange `PairConfirm{mac = key.confirm_mac(own role)}` and check the peer's with
//!    [`PairingKey::verify`] (peer role). The hub answers `PairResult`.
//!
//! Construction (wire contract):
//! - SPAKE2 from the `spake2` crate, `Ed25519Group`, symmetric mode, identity
//!   [`SPAKE2_IDENTITY`]; the password is the UTF-8 PIN or token.
//! - `PairingKey = HKDF-SHA256(ikm = SPAKE2 key, salt = Noise handshake hash,
//!   info = `[`PAIRING_KDF_INFO`]`)`, 32 bytes. The salt binds the key to this Noise session,
//!   so a man in the middle (who necessarily runs two Noise sessions with different hashes)
//!   cannot relay the pairing.
//! - `confirm_mac(role) = HMAC-SHA256(PairingKey, role.label() ‖ handshake_hash)`.

use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{ProtoError, Result};

/// SPAKE2 symmetric-mode identity (both sides).
pub const SPAKE2_IDENTITY: &[u8] = b"hfa-pairing-v0";
/// HKDF `info` for the pairing key.
pub const PAIRING_KDF_INFO: &[u8] = b"hfa pairing v0";
/// Number of digits of a pairing PIN.
pub const PIN_DIGITS: usize = 6;
/// Number of random bytes in a pairing token.
pub const TOKEN_BYTES: usize = 16;
/// Length of a confirmation MAC.
pub const MAC_LEN: usize = 32;

/// Which side computes a confirmation MAC. Each side's MAC uses a distinct label so a MAC
/// cannot be reflected back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairingRole {
    /// The hub (Noise responder).
    Hub,
    /// The sender (Noise initiator).
    Sender,
}

impl PairingRole {
    /// Label prepended to the handshake hash in the confirmation MAC. Part of the wire contract.
    pub fn label(self) -> &'static [u8] {
        match self {
            PairingRole::Hub => b"hfa-v0 pair-confirm hub",
            PairingRole::Sender => b"hfa-v0 pair-confirm sender",
        }
    }
}

/// An in-progress SPAKE2 exchange.
pub struct PairingSession {
    spake: spake2::Spake2<spake2::Ed25519Group>,
    handshake_hash: [u8; 32],
    /// Our outgoing message, to detect a reflected message.
    own_msg: Vec<u8>,
}

impl PairingSession {
    /// Starts SPAKE2 with `password` (PIN or token), bound to `handshake_hash`. Returns the
    /// session and the outgoing SPAKE2 message (33 bytes).
    pub fn start(password: &str, handshake_hash: &[u8; 32]) -> (PairingSession, Vec<u8>) {
        let (spake, msg) = spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(
            &spake2::Password::new(password.as_bytes()),
            &spake2::Identity::new(SPAKE2_IDENTITY),
        );
        let session = PairingSession {
            spake,
            handshake_hash: *handshake_hash,
            own_msg: msg.clone(),
        };
        (session, msg)
    }

    /// Completes SPAKE2 with the peer's message and derives the shared [`PairingKey`].
    ///
    /// Note: a wrong password is NOT detected here; it is detected by [`PairingKey::verify`].
    ///
    /// # Errors
    /// [`crate::ProtoError::Pairing`] if the peer message is malformed (wrong length, wrong
    /// side marker, not a curve point) or is our own message reflected back.
    pub fn finish(self, peer_msg: &[u8]) -> Result<PairingKey> {
        if bool::from(peer_msg.ct_eq(&self.own_msg)) {
            return Err(ProtoError::Pairing("reflected SPAKE2 message".into()));
        }
        let spake_key = Zeroizing::new(
            self.spake
                .finish(peer_msg)
                .map_err(|e| ProtoError::Pairing(format!("SPAKE2: {e}")))?,
        );
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&self.handshake_hash), &spake_key)
            .expand(PAIRING_KDF_INFO, &mut key)
            .map_err(|_| ProtoError::Pairing("key derivation failed".into()))?;
        Ok(PairingKey {
            key,
            handshake_hash: self.handshake_hash,
        })
    }
}

impl std::fmt::Debug for PairingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingSession").finish_non_exhaustive()
    }
}

/// The key agreed through SPAKE2. Wiped on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct PairingKey {
    key: [u8; 32],
    handshake_hash: [u8; 32],
}

impl PairingKey {
    /// HMAC-SHA256(key, `role.label()` ‖ handshake_hash).
    pub fn confirm_mac(&self, role: PairingRole) -> [u8; MAC_LEN] {
        // HMAC zero-pads keys shorter than the 64-byte SHA-256 block (RFC 2104), so padding
        // the 32-byte key ourselves gives the same MAC through the infallible `KeyInit::new`.
        let mut block = Zeroizing::new([0u8; 64]);
        block[..32].copy_from_slice(&self.key);
        let mut mac = <Hmac<Sha256> as KeyInit>::new(&(*block).into());
        mac.update(role.label());
        mac.update(&self.handshake_hash);
        mac.finalize().into_bytes().into()
    }

    /// Checks (in constant time) that `mac` equals `confirm_mac(role)`.
    ///
    /// # Errors
    /// [`crate::ProtoError::Pairing`] on mismatch (wrong PIN/token or tampering).
    pub fn verify(&self, role: PairingRole, mac: &[u8]) -> Result<()> {
        let expected = self.confirm_mac(role);
        // `ct_eq` on slices of different lengths returns false without comparing contents.
        if bool::from(expected.as_slice().ct_eq(mac)) {
            Ok(())
        } else {
            Err(ProtoError::Pairing(
                "confirmation MAC mismatch (wrong PIN/token?)".into(),
            ))
        }
    }
}

impl std::fmt::Debug for PairingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingKey(<redacted>)")
    }
}

/// Generates a uniformly random 6-digit PIN (`"000000"`..=`"999999"`) from the thread-local
/// CSPRNG (OS-seeded).
pub fn generate_pin() -> String {
    // Rejection sampling: 4_294_000_000 is the largest multiple of 10^6 below 2^32, so every
    // accepted value maps to each PIN equally often (the loop almost never repeats:
    // P(reject) ≈ 2.3e-4).
    const LIMIT: u32 = 4_294_000_000;
    let mut rng = rand::rng();
    loop {
        let v = rng.next_u32();
        if v < LIMIT {
            return format!("{:06}", v % 1_000_000);
        }
    }
}

/// Generates a one-time token: 16 random bytes, base64url without padding (22 chars), from
/// the thread-local CSPRNG (OS-seeded).
pub fn generate_token() -> String {
    let mut bytes = Zeroizing::new([0u8; TOKEN_BYTES]);
    rand::rng().fill_bytes(&mut *bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::Mac;

    const HH: [u8; 32] = [0x11; 32];

    /// Runs both SPAKE2 halves and returns (sender key, hub key).
    fn pair(
        pw_sender: &str,
        pw_hub: &str,
        hh_sender: &[u8; 32],
        hh_hub: &[u8; 32],
    ) -> (PairingKey, PairingKey) {
        let (s, s_msg) = PairingSession::start(pw_sender, hh_sender);
        let (h, h_msg) = PairingSession::start(pw_hub, hh_hub);
        assert_eq!(s_msg.len(), 33);
        (s.finish(&h_msg).unwrap(), h.finish(&s_msg).unwrap())
    }

    #[test]
    fn same_pin_pairs_and_both_macs_verify() {
        let (sender, hub) = pair("042137", "042137", &HH, &HH);
        assert_eq!(sender.key, hub.key);
        let s_mac = sender.confirm_mac(PairingRole::Sender);
        let h_mac = hub.confirm_mac(PairingRole::Hub);
        hub.verify(PairingRole::Sender, &s_mac).unwrap();
        sender.verify(PairingRole::Hub, &h_mac).unwrap();
    }

    #[test]
    fn different_pin_fails_with_mac_mismatch() {
        let (sender, hub) = pair("123456", "123457", &HH, &HH);
        assert_ne!(sender.key, hub.key);
        let s_mac = sender.confirm_mac(PairingRole::Sender);
        assert!(matches!(
            hub.verify(PairingRole::Sender, &s_mac),
            Err(ProtoError::Pairing(_))
        ));
        let h_mac = hub.confirm_mac(PairingRole::Hub);
        assert!(sender.verify(PairingRole::Hub, &h_mac).is_err());
    }

    #[test]
    fn different_handshake_hash_fails() {
        // Same PIN, but the two sides are in different Noise sessions (a relaying MITM).
        let (sender, hub) = pair("555555", "555555", &[1; 32], &[2; 32]);
        let s_mac = sender.confirm_mac(PairingRole::Sender);
        assert!(hub.verify(PairingRole::Sender, &s_mac).is_err());
    }

    #[test]
    fn macs_differ_per_role_and_cannot_be_reflected() {
        let (sender, hub) = pair("000000", "000000", &HH, &HH);
        let s_mac = sender.confirm_mac(PairingRole::Sender);
        let h_mac = sender.confirm_mac(PairingRole::Hub);
        assert_ne!(s_mac, h_mac);
        assert_ne!(PairingRole::Hub.label(), PairingRole::Sender.label());
        // The hub must not accept its own MAC reflected back as the sender's.
        assert!(hub
            .verify(PairingRole::Sender, &hub.confirm_mac(PairingRole::Hub))
            .is_err());
    }

    #[test]
    fn verify_rejects_wrong_lengths() {
        let (sender, _) = pair("1", "1", &HH, &HH);
        let mac = sender.confirm_mac(PairingRole::Sender);
        assert!(sender.verify(PairingRole::Sender, &mac[..31]).is_err());
        let mut long = mac.to_vec();
        long.push(0);
        assert!(sender.verify(PairingRole::Sender, &long).is_err());
        assert!(sender.verify(PairingRole::Sender, &[]).is_err());
    }

    /// Pins the MAC and KDF construction against an independent computation.
    #[test]
    fn construction_matches_spec() {
        let key = PairingKey {
            key: [0x42; 32],
            handshake_hash: HH,
        };
        let mut reference = <Hmac<Sha256> as KeyInit>::new_from_slice(&[0x42; 32]).unwrap();
        reference.update(b"hfa-v0 pair-confirm sender");
        reference.update(&HH);
        let expected: [u8; 32] = reference.finalize().into_bytes().into();
        assert_eq!(key.confirm_mac(PairingRole::Sender), expected);

        // Derivation: HKDF-SHA256(ikm = SPAKE2 key, salt = hh, info = "hfa pairing v0"),
        // with symmetric SPAKE2 over the identity "hfa-pairing-v0".
        let pw = spake2::Password::new(b"pw");
        let id = spake2::Identity::new(b"hfa-pairing-v0");
        let (ours, our_msg) = spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(&pw, &id);
        let (peer, peer_msg) = spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(&pw, &id);
        let spake_key = peer.finish(&our_msg).unwrap();
        let mut okm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&HH), &spake_key)
            .expand(b"hfa pairing v0", &mut okm)
            .unwrap();
        let session = PairingSession {
            spake: ours,
            handshake_hash: HH,
            own_msg: our_msg,
        };
        assert_eq!(session.finish(&peer_msg).unwrap().key, okm);

        // And `start` really uses that identity: its message is compatible with the reference.
        let (session, msg) = PairingSession::start("pw", &HH);
        let (peer, peer_msg) = spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(&pw, &id);
        let spake_key = peer.finish(&msg).unwrap();
        let mut okm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&HH), &spake_key)
            .expand(PAIRING_KDF_INFO, &mut okm)
            .unwrap();
        assert_eq!(session.finish(&peer_msg).unwrap().key, okm);
    }

    #[test]
    fn malformed_and_reflected_messages_are_rejected() {
        let (s, msg) = PairingSession::start("123456", &HH);
        assert!(matches!(s.finish(&msg), Err(ProtoError::Pairing(_))));
        let (s, _) = PairingSession::start("123456", &HH);
        assert!(s.finish(&[0x53; 10]).is_err());
        let (s, _) = PairingSession::start("123456", &HH);
        assert!(s.finish(&[]).is_err());
        // Wrong side marker ('A' instead of 'S').
        let (s, _) = PairingSession::start("123456", &HH);
        let (_, mut other) = PairingSession::start("123456", &HH);
        other[0] = 0x41;
        assert!(s.finish(&other).is_err());
    }

    #[test]
    fn debug_never_prints_keys() {
        let (sender, _) = pair("1", "1", &HH, &HH);
        assert_eq!(format!("{sender:?}"), "PairingKey(<redacted>)");
        let (session, _) = PairingSession::start("123456", &HH);
        let text = format!("{session:?}");
        assert!(!text.contains("123456"), "{text}");
    }

    #[test]
    fn pairing_key_zeroize() {
        let (mut key, _) = pair("1", "1", &HH, &HH);
        key.zeroize();
        assert_eq!((key.key, key.handshake_hash), ([0; 32], [0; 32]));
    }

    #[test]
    fn pins_are_six_digits_and_spread() {
        let mut first_digits = [0u32; 10];
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            let pin = generate_pin();
            assert_eq!(pin.len(), PIN_DIGITS);
            assert!(pin.bytes().all(|b| b.is_ascii_digit()), "{pin}");
            first_digits[usize::from(pin.as_bytes()[0] - b'0')] += 1;
            seen.insert(pin);
        }
        // 2000 draws from 10^6 values: collisions are rare (expected ~2).
        assert!(seen.len() > 1980, "{}", seen.len());
        // Every leading digit (expected 200 each) shows up, leading zeros included.
        assert!(first_digits.iter().all(|&c| c > 120), "{first_digits:?}");
    }

    #[test]
    fn tokens_are_22_char_base64url_of_16_bytes() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let t = generate_token();
            assert_eq!(t.len(), 22);
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "{t}"
            );
            let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&t)
                .unwrap();
            assert_eq!(raw.len(), TOKEN_BYTES);
            assert!(seen.insert(t));
        }
    }

    #[test]
    fn token_pairs_like_a_pin() {
        let token = generate_token();
        let (sender, hub) = pair(&token, &token, &HH, &HH);
        hub.verify(
            PairingRole::Sender,
            &sender.confirm_mac(PairingRole::Sender),
        )
        .unwrap();
    }
}
