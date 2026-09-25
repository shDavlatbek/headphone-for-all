//! The control channel: TCP + Noise XX, then length-prefixed [`ControlMessage`] frames.
//!
//! # Wire procedure
//!
//! 1. **Noise XX handshake** ([`hfa_proto::noise`], prologue `hfa-v0 control`, empty
//!    handshake payloads); each handshake message is a *record*: its length as `u16` BE, then
//!    the message. The sender (TCP client) is the initiator. XX gives the initiator the hub's
//!    static key in message 2: if the sender was given an expected hub key (pairing URI,
//!    trusted peer) or an expected hub device id (a hub looked up by id over mDNS, see
//!    [`ControlChannel::connect_checked`]), it compares it right there and fails with
//!    [`crate::CoreError::KeyMismatch`] **before sending anything else** (so a wrong hub
//!    never even learns the sender's static key, `Hello` or a pairing attempt).
//! 2. **`Hello` exchange** over the transport: the sender sends its `Hello` first, the hub
//!    answers with its own. Both check `protocol_version == 0`, the peer's `role` and that
//!    `device_id` is the fingerprint of the peer's authenticated static key
//!    ([`crate::CoreError::Protocol`] otherwise).
//!    - The **sender** sets `Hello.pairing_required = true` iff it does **not** trust the
//!      hub's static key (it knows the key after step 1).
//!    - The **hub** sets `Hello.pairing_required = true` iff it does not trust the sender's
//!      key **or** the sender's `Hello` asked for pairing, so both sides reach the same
//!      decision without an extra round trip.
//! 3. **Pairing** is needed iff the sender does not trust the hub's key **or** the hub's
//!    `Hello` says `pairing_required`. The sender's own check is mandatory whatever the hub
//!    signals: a sender never streams to an untrusted, unpaired hub (a rogue hub advertising
//!    the same name would otherwise receive the audio).
//!    - Pairing needed and no secret: the sender sends `Bye` and fails with
//!      [`crate::CoreError::PairingRequired`]; the hub then fails with `PairingRequired` too.
//!    - Otherwise, SPAKE2 ([`hfa_proto::pairing`]) bound to the Noise handshake hash:
//!      ```text
//!      sender                                   hub
//!        PairStart{method}          ─────▶      begin_attempt(method)
//!                                   ◀─────      PairSpake{hub msg}   (or PairResult{ok:false})
//!        PairSpake{sender msg}      ─────▶
//!        PairConfirm{sender mac}    ─────▶      verify sender mac, attempt.succeed(),
//!                                               trust sender
//!                                   ◀─────      PairConfirm{hub mac} + PairResult{ok:true}
//!        verify hub mac, trust hub                (or PairResult{ok:false, reason})
//!      ```
//!      The hub obtains the secret only through [`PairingManager::begin_attempt`], which
//!      counts the attempt *before* SPAKE2 runs and allows one attempt in flight per window;
//!      if it returns `None` (no window, expired, budget used, another attempt in flight)
//!      the hub answers `PairResult{ok: false}`. After the sender's MAC verified, the hub
//!      commits the attempt with [`crate::pairing::PairingAttempt::succeed`], which consumes
//!      the one-time secret and fails if the window was cancelled, replaced or expired
//!      meanwhile (then `PairResult{ok: false}` and nobody is trusted). The sender waits for the hub's `PairSpake`
//!      before sending its own, so a rejected `PairStart` never leaves unread data behind
//!      (which could turn the hub's close into a TCP reset that swallows the reason). On
//!      success both sides add each other to their [`TrustStore`].
//!    - If the hub needs pairing and the sender's next message is not `PairStart`, the hub
//!      sends `Bye` and fails with [`crate::CoreError::PairingRequired`].
//!    - If neither side needs pairing, the channel is ready right after the `Hello`s.
//! 4. Afterwards every Noise transport message is a record (`u16` BE length + ciphertext)
//!    carrying exactly one [`hfa_proto::encode_frame`] payload.
//!
//! Steps 1–3 (including the TCP connect) are bounded by [`HANDSHAKE_TIMEOUT`] as a whole,
//! so every single step is bounded too ([`crate::CoreError::Timeout`]), except the
//! trust-store save after a successful pairing: once the secret is consumed and the save has
//! started it always completes (a deadline in the middle would leave the peer trusted on
//! disk while the call reported a failure); the hub's two confirmation messages after it
//! get the remaining time, but at least 2 s. During steps 1–3 any undecodable frame is fatal
//! ([`crate::CoreError::Protocol`]). Every failure is a
//! typed [`crate::CoreError`]: I/O → `Io`, peer closed → `Closed`, Noise/decoding →
//! `Proto`, unexpected message → `Protocol`, wrong secret or refused attempt →
//! `PairingFailed`, missing secret → `PairingRequired`, pinned key differs → `KeyMismatch`.
//!
//! # Using a channel from one task
//!
//! [`ControlChannel::recv`] is **cancel-safe**: it only reads with
//! `AsyncReadExt::read_buf` into a private buffer and decrypts a record once it is complete,
//! so a `recv` future dropped half-way (because another `tokio::select!` branch won) loses no
//! bytes and never desynchronizes the Noise nonce. [`ControlChannel::send`] is **not**
//! cancel-safe: await it to completion inside a branch *body*, never as a branch future. An
//! engine therefore owns the channel in a single task and multiplexes like this:
//!
//! ```no_run
//! # use hfa_core::control::ControlChannel;
//! # use hfa_proto::control::{Body, Ping, Pong};
//! # use hfa_proto::ControlMessage;
//! # async fn run(mut ch: ControlChannel, mut commands: tokio::sync::mpsc::Receiver<ControlMessage>)
//! #     -> hfa_core::Result<()> {
//! let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
//! loop {
//!     tokio::select! {
//!         msg = ch.recv() => match msg?.body {
//!             Some(Body::Ping(p)) => {
//!                 ch.send(&Body::Pong(Pong { nonce: p.nonce, t_us: p.t_us }).into()).await?
//!             }
//!             Some(Body::Bye(_)) => return Ok(()),
//!             _ => { /* handle the message */ }
//!         },
//!         _ = tick.tick() => ch.send(&Body::Ping(Ping { nonce: 1, t_us: 0 }).into()).await?,
//!         cmd = commands.recv() => match cmd {
//!             Some(msg) => ch.send(&msg).await?,
//!             None => return ch.close("stopped").await,
//!         },
//!     }
//! }
//! # }
//! ```
//!
//! Every error returned by `send` (except an unencodable message, which writes nothing) or
//! `recv` is fatal: the channel is then closed and later calls return
//! [`crate::CoreError::Closed`] (or the original error). A `send` future that was dropped
//! mid-write also closes the channel (the next call returns `Closed`), because the peer
//! would see a torn record.

