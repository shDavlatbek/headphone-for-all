//! UDP media helpers built on [`hfa_proto`] sealing.
//!
//! The sender owns one [`MediaSender`] per stream. The hub has one UDP socket (same port
//! number as the TCP control port) and one [`MediaDemux`] that routes datagrams to per-stream
//! openers by `stream_id`.
//!
//! # Rejection rules (hub)
//!
//! [`MediaDemux::open`] accepts a datagram only if, in this order (cheap checks first, so a
//! flood of garbage never reaches the AEAD):
//! 1. it has a valid header (magic, version 0) and fits `header + tag ..= MAX_DATAGRAM`;
//! 2. its `stream_id` is registered ([`crate::CoreError::UnknownStream`]);
//! 3. its `seq` lies in the stream's **sequence window**: not more than
//!    [`MAX_SEQ_JUMP`] above the highest `seq` accepted so far (or above `0` before the first
//!    packet, since every stream starts at `seq = 0`), and not a duplicate or older than
//!    [`hfa_proto::REPLAY_WINDOW`] below it ([`hfa_proto::ProtoError::Replay`] for both);
//! 4. it authenticates with the stream's key ([`hfa_proto::ProtoError::Crypto`]).
//!
//! The window only moves after authentication, so forged datagrams can neither advance nor
//! poison it, and a replayed or reordered-beyond-the-window datagram is dropped before it
//! reaches the jitter buffer. Rejections are counted ([`MediaDemux::stats`]) and logged at
//! most once per [`REJECT_LOG_INTERVAL`] (a flood cannot flood the log).

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hfa_proto::{MediaHeader, MediaKey, MediaOpener, MediaSealer, ProtoError};
use tokio::net::UdpSocket;

use crate::{CoreError, Result};

/// How far (in packets) a datagram's `seq` may jump ahead of the highest accepted one
/// (32768 packets = 5.5 min of 10 ms frames, far longer than the hub keeps an idle stream).
/// Anything further ahead is rejected without decryption.
pub const MAX_SEQ_JUMP: u32 = 1 << 15;
/// Minimum time between two log lines about rejected datagrams.
pub const REJECT_LOG_INTERVAL: Duration = Duration::from_secs(5);
/// Largest payload one datagram can carry (`MAX_DATAGRAM` minus header and tag).
pub const MAX_MEDIA_PAYLOAD: usize =
    hfa_proto::MAX_DATAGRAM - hfa_proto::MEDIA_HEADER_LEN - hfa_proto::AEAD_TAG_LEN;

/// Sends sealed media datagrams of one stream. Usable from a non-async thread (the encoder
/// thread): `send` never blocks and never waits for the tokio reactor. It sends through a
/// duplicated standard-library handle of the same (non-blocking) socket, so the very first
/// packet goes out even before the runtime has polled the socket, and a full socket buffer
/// simply drops the packet. If the handle cannot be duplicated (out of file descriptors), it
/// falls back to tokio's `try_send_to`.
///
/// Nonce safety: every `MediaSender` generates its **own fresh** [`MediaKey`] and starts at
/// `seq = 0`, and `seq` only ever increases, so a `(key, seq)` pair is never reused. To
/// restart a stream (new capture, new encoder...) either keep this sender (and set
/// [`hfa_proto::FLAG_RESET`] on the next packet) or create a new one, which means a new
/// `stream_id`, a new key and a new `StreamStart`. Announce each sender's key in exactly one
/// `StreamStart`.
pub struct MediaSender {
    socket: Arc<UdpSocket>,
    /// Non-blocking duplicate of `socket` for sending from any thread.
    raw: Option<std::net::UdpSocket>,
    dest: SocketAddr,
    key: MediaKey,
    sealer: MediaSealer,
    /// Sequence number of the next packet; `None` once `u32::MAX` has been used.
    next_seq: Option<u32>,
    buf: Vec<u8>,
}

impl std::fmt::Debug for MediaSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSender")
            .field("dest", &self.dest)
            .field("stream_id", &self.stream_id())
            .field("next_seq", &self.next_seq)
            .finish_non_exhaustive()
    }
}

impl MediaSender {
    /// Creates a sender for `stream_id` to `dest` with a freshly generated [`MediaKey`]
    /// (announce it with [`MediaSender::key`] in the stream's `StreamStart`), starting at
    /// `seq = 0`.
    pub fn new(socket: Arc<UdpSocket>, dest: SocketAddr, stream_id: u32) -> Self {
        let key = MediaKey::generate();
        let raw = match duplicate_socket(&socket) {
            Ok(raw) => Some(raw),
            Err(e) => {
                tracing::warn!(error = %e, "cannot duplicate the media socket; using tokio's try_send_to");
                None
            }
        };
        Self {
            socket,
            raw,
            dest,
            sealer: MediaSealer::new(&key, stream_id),
            key,
            next_seq: Some(0),
            buf: Vec::with_capacity(hfa_proto::MAX_DATAGRAM),
        }
    }

