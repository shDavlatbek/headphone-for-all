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

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::Result;

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
}

impl PairingSession {
    /// Starts SPAKE2 with `password` (PIN or token), bound to `handshake_hash`. Returns the
    /// session and the outgoing SPAKE2 message.
    pub fn start(_password: &str, _handshake_hash: &[u8; 32]) -> (PairingSession, Vec<u8>) {
        todo!("feat/proto")
    }

    /// Completes SPAKE2 with the peer's message and derives the shared [`PairingKey`].
    ///
    /// Note: a wrong password is NOT detected here; it is detected by [`PairingKey::verify`].
    ///
    /// # Errors
    /// [`crate::ProtoError::Pairing`] if the peer message is malformed.
    pub fn finish(self, _peer_msg: &[u8]) -> Result<PairingKey> {
        let _ = (&self.spake, &self.handshake_hash);
        todo!("feat/proto")
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
    pub fn confirm_mac(&self, _role: PairingRole) -> [u8; 32] {
        let _ = (&self.key, &self.handshake_hash);
        todo!("feat/proto")
    }

    /// Checks (in constant time) that `mac` equals `confirm_mac(role)`.
    ///
    /// # Errors
    /// [`crate::ProtoError::Pairing`] on mismatch (wrong PIN/token or tampering).
    pub fn verify(&self, _role: PairingRole, _mac: &[u8]) -> Result<()> {
        todo!("feat/proto")
    }
}

/// Generates a uniformly random 6-digit PIN (`"000000"`..=`"999999"`).
pub fn generate_pin() -> String {
    todo!("feat/proto")
}

/// Generates a one-time token: 16 random bytes, base64url without padding (22 chars).
pub fn generate_token() -> String {
    todo!("feat/proto")
}