use std::net::SocketAddr;
use std::time::Duration;

use hfa_proto::control::{
    Body, Bye, Hello, PairConfirm, PairMethod, PairResult, PairSpake, PairStart, Role,
};
use hfa_proto::{
    ControlMessage, FrameDecoder, NoiseHandshake, NoiseTransport, PairingKey, PairingRole,
    PairingSession,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

use crate::identity::{Identity, PeerRole, TrustStore, TrustedPeer};
use crate::pairing::{method_for_secret, PairingManager};
use crate::{CoreError, Result};

/// Timeout for the complete connect + handshake + hello + pairing phase.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Length of the record length prefix (`u16` BE) in front of every Noise message.
pub const RECORD_PREFIX_LEN: usize = 2;
/// How long [`ControlChannel::close`] waits for the `Bye` to be written.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Read granularity of the receive buffer.
const READ_CHUNK: usize = 16 * 1024;
/// Longest `platform` / `app_version` string accepted from a peer's `Hello` (longer values
/// are truncated).
const MAX_HELLO_FIELD: usize = 64;
/// After the handshake, how many undecodable frames in a row [`ControlChannel::recv`] skips
/// before it closes the channel ([`crate::CoreError::Protocol`]).
pub const MAX_SKIPPED_FRAMES: u32 = 64;
/// Minimum time between two log lines about skipped control frames.
const SKIP_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// What we learned about the peer during the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// Fingerprint of the peer's static key.
    pub device_id: String,
    /// Peer display name (from `Hello`, sanitized; the device id if it was empty).
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

/// A TCP stream carrying `u16`-length-prefixed records, with a cancel-safe reader.
struct RecordStream {
    stream: TcpStream,
    /// Bytes read from the socket; `rx[pos..]` do not form a complete record yet. Filled
    /// only with the cancel-safe `read_buf`, consumed only synchronously.
    rx: Vec<u8>,
    /// Start of the unconsumed bytes in `rx`. Consuming a record only moves this offset;
    /// the consumed prefix is dropped once per socket read (never once per record, which
    /// would make many small records in one read quadratic).
    pos: usize,
}

impl RecordStream {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            rx: Vec::with_capacity(READ_CHUNK),
            pos: 0,
        }
    }

    /// Removes and returns the first complete record from the buffer, if any.
    fn take_record(&mut self) -> Option<Vec<u8>> {
        let pending = self.rx.get(self.pos..)?;
        let (prefix, rest) = pending.split_first_chunk::<RECORD_PREFIX_LEN>()?;
        let len = usize::from(u16::from_be_bytes(*prefix));
        let record = rest.get(..len)?.to_vec();
        self.pos += RECORD_PREFIX_LEN + len;
        if self.pos == self.rx.len() {
            self.rx.clear();
            self.pos = 0;
        }
        Some(record)
    }

    /// Reads the next record. **Cancel-safe** (see the module docs).
    async fn read_record(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(record) = self.take_record() {
                return Ok(record);
            }
            if self.pos > 0 {
                // Compact before reading more (synchronous, so still cancel-safe).
                self.rx.drain(..self.pos);
                self.pos = 0;
            }
            self.rx.reserve(READ_CHUNK);
            let n = self.stream.read_buf(&mut self.rx).await?;
            if n == 0 {
                if !self.rx.is_empty() {
                    tracing::debug!(
                        buffered = self.rx.len(),
                        "control connection closed mid-record"
                    );
                }
                return Err(CoreError::Closed);
            }
        }
    }

    /// Writes one record. Not cancel-safe.
    async fn write_record(&mut self, payload: &[u8]) -> Result<()> {
        let len = u16::try_from(payload.len()).map_err(|_| {
            CoreError::Proto(hfa_proto::ProtoError::FrameTooLarge {
                len: payload.len(),
                max: usize::from(u16::MAX),
            })
        })?;
        let mut buf = Vec::with_capacity(RECORD_PREFIX_LEN + payload.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(payload);
        self.stream.write_all(&buf).await?;
        Ok(())
    }
}

