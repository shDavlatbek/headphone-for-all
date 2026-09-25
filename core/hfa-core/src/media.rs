//! UDP media helpers built on [`hfa_proto`] sealing.
//!
//! The sender owns one [`MediaSender`] per stream. The hub has one UDP socket (same port
//! number as the TCP control port) and one [`MediaDemux`] that routes datagrams to per-stream
//! openers by `stream_id`.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use hfa_proto::{MediaHeader, MediaKey, MediaOpener, MediaSealer};
use tokio::net::UdpSocket;

use crate::Result;

/// Sends sealed media datagrams of one stream. Usable from a non-async thread: sending
/// uses `try_send_to` and never blocks (a full socket buffer drops the packet).
///
/// Nonce safety: every `MediaSender` generates its **own fresh** [`MediaKey`] and starts at
/// `seq = 0`, and `seq` only ever increases, so a `(key, seq)` pair is never reused. To
/// restart a stream (new capture, new encoder...) either keep this sender (and set
/// [`hfa_proto::FLAG_RESET`] on the next packet) or create a new one, which means a new
/// `stream_id`, a new key and a new `StreamStart`.
pub struct MediaSender {
    socket: Arc<UdpSocket>,
    dest: SocketAddr,
    key: MediaKey,
    sealer: MediaSealer,
    /// Sequence number of the next packet; `None` once `u32::MAX` has been used.
    next_seq: Option<u32>,
    buf: Vec<u8>,
}

impl MediaSender {
    /// Creates a sender for `stream_id` to `dest` with a freshly generated [`MediaKey`]
    /// (announce it with [`MediaSender::key`] in the stream's `StreamStart`), starting at
    /// `seq = 0`.
    pub fn new(socket: Arc<UdpSocket>, dest: SocketAddr, stream_id: u32) -> Self {
        let key = MediaKey::generate();
        Self {
            socket,
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

    /// Seals and sends one packet with the next sequence number. Returns `Ok(false)` if the
    /// packet was dropped because the socket buffer is full (its `seq` is still consumed).
    ///
    /// # Errors
    /// Sealing errors, non-`WouldBlock` socket errors, or
    /// [`crate::CoreError::SequenceExhausted`] once `seq = u32::MAX` has been used (the
    /// sequence never wraps; start a new stream).
    pub fn send(&mut self, _flags: u8, _timestamp: u32, _payload: &[u8]) -> Result<bool> {
        let _ = (&self.socket, self.dest, &self.sealer, &self.buf);
        todo!("feat/core-engine")
    }

    /// Sequence number the next packet will use, `None` if the sequence space is exhausted.
    pub fn next_seq(&self) -> Option<u32> {
        self.next_seq
    }
}

/// Hub-side demultiplexer: authenticates and decrypts datagrams of all known streams.
#[derive(Default)]
pub struct MediaDemux {
    openers: HashMap<u32, MediaOpener>,
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

    /// Opens a datagram of a registered stream (with the stream's replay protection, see
    /// [`MediaOpener::open`]).
    ///
    /// # Errors
    /// [`crate::CoreError::UnknownStream`] or [`crate::CoreError::Proto`] (bad header, replay
    /// or authentication failure).
    pub fn open(&mut self, _datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)> {
        todo!("feat/core-engine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
