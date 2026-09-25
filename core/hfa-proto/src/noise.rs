//! Noise XX handshake and transport for the TCP control channel (`snow`).
//!
//! The control channel frames each handshake message with a `u16` BE length prefix (done by
//! `hfa-core`); after the handshake every [`NoiseTransport`] message carries one
//! [`crate::encode_frame`] payload.

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::Result;

/// The Noise protocol name used by every hfa peer.
pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

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
        todo!("feat/proto")
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
    pub fn initiator(_keys: &StaticKeypair) -> Result<Self> {
        todo!("feat/proto")
    }

    /// Starts a handshake as the responder (the hub).
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the handshake state cannot be built.
    pub fn responder(_keys: &StaticKeypair) -> Result<Self> {
        todo!("feat/proto")
    }

    /// Writes the next handshake message carrying `payload`.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] on protocol misuse (wrong turn) or crypto failure.
    pub fn write_message(&mut self, _payload: &[u8]) -> Result<Vec<u8>> {
        let _ = &self.state;
        todo!("feat/proto")
    }

    /// Reads the peer's next handshake message and returns its payload.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] on protocol misuse or authentication failure.
    pub fn read_message(&mut self, _message: &[u8]) -> Result<Vec<u8>> {
        todo!("feat/proto")
    }

    /// `true` once all handshake messages have been exchanged.
    pub fn is_finished(&self) -> bool {
        todo!("feat/proto")
    }

    /// The peer's static public key, once it has been received.
    pub fn remote_static(&self) -> Option<[u8; 32]> {
        todo!("feat/proto")
    }

    /// The handshake hash `h`. Unique per session; pairing binds to it.
    pub fn handshake_hash(&self) -> [u8; 32] {
        todo!("feat/proto")
    }

    /// Converts a finished handshake into transport mode.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if the handshake is not finished.
    pub fn into_transport(self) -> Result<NoiseTransport> {
        todo!("feat/proto")
    }
}

/// Noise transport state after a finished handshake.
pub struct NoiseTransport {
    state: snow::TransportState,
}

impl NoiseTransport {
    /// Encrypts one message (at most 65 535 − 16 bytes of plaintext).
    ///
    /// # Errors
    /// [`crate::ProtoError::FrameTooLarge`] or [`crate::ProtoError::Noise`].
    pub fn encrypt(&mut self, _plaintext: &[u8]) -> Result<Vec<u8>> {
        let _ = &self.state;
        todo!("feat/proto")
    }

    /// Decrypts and authenticates one message.
    ///
    /// # Errors
    /// [`crate::ProtoError::Noise`] if authentication fails.
    pub fn decrypt(&mut self, _ciphertext: &[u8]) -> Result<Vec<u8>> {
        todo!("feat/proto")
    }
}