/// An established, authenticated control channel.
pub struct ControlChannel {
    io: RecordStream,
    transport: NoiseTransport,
    decoder: FrameDecoder,
    peer_addr: SocketAddr,
    /// Set once the channel failed; returned by every later call.
    failed: Option<CoreError>,
    /// `true` while a `send` is writing; still `true` at the next call if that `send` was
    /// cancelled mid-write.
    writing: bool,
    /// `true` until `connect`/`accept` hand the channel out: during the handshake every
    /// undecodable frame is fatal.
    strict: bool,
    /// Undecodable frames skipped since the last decodable one.
    skipped_in_a_row: u32,
    /// Skipped frames not yet reported in the log.
    skipped_unlogged: u64,
    /// When a skipped frame was last logged.
    last_skip_log: Option<std::time::Instant>,
}

impl std::fmt::Debug for ControlChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlChannel")
            .field("peer_addr", &self.peer_addr)
            .field(
                "remote_static",
                &hfa_proto::fingerprint(&self.remote_static()),
            )
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl ControlChannel {
    fn new(io: RecordStream, transport: NoiseTransport, peer_addr: SocketAddr) -> Self {
        Self {
            io,
            transport,
            decoder: FrameDecoder::new(),
            peer_addr,
            failed: None,
            writing: false,
            strict: true,
            skipped_in_a_row: 0,
            skipped_unlogged: 0,
            last_skip_log: None,
        }
    }

    /// Connects to a hub as sender (Noise initiator), following the module-level procedure.
    ///
    /// - `expected_hub_key`: if `Some`, the hub's Noise static key must equal it (from
    ///   `PairingUri::hub_id`, or a trusted peer the caller resolved), else
    ///   [`crate::CoreError::KeyMismatch`] (checked before the sender reveals its own key).
    /// - `pairing_secret` (PIN or token, see [`crate::pairing::method_for_secret`]) is used
    ///   whenever pairing is needed: the sender does not trust the hub's key, or the hub does
    ///   not trust the sender. On success the hub is added to `trust`.
    ///
    /// # Errors
    /// I/O, handshake, [`crate::CoreError::PairingRequired`] (pairing needed but no secret),
    /// [`crate::CoreError::PairingFailed`], [`crate::CoreError::KeyMismatch`],
    /// [`crate::CoreError::Protocol`], [`crate::CoreError::Closed`],
    /// [`crate::CoreError::Timeout`] (whole procedure bounded by [`HANDSHAKE_TIMEOUT`]).
    pub async fn connect(
        addr: SocketAddr,
        identity: &Identity,
        trust: &TrustStore,
        expected_hub_key: Option<[u8; 32]>,
        pairing_secret: Option<String>,
    ) -> Result<(ControlChannel, PeerInfo)> {
        Self::connect_checked(
            addr,
            identity,
            trust,
            expected_hub_key,
            None,
            pairing_secret,
        )
        .await
    }

    /// [`ControlChannel::connect`], and if `expected_hub_id` is `Some`, the fingerprint of the
    /// hub's static key must equal it (e.g. a hub looked up by device id over mDNS, whose
    /// address is only a hint). Like `expected_hub_key`, it is checked right after Noise
    /// message 2, so a hub with another key gets neither the sender's static key, nor its
    /// `Hello`, nor a pairing attempt with the secret: [`crate::CoreError::KeyMismatch`]
    /// with the device id of the key that answered.
    ///
    /// # Errors
    /// As [`ControlChannel::connect`].
    pub async fn connect_checked(
        addr: SocketAddr,
        identity: &Identity,
        trust: &TrustStore,
        expected_hub_key: Option<[u8; 32]>,
        expected_hub_id: Option<&str>,
        pairing_secret: Option<String>,
    ) -> Result<(ControlChannel, PeerInfo)> {
        let secret = pairing_secret.map(Zeroizing::new);
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        let expected = Expected {
            key: expected_hub_key,
            id: expected_hub_id,
        };
        let result = connect_procedure(
            addr,
            identity,
            trust,
            expected,
            secret.as_ref().map(|s| s.as_str()),
            deadline,
        )
        .await;
        if let Err(e) = &result {
            tracing::debug!(%addr, error = %e, "control connect failed");
        }
        result
    }

    /// Accepts a sender on an incoming TCP stream as hub (Noise responder), following the
    /// module-level procedure. Pairing secrets come only from
    /// [`PairingManager::begin_attempt`] (never compare passwords directly). On a successful
    /// pairing the sender is added to `trust`.
    ///
    /// # Errors
    /// I/O, handshake, [`crate::CoreError::PairingRequired`],
    /// [`crate::CoreError::PairingFailed`], [`crate::CoreError::Protocol`],
    /// [`crate::CoreError::Closed`], [`crate::CoreError::Timeout`].
    pub async fn accept(
        stream: TcpStream,
        identity: &Identity,
        trust: &TrustStore,
        pairing: &PairingManager,
    ) -> Result<(ControlChannel, PeerInfo)> {
        let addr = stream.peer_addr()?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        let result = accept_procedure(stream, addr, identity, trust, pairing, deadline).await;
        if let Err(e) = &result {
            tracing::debug!(%addr, error = %e, "control accept failed");
        }
        result
    }

    /// Sends one message. **Not cancel-safe**: dropping the future mid-write leaves a torn
    /// record on the socket, so the channel is then treated as closed; always await it to
    /// completion.
    ///
    /// # Errors
    /// [`crate::CoreError::Proto`] if the message cannot be encoded (empty body, too large;
    /// nothing is sent and the channel stays usable); I/O or Noise errors (fatal);
    /// [`crate::CoreError::Closed`] after an earlier fatal error.
    pub async fn send(&mut self, msg: &ControlMessage) -> Result<()> {
        self.check_usable()?;
        let frame = hfa_proto::encode_frame(msg)?;
        let record = match self.transport.encrypt(&frame) {
            Ok(r) => r,
            Err(e) => return Err(self.fail(e.into())),
        };
        self.writing = true;
        if let Err(e) = self.io.write_record(&record).await {
            self.writing = false;
            return Err(self.fail(e));
        }
        self.writing = false;
        Ok(())
    }

    /// Receives the next message.
    ///
    /// **Cancel-safe**: it only reads with `AsyncReadExt::read_buf` into the internal buffer
    /// and decrypts/consumes a Noise message once its complete `u16`-prefixed record is
    /// buffered, so dropping the future (e.g. when another `tokio::select!` branch wins)
    /// loses no bytes and never desynchronizes the Noise receive nonce.
    ///
    /// A frame that decodes to no known message (e.g. a variant added by a newer peer, or
    /// malformed protobuf) is skipped; skips are logged at most once per 5 s (with a count),
    /// and more than [`MAX_SKIPPED_FRAMES`] of them in a row close the channel
    /// ([`crate::CoreError::Protocol`]), so a peer cannot flood the log or keep the task busy
    /// with garbage. During `connect`/`accept` (before the channel is handed out) an
    /// undecodable frame is fatal right away. A transport record with an empty plaintext
    /// (never produced by this implementation) is fatal too. A frame length above
    /// `MAX_CONTROL_FRAME` poisons the frame decoder; the channel is then closed and the
    /// error returned.
    ///
    /// # Errors
    /// I/O or Noise errors, [`crate::CoreError::Proto`] for a poisoned framing,
    /// [`crate::CoreError::Protocol`] for too many undecodable frames or an empty record,
    /// [`crate::CoreError::Closed`] on EOF or after an earlier fatal error. All are fatal.
    pub async fn recv(&mut self) -> Result<ControlMessage> {
        loop {
            self.check_usable()?;
            while let Some(item) = self.decoder.next() {
                match item {
                    Ok(msg) => {
                        self.skipped_in_a_row = 0;
                        return Ok(msg);
                    }
                    Err(e) if self.decoder.is_poisoned() => return Err(self.fail(e.into())),
                    Err(e) => self.skip_frame(&e)?,
                }
            }
            let record = match self.io.read_record().await {
                Ok(r) => r,
                Err(e) => return Err(self.fail(e)),
            };
            match self.transport.decrypt(&record) {
                Ok(plain) if plain.is_empty() => {
                    return Err(self.fail(CoreError::Protocol("empty control record".into())))
                }
                Ok(plain) => self.decoder.push(&plain),
                Err(e) => return Err(self.fail(e.into())),
            }
        }
    }

    /// Handles one undecodable (but correctly framed) frame: fatal while `strict` or past
    /// the [`MAX_SKIPPED_FRAMES`] limit, otherwise skipped with a rate-limited log line.
    fn skip_frame(&mut self, err: &hfa_proto::ProtoError) -> Result<()> {
        if self.strict {
            return Err(self.fail(CoreError::Protocol(format!(
                "undecodable control frame during the handshake: {err}"
            ))));
        }
        self.skipped_in_a_row += 1;
        if self.skipped_in_a_row > MAX_SKIPPED_FRAMES {
            return Err(self.fail(CoreError::Protocol(format!(
                "more than {MAX_SKIPPED_FRAMES} undecodable control frames in a row"
            ))));
        }
        self.skipped_unlogged += 1;
        let now = std::time::Instant::now();
        if self
            .last_skip_log
            .is_none_or(|last| now.duration_since(last) >= SKIP_LOG_INTERVAL)
        {
            tracing::warn!(
                peer = %self.peer_addr,
                error = %err,
                skipped = self.skipped_unlogged,
                "skipping undecodable control frames"
            );
            self.skipped_unlogged = 0;
            self.last_skip_log = Some(now);
        }
        Ok(())
    }

    /// Sends `Bye{reason}` (best effort, at most 2 s) and shuts the connection down.
    ///
    /// # Errors
    /// Never fails in practice; the `Result` only matches the engines' `?` style.
    pub async fn close(mut self, reason: &str) -> Result<()> {
        let bye = ControlMessage::new(Body::Bye(Bye {
            reason: reason.to_owned(),
        }));
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
            if self.send(&bye).await.is_ok() {
                let _ = self.io.stream.shutdown().await;
            }
        })
        .await;
        Ok(())
    }

    /// The peer's socket address.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// The peer's authenticated Noise static public key.
    pub fn remote_static(&self) -> [u8; 32] {
        self.transport.remote_static()
    }

    /// The Noise handshake hash of this session (unique per connection).
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.transport.handshake_hash()
    }

    /// `true` once the channel failed or was closed by the peer.
    pub fn is_closed(&self) -> bool {
        self.failed.is_some() || self.writing
    }

    fn check_usable(&mut self) -> Result<()> {
        if self.writing {
            // A previous `send` was cancelled mid-write: the stream holds a torn record.
            self.writing = false;
            self.failed = Some(CoreError::Closed);
        }
        match &self.failed {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// Records a fatal error: every later call returns it.
    fn fail(&mut self, err: CoreError) -> CoreError {
        if self.failed.is_none() {
            self.failed = Some(err.clone());
        }
        err
    }

    /// Sends `Bye{reason}`, ignoring errors (used on handshake failure paths).
    async fn send_bye(&mut self, reason: &str) {
        let bye = ControlMessage::new(Body::Bye(Bye {
            reason: reason.to_owned(),
        }));
        let _ = self.send(&bye).await;
    }

    /// Sends `PairResult{ok: false, reason}`, ignoring errors.
    async fn send_pair_failure(&mut self, reason: &str) {
        let msg = ControlMessage::new(Body::PairResult(PairResult {
            ok: false,
            reason: reason.to_owned(),
        }));
        let _ = self.send(&msg).await;
    }
}

/// Runs `fut` until `deadline`, mapping expiry to [`CoreError::Timeout`].
async fn within<T>(
    deadline: tokio::time::Instant,
    addr: SocketAddr,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout_at(deadline, fut)
        .await
        .unwrap_or_else(|_| {
            Err(CoreError::Timeout(format!(
                "control handshake with {addr} took longer than {HANDSHAKE_TIMEOUT:?}"
            )))
        })
}

/// Deadline for the short steps after a trust-store save, which is not bounded by the
/// handshake deadline: the remaining handshake time, but at least [`CLOSE_TIMEOUT`].
fn after_save_deadline(deadline: tokio::time::Instant) -> tokio::time::Instant {
    deadline.max(tokio::time::Instant::now() + CLOSE_TIMEOUT)
}

/// The sender side of the procedure: [`connect_inner`] within `deadline`, then the trust
/// save (see [`accept_procedure`] for why the save is outside the deadline).
async fn connect_procedure(
    addr: SocketAddr,
    identity: &Identity,
    trust: &TrustStore,
    expected: Expected<'_>,
    secret: Option<&str>,
    deadline: tokio::time::Instant,
) -> Result<(ControlChannel, PeerInfo)> {
    let (mut ch, mut peer, paired) = within(
        deadline,
        addr,
        connect_inner(addr, identity, trust, expected, secret),
    )
    .await?;
    if paired {
        if let Err(e) = add_trusted(
            trust,
            TrustedPeer::paired_as(peer.public_key, peer.name.clone(), PeerRole::Hub),
        )
        .await
        {
            let _ = within(after_save_deadline(deadline), addr, async {
                ch.send_bye("the sender could not save the pairing").await;
                Ok(())
            })
            .await;
            return Err(e);
        }
        peer.newly_paired = true;
        tracing::info!(hub = %peer.device_id, name = %peer.name, "paired with hub");
    }
    ch.strict = false;
    Ok((ch, peer))
}

/// The hub side of the procedure: [`accept_inner`] within `deadline`, then — after a
/// successful pairing — the trust save and the hub's confirmation.
///
/// The save deliberately runs **outside** the deadline: it starts only after
/// [`crate::pairing::PairingAttempt::succeed`] consumed the one-time secret, and a
/// `spawn_blocking` write cannot be stopped anyway, so cancelling it at the deadline would
/// leave a peer trusted on disk while `accept` reported a failure. The two confirmation
/// messages that follow get the remaining time (at least [`CLOSE_TIMEOUT`]). If they cannot
/// be delivered, the sender (which proved the secret) stays trusted on the hub, and `accept`
/// returns the error; the sender then does not trust the hub and pairs again next time.
async fn accept_procedure(
    stream: TcpStream,
    addr: SocketAddr,
    identity: &Identity,
    trust: &TrustStore,
    pairing: &PairingManager,
    deadline: tokio::time::Instant,
) -> Result<(ControlChannel, PeerInfo)> {
    let (mut ch, mut peer, paired) = within(
        deadline,
        addr,
        accept_inner(stream, addr, identity, trust, pairing),
    )
    .await?;
    if let Some(key) = paired {
        if let Err(e) = add_trusted(
            trust,
            TrustedPeer::paired_as(peer.public_key, peer.name.clone(), PeerRole::Sender),
        )
        .await
        {
            let _ = within(after_save_deadline(deadline), addr, async {
                ch.send_pair_failure("the hub could not save the pairing")
                    .await;
                Ok(())
            })
            .await;
            return Err(e);
        }
        within(
            after_save_deadline(deadline),
            addr,
            hub_confirm(&mut ch, &key),
        )
        .await?;
        peer.newly_paired = true;
        tracing::info!(sender = %peer.device_id, name = %peer.name, "paired with sender");
    }
    ch.strict = false;
    Ok((ch, peer))
}

/// What the sender requires of the hub's static key (checked after Noise message 2).
#[derive(Clone, Copy)]
struct Expected<'a> {
    key: Option<[u8; 32]>,
    id: Option<&'a str>,
}

