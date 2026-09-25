//! UDP media helpers built on [`hfa_proto`] sealing.
//!
//! The sender owns one [`MediaSender`] per stream. The hub has one UDP socket (same port
//! number as the TCP control port) and one [`MediaDemux`] that routes datagrams to per-stream
//! openers by `stream_id`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use hfa_proto::{MediaHeader, MediaKey, MediaOpener, MediaSealer};
use tokio::net::UdpSocket;

use crate::Result;

/// Sends sealed media datagrams of one stream. Usable from a non-async thread: sending
/// uses `try_send_to` and never blocks (a full socket buffer drops the packet).
pub struct MediaSender {
    socket: Arc<UdpSocket>,
    dest: SocketAddr,
    sealer: MediaSealer,
    next_seq: u32,
    buf: Vec<u8>,
}

impl MediaSender {
    /// Creates a sender for `stream_id` to `dest`, starting at `seq = 0`.
    pub fn new(socket: Arc<UdpSocket>, dest: SocketAddr, key: &MediaKey, stream_id: u32) -> Self {
        Self {
            socket,
            dest,
            sealer: MediaSealer::new(key, stream_id),
            next_seq: 0,
            buf: Vec::with_capacity(hfa_proto::MAX_DATAGRAM),
        }
    }

    /// Seals and sends one packet with the next sequence number. Returns `Ok(false)` if the
    /// packet was dropped because the socket buffer is full.
    ///
    /// # Errors
    /// Sealing errors or non-`WouldBlock` socket errors.
    pub fn send(&mut self, _flags: u8, _timestamp: u32, _payload: &[u8]) -> Result<bool> {
        let _ = (&self.socket, self.dest, &self.sealer, &self.buf);
        todo!("feat/core-engine")
    }

    /// Sequence number the next packet will use.
    pub fn next_seq(&self) -> u32 {
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

    /// Registers a stream (replaces an existing one with the same id).
    pub fn add_stream(&mut self, stream_id: u32, key: &MediaKey) {
        self.openers
            .insert(stream_id, MediaOpener::new(key, stream_id));
    }

    /// Forgets a stream.
    pub fn remove_stream(&mut self, stream_id: u32) {
        self.openers.remove(&stream_id);
    }

    /// Opens a datagram of a registered stream.
    ///
    /// # Errors
    /// [`crate::CoreError::UnknownStream`] or [`crate::CoreError::Proto`] (bad header or
    /// authentication failure).
    pub fn open(&self, _datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)> {
        todo!("feat/core-engine")
    }
}
