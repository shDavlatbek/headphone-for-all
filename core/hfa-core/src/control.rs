//! The control channel: TCP + Noise XX, then length-prefixed [`ControlMessage`] frames.
//!
//! Wire procedure:
//! 1. Noise XX handshake; each handshake message is prefixed with its length as `u16` BE.
//!    The sender (TCP client) is the initiator.
//! 2. Both sides exchange `Hello` (protocol version check, device info).
//! 3. If the hub does not trust the sender's static key, pairing is mandatory: the sender
//!    must have a pairing secret and the hub must have an open window (`PairStart`,
//!    `PairSpake`, `PairConfirm`, `PairResult`, bound to the Noise handshake hash). On
//!    success both sides add each other to their [`TrustStore`]. A sender that already
//!    trusts a hub pins its key and fails with [`crate::CoreError::KeyMismatch`] if it changed.
//! 4. Afterwards every Noise transport message is prefixed with its length as `u16` BE and
//!    carries exactly one [`hfa_proto::encode_frame`] payload.

use std::net::SocketAddr;

use hfa_proto::ControlMessage;
use tokio::net::TcpStream;

use crate::identity::{Identity, TrustStore};
use crate::pairing::PairingManager;
use crate::Result;

/// Timeout for the complete handshake + hello + pairing phase.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What we learned about the peer during the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// Fingerprint of the peer's static key.
    pub device_id: String,
    /// Peer display name (from `Hello`).
    pub name: String,
    /// Peer platform (from `Hello`).
    pub platform: String,
    /// Peer application version (from `Hello`).
    pub app_version: String,
    /// Peer Noise static public key.
    pub public_key: [u8; 32],
    /// Peer socket address.
    pub addr: SocketAddr,
    /// `true` if pairing happened during this connection.
    pub newly_paired: bool,
}

/// An established, authenticated control channel.
pub struct ControlChannel {
    stream: TcpStream,
    transport: hfa_proto::NoiseTransport,
    decoder: hfa_proto::FrameDecoder,
}

impl ControlChannel {
    /// Connects to a hub as sender (Noise initiator). `pairing_secret` (PIN or token, see
    /// [`crate::pairing::method_for_secret`]) is used only if the hub requires pairing.
    ///
    /// # Errors
    /// I/O, handshake, [`crate::CoreError::PairingRequired`], [`crate::CoreError::PairingFailed`],
    /// [`crate::CoreError::KeyMismatch`], [`crate::CoreError::Timeout`].
    pub async fn connect(
        _addr: SocketAddr,
        _identity: &Identity,
        _trust: &TrustStore,
        _pairing_secret: Option<String>,
    ) -> Result<(ControlChannel, PeerInfo)> {
        todo!("feat/core-engine")
    }

    /// Accepts a sender on an incoming TCP stream as hub (Noise responder).
    ///
    /// # Errors
    /// I/O, handshake, [`crate::CoreError::PairingRequired`], [`crate::CoreError::PairingFailed`],
    /// [`crate::CoreError::Timeout`].
    pub async fn accept(
        _stream: TcpStream,
        _identity: &Identity,
        _trust: &TrustStore,
        _pairing: &PairingManager,
    ) -> Result<(ControlChannel, PeerInfo)> {
        todo!("feat/core-engine")
    }

    /// Sends one message.
    ///
    /// # Errors
    /// I/O or Noise errors; [`crate::CoreError::Closed`].
    pub async fn send(&mut self, _msg: &ControlMessage) -> Result<()> {
        let _ = (&self.stream, &self.transport, &self.decoder);
        todo!("feat/core-engine")
    }

    /// Receives the next message (cancel-safe is NOT guaranteed; use from one task).
    ///
    /// # Errors
    /// I/O or Noise errors; [`crate::CoreError::Closed`] on EOF.
    pub async fn recv(&mut self) -> Result<ControlMessage> {
        todo!("feat/core-engine")
    }
}