impl Expected<'_> {
    fn check(self, hub_key: &[u8; 32]) -> Result<()> {
        let id_ok = self
            .id
            .is_none_or(|id| id.trim() == hfa_proto::fingerprint(hub_key));
        if self.key.is_some_and(|k| k != *hub_key) || !id_ok {
            return Err(CoreError::KeyMismatch(hfa_proto::fingerprint(hub_key)));
        }
        Ok(())
    }
}

/// The sender side up to (not including) the trust save. Returns `true` as the third
/// element if pairing succeeded and the hub must be saved.
async fn connect_inner(
    addr: SocketAddr,
    identity: &Identity,
    trust: &TrustStore,
    expected: Expected<'_>,
    secret: Option<&str>,
) -> Result<(ControlChannel, PeerInfo, bool)> {
    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    let peer_addr = stream.peer_addr().unwrap_or(addr);
    let mut io = RecordStream::new(stream);

    // Noise XX: -> e ; <- e, ee, s, es ; -> s, se
    let mut hs = NoiseHandshake::initiator(&identity.keypair)?;
    io.write_record(&hs.write_message(&[])?).await?;
    hs.read_message(&io.read_record().await?)?;
    let hub_key = hs
        .remote_static()
        .ok_or_else(|| CoreError::Protocol("hub sent no static key".into()))?;
    expected.check(&hub_key)?;
    io.write_record(&hs.write_message(&[])?).await?;
    let mut ch = ControlChannel::new(io, hs.into_transport()?, peer_addr);

    let trusts_hub = trust.is_trusted_as(&hub_key, PeerRole::Hub);
    ch.send(&hello(identity, Role::Sender, !trusts_hub)).await?;
    let hub_hello = match check_hello(ch.recv().await?, Role::Hub, &hub_key) {
        Ok(h) => h,
        Err(e) => {
            ch.send_bye("invalid hello").await;
            return Err(e);
        }
    };
    let peer = peer_info(&hub_hello, hub_key, peer_addr);

    if !trusts_hub || hub_hello.pairing_required {
        let Some(secret) = secret else {
            ch.send_bye("pairing required").await;
            return Err(CoreError::PairingRequired);
        };
        sender_pair(&mut ch, secret).await?;
        return Ok((ch, peer, true));
    }
    Ok((ch, peer, false))
}