    /// The stream's media key (send it in `StreamStart.media_key`).
    pub fn key(&self) -> &MediaKey {
        &self.key
    }

    /// The stream id this sender seals for.
    pub fn stream_id(&self) -> u32 {
        self.sealer.stream_id()
    }

    /// Where datagrams go.
    pub fn dest(&self) -> SocketAddr {
        self.dest
    }

    /// Changes the destination (e.g. to the `udp_port` of `StreamAccepted`). The key and the
    /// sequence continue.
    pub fn set_dest(&mut self, dest: SocketAddr) {
        self.dest = dest;
    }

    /// Seals and sends one packet with the next sequence number. Returns `Ok(false)` if the
    /// packet was dropped because the socket buffer is full (its `seq` is still consumed).
    /// Never blocks and never allocates (the datagram buffer is reused).
    ///
    /// # Errors
    /// [`crate::CoreError::Proto`] (`FrameTooLarge`) if `payload` exceeds
    /// [`MAX_MEDIA_PAYLOAD`] (nothing is consumed), sealing errors, non-`WouldBlock` socket
    /// errors (the `seq` is consumed), or [`crate::CoreError::SequenceExhausted`] once
    /// `seq = u32::MAX` has been used (the sequence never wraps; start a new stream).
    pub fn send(&mut self, flags: u8, timestamp: u32, payload: &[u8]) -> Result<bool> {
        let stream_id = self.stream_id();
        let seq = self
            .next_seq
            .ok_or(CoreError::SequenceExhausted(stream_id))?;
        if payload.len() > MAX_MEDIA_PAYLOAD {
            return Err(ProtoError::FrameTooLarge {
                len: payload.len(),
                max: MAX_MEDIA_PAYLOAD,
            }
            .into());
        }
        let header = MediaHeader {
            flags,
            stream_id,
            seq,
            timestamp,
        };
        self.sealer.seal(&header, payload, &mut self.buf)?;
        self.next_seq = seq.checked_add(1);
        let sent = match &self.raw {
            Some(raw) => raw.send_to(&self.buf, self.dest),
            None => self.socket.try_send_to(&self.buf, self.dest),
        };
        match sent {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Sequence number the next packet will use, `None` if the sequence space is exhausted.
    pub fn next_seq(&self) -> Option<u32> {
        self.next_seq
    }
}

/// A standard-library handle to the same socket (a duplicated descriptor), non-blocking.
fn duplicate_socket(socket: &UdpSocket) -> std::io::Result<std::net::UdpSocket> {
    #[cfg(unix)]
    let raw = {
        use std::os::fd::AsFd;
        std::net::UdpSocket::from(socket.as_fd().try_clone_to_owned()?)
    };
    #[cfg(windows)]
    let raw = {
        use std::os::windows::io::AsSocket;
        std::net::UdpSocket::from(socket.as_socket().try_clone_to_owned()?)
    };
    #[cfg(not(any(unix, windows)))]
    let raw: std::net::UdpSocket = {
        let _ = socket;
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "socket duplication is not supported on this platform",
        ));
    };
    raw.set_nonblocking(true)?;
    Ok(raw)
}

/// Counters of a [`MediaDemux`] (all streams, since creation).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DemuxStats {
    /// Datagrams accepted and decrypted.
    pub accepted: u64,
    /// Datagrams with a bad header or size.
    pub malformed: u64,
    /// Datagrams for a stream id that is not registered.
    pub unknown_stream: u64,
    /// Datagrams outside the stream's sequence window (replays, duplicates, too old, too far
    /// ahead).
    pub out_of_window: u64,
    /// Datagrams that failed authentication.
    pub unauthenticated: u64,
}

impl DemuxStats {
    /// Total rejected datagrams.
    pub fn rejected(&self) -> u64 {
        self.malformed + self.unknown_stream + self.out_of_window + self.unauthenticated
    }
}

/// Hub-side demultiplexer: authenticates and decrypts datagrams of all known streams. See
/// the module docs for the rejection rules.
#[derive(Default)]
pub struct MediaDemux {
    openers: HashMap<u32, MediaOpener>,
    stats: DemuxStats,
    /// Rejections already reported in the log.
    logged_rejections: u64,
    last_log: Option<Instant>,
}

