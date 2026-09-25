//! The control channel: TCP + Noise XX, then length-prefixed [`ControlMessage`] frames.
//!
//! Wire procedure:
//! 1. Noise XX handshake; each handshake message is prefixed with its length as `u16` BE.
//!    The sender (TCP client) is the initiator. If the sender was given an expected hub key
//!    (pairing URI, trusted peer), it compares it with the hub's Noise remote static key right
//!    after the handshake and fails with [`crate::CoreError::KeyMismatch`] on a difference,
//!    before sending anything else.
//! 2. `Hello` exchange: the sender sends its `Hello` first, the hub answers with its own. The
//!    hub sets `Hello.pairing_required = true` iff it does not trust the sender's static key
//!    (the sender leaves the field `false`; the hub ignores it).
//! 3. Pairing is needed if **the sender does not trust the hub's static key** (its
//!    [`TrustStore`]) **or** the hub's `Hello` says `pairing_required`. The sender's own
//!    check is mandatory whatever the hub signals: a sender never streams to an untrusted,
//!    unpaired hub (a rogue hub advertising the same name would otherwise receive the audio).
//!    - Pairing needed and no secret: the sender sends `Bye` and fails with
//!      [`crate::CoreError::PairingRequired`].
//!    - Otherwise the sender sends `PairStart{method}`, both sides exchange `PairSpake`, then
//!      the sender sends its `PairConfirm`; the hub verifies it and answers with its own
//!      `PairConfirm` followed by `PairResult` (SPAKE2 bound to the Noise handshake hash, see
//!      [`hfa_proto::pairing`]). The hub obtains the secret with
//!      [`PairingManager::begin_attempt`], which counts the attempt *before* SPAKE2 runs and
//!      allows one attempt in flight per window; if it returns `None` the hub answers
//!      `PairResult{ok: false}`. On success both sides add each other to their
//!      [`TrustStore`].
//!    - If the hub does not trust the sender and the sender's next message is not
//!      `PairStart`, the hub sends `Bye` and fails with
//!      [`crate::CoreError::PairingRequired`].
//!    - If neither side needs pairing, the channel is ready right after the `Hello`s.
//! 4. Afterwards every Noise transport message is prefixed with its length as `u16` BE and
//!    carries exactly one [`hfa_proto::encode_frame`] payload.
//!
//! Concurrency: [`ControlChannel::recv`] is **cancel-safe**, so one task can multiplex a
//! channel with `tokio::select! { m = ch.recv() => .., _ = tick.tick() => ch.send(..).await }`.
//! [`ControlChannel::send`] is not cancel-safe: await it to completion (in a `select!` branch
//! *body*, never as a branch future).

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
    /// Raw bytes read from the socket that do not form a complete `u16`-prefixed Noise
    /// message yet. Keeping them here (filled with the cancel-safe `AsyncReadExt::read_buf`)
    /// is what makes [`ControlChannel::recv`] cancel-safe.
    rx: Vec<u8>,
}

impl ControlChannel {
    /// Connects to a hub as sender (Noise initiator), following the module-level procedure.
    ///
    /// - `expected_hub_key`: if `Some`, the hub's Noise static key must equal it (from
    ///   `PairingUri::hub_id`, or a trusted peer the caller resolved), else
    ///   [`crate::CoreError::KeyMismatch`].
    /// - `pairing_secret` (PIN or token, see [`crate::pairing::method_for_secret`]) is used
    ///   whenever pairing is needed: the sender does not trust the hub's key, or the hub does
    ///   not trust the sender.
    ///
    /// # Errors
    /// I/O, handshake, [`crate::CoreError::PairingRequired`] (pairing needed but no secret),
    /// [`crate::CoreError::PairingFailed`], [`crate::CoreError::KeyMismatch`],
    /// [`crate::CoreError::Timeout`] (whole procedure bounded by [`HANDSHAKE_TIMEOUT`]).
    pub async fn connect(
        _addr: SocketAddr,
        _identity: &Identity,
        _trust: &TrustStore,
        _expected_hub_key: Option<[u8; 32]>,
        _pairing_secret: Option<String>,
    ) -> Result<(ControlChannel, PeerInfo)> {
        todo!("feat/core-engine")
    }

    /// Accepts a sender on an incoming TCP stream as hub (Noise responder), following the
    /// module-level procedure. Pairing secrets come only from
    /// [`PairingManager::begin_attempt`] (never compare passwords directly).
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

    /// Sends one message. **Not cancel-safe**: dropping the future mid-write can leave a
    /// partial record on the socket, so always await it to completion.
    ///
    /// # Errors
    /// I/O or Noise errors; [`crate::CoreError::Closed`].
    pub async fn send(&mut self, _msg: &ControlMessage) -> Result<()> {
        let _ = (&self.stream, &self.transport, &self.decoder, &self.rx);
        todo!("feat/core-engine")
    }

    /// Receives the next message.
    ///
    /// **Cancel-safe**: it only reads with `AsyncReadExt::read_buf` into the internal buffer
    /// and decrypts/consumes a Noise message once its complete `u16`-prefixed record is
    /// buffered, so dropping the future (e.g. when another `tokio::select!` branch wins)
    /// loses no bytes and never desynchronizes the Noise receive nonce.
    ///
    /// # Errors
    /// I/O or Noise errors; [`crate::CoreError::Closed`] on EOF.
    pub async fn recv(&mut self) -> Result<ControlMessage> {
        todo!("feat/core-engine")
    }
}