/// The hub side up to (not including) the trust save. Returns the pairing key as the third
/// element if the sender proved the secret and the window committed the attempt.
async fn accept_inner(
    stream: TcpStream,
    addr: SocketAddr,
    identity: &Identity,
    trust: &TrustStore,
    pairing: &PairingManager,
) -> Result<(ControlChannel, PeerInfo, Option<PairingKey>)> {
    stream.set_nodelay(true)?;
    let mut io = RecordStream::new(stream);

    let mut hs = NoiseHandshake::responder(&identity.keypair)?;
    hs.read_message(&io.read_record().await?)?;
    io.write_record(&hs.write_message(&[])?).await?;
    hs.read_message(&io.read_record().await?)?;
    let transport = hs.into_transport()?;
    let sender_key = transport.remote_static();
    let mut ch = ControlChannel::new(io, transport, addr);

    let sender_hello = match check_hello(ch.recv().await?, Role::Sender, &sender_key) {
        Ok(h) => h,
        Err(e) => {
            ch.send_bye("invalid hello").await;
            return Err(e);
        }
    };
    let peer = peer_info(&sender_hello, sender_key, addr);
    let needs_pairing =
        !trust.is_trusted_as(&sender_key, PeerRole::Sender) || sender_hello.pairing_required;
    ch.send(&hello(identity, Role::Hub, needs_pairing)).await?;

    if needs_pairing {
        match ch.recv().await?.body {
            Some(Body::PairStart(start)) => {
                let key = hub_pair(&mut ch, start, pairing).await?;
                return Ok((ch, peer, Some(key)));
            }
            Some(Body::Bye(_)) => return Err(CoreError::PairingRequired),
            other => {
                tracing::debug!(
                    got = body_name(other.as_ref()),
                    "untrusted sender did not pair"
                );
                ch.send_bye("pairing required").await;
                return Err(CoreError::PairingRequired);
            }
        }
    }
    Ok((ch, peer, None))
}