impl std::fmt::Debug for MediaDemux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaDemux")
            .field("streams", &self.openers.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl MediaDemux {
    /// Creates an empty demux.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a stream. Returns `false` and changes nothing if `stream_id` is already
    /// registered: stream ids are unique among active streams (so one sender cannot hijack
    /// another's stream) and re-registering would reset the replay window. The hub answers
    /// such a `StreamStart` with `StreamRejected`.
    #[must_use]
    pub fn add_stream(&mut self, stream_id: u32, key: &MediaKey) -> bool {
        match self.openers.entry(stream_id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(MediaOpener::new(key, stream_id));
                true
            }
        }
    }

    /// `true` if `stream_id` is registered.
    pub fn contains(&self, stream_id: u32) -> bool {
        self.openers.contains_key(&stream_id)
    }

    /// Forgets a stream.
    pub fn remove_stream(&mut self, stream_id: u32) {
        self.openers.remove(&stream_id);
    }

    /// Number of registered streams.
    pub fn len(&self) -> usize {
        self.openers.len()
    }

    /// `true` if no stream is registered.
    pub fn is_empty(&self) -> bool {
        self.openers.is_empty()
    }

    /// Highest `seq` accepted on `stream_id`, if any.
    pub fn highest_seq(&self, stream_id: u32) -> Option<u32> {
        self.openers.get(&stream_id)?.highest_seq()
    }

    /// Counters since creation.
    pub fn stats(&self) -> DemuxStats {
        self.stats
    }

    /// Opens a datagram of a registered stream, applying the module-level rejection rules.
    /// Rejected datagrams are counted and reported in the log at most once per
    /// [`REJECT_LOG_INTERVAL`], so callers can simply drop them.
    ///
    /// # Errors
    /// [`crate::CoreError::UnknownStream`] or [`crate::CoreError::Proto`]: header/size
    /// errors, `Replay` (outside the sequence window) or `Crypto` (authentication failure).
    pub fn open(&mut self, datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)> {
        let result = self.open_inner(datagram);
        match &result {
            Ok(_) => self.stats.accepted += 1,
            Err(e) => {
                match e {
                    CoreError::UnknownStream(_) => self.stats.unknown_stream += 1,
                    CoreError::Proto(ProtoError::Replay { .. }) => self.stats.out_of_window += 1,
                    CoreError::Proto(ProtoError::Crypto) => self.stats.unauthenticated += 1,
                    _ => self.stats.malformed += 1,
                }
                self.log_rejections(e);
            }
        }
        result
    }

    fn open_inner(&mut self, datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)> {
        let (header, _) = MediaHeader::decode(datagram)?;
        let opener = self
            .openers
            .get_mut(&header.stream_id)
            .ok_or(CoreError::UnknownStream(header.stream_id))?;
        let limit = opener
            .highest_seq()
            .map_or(MAX_SEQ_JUMP, |high| high.saturating_add(MAX_SEQ_JUMP));
        if header.seq > limit {
            return Err(ProtoError::Replay { seq: header.seq }.into());
        }
        Ok(opener.open(datagram)?)
    }

    /// Rate-limited report of rejected datagrams.
    fn log_rejections(&mut self, last: &CoreError) {
        let now = Instant::now();
        if self
            .last_log
            .is_some_and(|t| now.duration_since(t) < REJECT_LOG_INTERVAL)
        {
            return;
        }
        let new = self.stats.rejected() - self.logged_rejections;
        self.logged_rejections = self.stats.rejected();
        self.last_log = Some(now);
        tracing::debug!(
            rejected = new,
            total = ?self.stats,
            last_error = %last,
            "dropping invalid media datagrams"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sequence_never_wraps() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind"));
        let dest = socket.local_addr().expect("addr");
        let mut sender = MediaSender::new(Arc::clone(&socket), dest, 9);
        sender.next_seq = Some(u32::MAX);
        assert!(sender.send(0, 0, b"last").expect("last seq"));
        assert_eq!(sender.next_seq(), None);
        assert_eq!(
            sender.send(0, 0, b"one more"),
            Err(CoreError::SequenceExhausted(9))
        );
        let mut demux = MediaDemux::new();
        assert!(demux.add_stream(9, sender.key()));
        let mut buf = [0u8; 2048];
        let (n, _) = socket.recv_from(&mut buf).await.expect("recv");
        // u32::MAX is far ahead of a fresh stream's window.
        assert!(demux.open(&buf[..n]).is_err());
        assert_eq!(demux.stats().out_of_window, 1);
    }

    #[test]
    fn demux_refuses_to_replace_an_active_stream() {
        let mut demux = MediaDemux::new();
        let a = MediaKey::from_bytes([1; 32]);
        let b = MediaKey::from_bytes([2; 32]);
        assert!(!demux.contains(7));
        assert!(demux.add_stream(7, &a));
        assert!(!demux.add_stream(7, &b), "stream 7 must not be hijacked");
        assert!(demux.contains(7));
        demux.remove_stream(7);
        assert!(!demux.contains(7));
        assert!(demux.add_stream(7, &b));
    }
}