/// Sender side of SPAKE2 pairing (see the module docs for the message order).
async fn sender_pair(ch: &mut ControlChannel, secret: &str) -> Result<()> {
    let method = method_for_secret(secret);
    ch.send(
        &Body::PairStart(PairStart {
            method: method as i32,
        })
        .into(),
    )
    .await?;
    let hub_msg = match ch.recv().await?.body {
        Some(Body::PairSpake(s)) => s.msg,
        other => return Err(pairing_reply_error(other, "PairSpake")),
    };
    let (session, own_msg) = PairingSession::start(secret, &ch.handshake_hash());
    ch.send(&Body::PairSpake(PairSpake { msg: own_msg }).into())
        .await?;
    let key = match session.finish(&hub_msg) {
        Ok(k) => k,
        Err(e) => {
            ch.send_bye("pairing failed").await;
            return Err(CoreError::PairingFailed(e.to_string()));
        }
    };
    ch.send(
        &Body::PairConfirm(PairConfirm {
            mac: key.confirm_mac(PairingRole::Sender).to_vec(),
        })
        .into(),
    )
    .await?;
    match ch.recv().await?.body {
        Some(Body::PairConfirm(c)) => {
            if key.verify(PairingRole::Hub, &c.mac).is_err() {
                ch.send_bye("hub confirmation invalid").await;
                return Err(CoreError::PairingFailed(
                    "the hub could not prove it knows the PIN/token".into(),
                ));
            }
        }
        other => return Err(pairing_reply_error(other, "PairConfirm")),
    }
    match ch.recv().await?.body {
        Some(Body::PairResult(r)) if r.ok => Ok(()),
        other => Err(pairing_reply_error(other, "PairResult")),
    }
}

/// Hub side of SPAKE2 pairing, after the sender's `PairStart`, up to the verified sender
/// MAC and the committed attempt. Returns the key for [`hub_confirm`].
async fn hub_pair(
    ch: &mut ControlChannel,
    start: PairStart,
    pairing: &PairingManager,
) -> Result<PairingKey> {
    let method = PairMethod::try_from(start.method).unwrap_or(PairMethod::Unspecified);
    if method == PairMethod::Unspecified {
        ch.send_pair_failure("invalid pairing method").await;
        return Err(CoreError::Protocol(format!(
            "invalid pairing method {}",
            start.method
        )));
    }
    let Some(attempt) = pairing.begin_attempt(method) else {
        const REASON: &str =
            "pairing is not open on the hub (no window, expired, too many attempts or another pairing in progress)";
        ch.send_pair_failure(REASON).await;
        return Err(CoreError::PairingFailed(REASON.into()));
    };
    let (session, own_msg) = PairingSession::start(attempt.secret(), &ch.handshake_hash());
    ch.send(&Body::PairSpake(PairSpake { msg: own_msg }).into())
        .await?;
    let sender_msg = match ch.recv().await?.body {
        Some(Body::PairSpake(s)) => s.msg,
        other => return Err(pairing_reply_error(other, "PairSpake")),
    };
    let key = match session.finish(&sender_msg) {
        Ok(k) => k,
        Err(e) => {
            ch.send_pair_failure("invalid SPAKE2 message").await;
            return Err(CoreError::PairingFailed(e.to_string()));
        }
    };
    let mac = match ch.recv().await?.body {
        Some(Body::PairConfirm(c)) => c.mac,
        other => return Err(pairing_reply_error(other, "PairConfirm")),
    };
    if key.verify(PairingRole::Sender, &mac).is_err() {
        // Dropping `attempt` records the failed guess.
        ch.send_pair_failure("wrong PIN or token").await;
        return Err(CoreError::PairingFailed("wrong PIN or token".into()));
    }
    // Atomically consume the one-time secret, unless the user cancelled or replaced the
    // window (or it expired) while this attempt was running.
    if !attempt.succeed() {
        const REASON: &str = "pairing was cancelled on the hub";
        ch.send_pair_failure(REASON).await;
        return Err(CoreError::PairingFailed(REASON.into()));
    }
    Ok(key)
}

/// The hub's confirmation after the sender was saved: `PairConfirm{hub mac}` +
/// `PairResult{ok: true}`.
async fn hub_confirm(ch: &mut ControlChannel, key: &PairingKey) -> Result<()> {
    ch.send(
        &Body::PairConfirm(PairConfirm {
            mac: key.confirm_mac(PairingRole::Hub).to_vec(),
        })
        .into(),
    )
    .await?;
    ch.send(
        &Body::PairResult(PairResult {
            ok: true,
            reason: String::new(),
        })
        .into(),
    )
    .await
}

/// Maps an unexpected reply during pairing to an error.
fn pairing_reply_error(body: Option<Body>, expected: &str) -> CoreError {
    match body {
        Some(Body::PairResult(r)) if !r.ok => CoreError::PairingFailed(if r.reason.is_empty() {
            "rejected by the peer".into()
        } else {
            r.reason
        }),
        Some(Body::Bye(b)) => CoreError::PairingFailed(format!("peer aborted: {}", b.reason)),
        other => CoreError::Protocol(format!(
            "expected {expected} during pairing, got {}",
            body_name(other.as_ref())
        )),
    }
}

/// Saves a trusted peer without blocking the async executor.
async fn add_trusted(trust: &TrustStore, peer: TrustedPeer) -> Result<()> {
    let trust = trust.clone();
    tokio::task::spawn_blocking(move || trust.add(peer))
        .await
        .map_err(|e| CoreError::Io(format!("trust store task failed: {e}")))?
}

/// Our `Hello`.
fn hello(identity: &Identity, role: Role, pairing_required: bool) -> ControlMessage {
    ControlMessage::new(Body::Hello(Hello {
        protocol_version: u32::from(hfa_proto::PROTOCOL_VERSION),
        device_id: identity.device_id.clone(),
        device_name: identity.name.clone(),
        platform: crate::platform_name().to_owned(),
        app_version: crate::APP_VERSION.to_owned(),
        role: role as i32,
        pairing_required,
    }))
}

/// Validates the peer's `Hello`.
fn check_hello(msg: ControlMessage, role: Role, peer_key: &[u8; 32]) -> Result<Hello> {
    let hello = match msg.body {
        Some(Body::Hello(h)) => h,
        Some(Body::Bye(b)) => {
            return Err(CoreError::Protocol(format!(
                "peer closed the connection: {}",
                b.reason
            )))
        }
        other => {
            return Err(CoreError::Protocol(format!(
                "expected Hello, got {}",
                body_name(other.as_ref())
            )))
        }
    };
    if hello.protocol_version != u32::from(hfa_proto::PROTOCOL_VERSION) {
        return Err(CoreError::Protocol(format!(
            "unsupported protocol version {}",
            hello.protocol_version
        )));
    }
    if hello.role != role as i32 {
        return Err(CoreError::Protocol(format!(
            "expected a peer with role {role:?}, got role {}",
            hello.role
        )));
    }
    if hello.device_id != hfa_proto::fingerprint(peer_key) {
        return Err(CoreError::Protocol(
            "the peer's device id does not match its key".into(),
        ));
    }
    Ok(hello)
}

/// Builds [`PeerInfo`] from a validated `Hello`.
fn peer_info(hello: &Hello, public_key: [u8; 32], addr: SocketAddr) -> PeerInfo {
    let device_id = hfa_proto::fingerprint(&public_key);
    let name = hfa_proto::sanitize_name(hello.device_name.trim());
    PeerInfo {
        name: if name.trim().is_empty() {
            device_id.clone()
        } else {
            name
        },
        device_id,
        platform: short_field(&hello.platform),
        app_version: short_field(&hello.app_version),
        public_key,
        addr,
        newly_paired: false,
    }
}

/// Sanitizes a short free-form `Hello` field.
fn short_field(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_HELLO_FIELD)
        .collect()
}

/// Name of a message variant for error messages (never prints contents).
fn body_name(body: Option<&Body>) -> &'static str {
    match body {
        None => "an empty message",
        Some(Body::Hello(_)) => "Hello",
        Some(Body::PairStart(_)) => "PairStart",
        Some(Body::PairSpake(_)) => "PairSpake",
        Some(Body::PairConfirm(_)) => "PairConfirm",
        Some(Body::PairResult(_)) => "PairResult",
        Some(Body::StreamStart(_)) => "StreamStart",
        Some(Body::StreamAccepted(_)) => "StreamAccepted",
        Some(Body::StreamRejected(_)) => "StreamRejected",
        Some(Body::StreamStop(_)) => "StreamStop",
        Some(Body::SetVolume(_)) => "SetVolume",
        Some(Body::SetMute(_)) => "SetMute",
        Some(Body::SetPriority(_)) => "SetPriority",
        Some(Body::Ping(_)) => "Ping",
        Some(Body::Pong(_)) => "Pong",
        Some(Body::Stats(_)) => "Stats",
        Some(Body::Bye(_)) => "Bye",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_reader_handles_split_and_coalesced_records() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let writer = tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.expect("connect");
            // Record "abc" split in the middle of its prefix, then two records at once and
            // an empty record.
            s.write_all(&[0]).await.expect("w");
            s.flush().await.expect("f");
            tokio::time::sleep(Duration::from_millis(20)).await;
            s.write_all(&[3, b'a', b'b']).await.expect("w");
            tokio::time::sleep(Duration::from_millis(20)).await;
            s.write_all(&[b'c', 0, 1, b'x', 0, 2, b'y', b'z', 0, 0])
                .await
                .expect("w");
        });
        let (stream, _) = listener.accept().await.expect("accept");
        let mut rs = RecordStream::new(stream);
        assert_eq!(rs.read_record().await.expect("r1"), b"abc");
        assert_eq!(rs.read_record().await.expect("r2"), b"x");
        assert_eq!(rs.read_record().await.expect("r3"), b"yz");
        assert_eq!(rs.read_record().await.expect("r4"), b"");
        writer.await.expect("writer");
        assert!(matches!(rs.read_record().await, Err(CoreError::Closed)));
    }

    #[test]
    fn hello_validation() {
        let key = [3u8; 32];
        let ok = Hello {
            protocol_version: 0,
            device_id: hfa_proto::fingerprint(&key),
            device_name: "Desk\u{1b}[31m".into(),
            platform: "linux".into(),
            app_version: "0.1.0".into(),
            role: Role::Hub as i32,
            pairing_required: false,
        };
        let msg = |h: Hello| ControlMessage::new(Body::Hello(h));
        let checked = check_hello(msg(ok.clone()), Role::Hub, &key).expect("valid");
        let info = peer_info(&checked, key, "127.0.0.1:1".parse().expect("addr"));
        assert_eq!(info.name, "Desk[31m", "control characters are dropped");
        assert!(matches!(
            check_hello(msg(ok.clone()), Role::Sender, &key),
            Err(CoreError::Protocol(_))
        ));
        assert!(matches!(
            check_hello(msg(ok.clone()), Role::Hub, &[4u8; 32]),
            Err(CoreError::Protocol(_))
        ));
        let mut v1 = ok;
        v1.protocol_version = 1;
        assert!(matches!(
            check_hello(msg(v1), Role::Hub, &key),
            Err(CoreError::Protocol(_))
        ));
        assert!(matches!(
            check_hello(Body::Ping(Default::default()).into(), Role::Hub, &key),
            Err(CoreError::Protocol(_))
        ));
    }
}
