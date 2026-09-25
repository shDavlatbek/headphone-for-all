//! Control-channel messages (protobuf via hand-written prost derives; no `protoc` needed).
//!
//! **The field tags below are the wire contract and are FINAL.** Never renumber or reuse a tag;
//! add new fields/variants with new tags only.
//!
//! Equivalent `.proto` (for reference):
//!
//! ```text
//! enum Role       { ROLE_UNSPECIFIED = 0; ROLE_HUB = 1; ROLE_SENDER = 2; }
//! enum PairMethod { PAIR_METHOD_UNSPECIFIED = 0; PAIR_METHOD_PIN = 1; PAIR_METHOD_TOKEN = 2; }
//!
//! message ControlMessage {
//!   oneof body {
//!     Hello hello = 1;
//!     PairStart pair_start = 2;   PairSpake pair_spake = 3;
//!     PairConfirm pair_confirm = 4; PairResult pair_result = 5;
//!     StreamStart stream_start = 10; StreamAccepted stream_accepted = 11;
//!     StreamRejected stream_rejected = 12; StreamStop stream_stop = 13;
//!     SetVolume set_volume = 20; SetMute set_mute = 21; SetPriority set_priority = 22;
//!     Ping ping = 30; Pong pong = 31; Stats stats = 32; Bye bye = 40;
//!   }
//! }
//! message Hello { uint32 protocol_version = 1; string device_id = 2; string device_name = 3;
//!                 string platform = 4; string app_version = 5; Role role = 6;
//!                 bool pairing_required = 7; }  // sender: "I don't trust you"; hub: "pair now"
//! message PairStart   { PairMethod method = 1; }
//! message PairSpake   { bytes msg = 1; }
//! message PairConfirm { bytes mac = 1; }
//! message PairResult  { bool ok = 1; string reason = 2; }
//! message StreamStart { uint32 stream_id = 1; uint32 sample_rate = 2; uint32 channels = 3;
//!                       uint32 frame_ms = 4; uint32 bitrate = 5; string label = 6; bytes media_key = 7; }
//! message StreamAccepted { uint32 stream_id = 1; uint32 udp_port = 2; }
//! message StreamRejected { uint32 stream_id = 1; string reason = 2; }
//! message StreamStop  { uint32 stream_id = 1; }
//! message SetVolume   { uint32 stream_id = 1; float gain = 2; }
//! message SetMute     { uint32 stream_id = 1; bool muted = 2; }
//! message SetPriority { uint32 stream_id = 1; bool priority = 2; }
//! message Ping { uint64 nonce = 1; uint64 t_us = 2; }
//! message Pong { uint64 nonce = 1; uint64 t_us = 2; }
//! message Stats { uint32 stream_id = 1; float loss_pct = 2; float jitter_ms = 3; float buffer_ms = 4;
//!                 float latency_ms = 5; uint32 recommended_bitrate = 6; }
//! message Bye { string reason = 1; }
//! ```
//!
//! Framing: every encoded `ControlMessage` is prefixed with its length as `u32` big-endian
//! ([`encode_frame`] / [`FrameDecoder`]). Frames whose protobuf payload is larger than
//! [`crate::MAX_CONTROL_FRAME`] are rejected on both sides.

use std::fmt;

use prost::Message;

use crate::{ProtoError, Result, MAX_CONTROL_FRAME};

/// Length of the frame length prefix (`u32` BE).
pub const FRAME_PREFIX_LEN: usize = 4;

/// Role a peer announces in [`Hello`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum Role {
    /// Not set (proto3 default); treated as a protocol error.
    Unspecified = 0,
    /// The device the headphone is connected to; receives and mixes streams.
    Hub = 1,
    /// A device that captures its audio and streams it to a hub.
    Sender = 2,
}

/// Which shared secret the sender uses for SPAKE2 pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum PairMethod {
    /// Not set (proto3 default); treated as a protocol error.
    Unspecified = 0,
    /// The 6-digit PIN shown on the hub.
    Pin = 1,
    /// The one-time token from the pairing URI / QR code.
    Token = 2,
}

/// First message on a control channel, sent by both sides right after the Noise handshake.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Hello {
    /// [`crate::PROTOCOL_VERSION`] of the sender of this message.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Fingerprint of the device's static public key ([`crate::fingerprint`]).
    #[prost(string, tag = "2")]
    pub device_id: String,
    /// Human-readable device name.
    #[prost(string, tag = "3")]
    pub device_name: String,
    /// Platform name (`windows`, `macos`, `linux`, `android`, `ios`).
    #[prost(string, tag = "4")]
    pub platform: String,
    /// Application version string.
    #[prost(string, tag = "5")]
    pub app_version: String,
    /// [`Role`] as `i32` (use [`Hello::role()`] / [`Hello::set_role`]).
    #[prost(enumeration = "Role", tag = "6")]
    pub role: i32,
    /// Pairing request, meaningful in both directions:
    /// - sender → hub: `true` iff the sender does **not** trust the hub's static key (it
    ///   knows it after the Noise handshake), so the sender will pair next;
    /// - hub → sender: `true` iff the hub does not trust the sender's key **or** the
    ///   sender's `Hello` set this flag, so both sides agree whether pairing follows.
    ///
    /// A sender that does not trust the hub pairs regardless of the hub's flag. See the
    /// `hfa-core` `control` module for the full procedure.
    #[prost(bool, tag = "7")]
    pub pairing_required: bool,
}

/// Sender → hub: begin pairing with the given method.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PairStart {
    /// [`PairMethod`] as `i32`.
    #[prost(enumeration = "PairMethod", tag = "1")]
    pub method: i32,
}

/// SPAKE2 message (both directions). `Debug` prints only the length.
#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub struct PairSpake {
    /// The SPAKE2 message bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub msg: Vec<u8>,
}

/// Key-confirmation MAC (both directions), see [`crate::PairingKey::confirm_mac`]. `Debug`
/// prints only the length.
#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub struct PairConfirm {
    /// HMAC-SHA256 confirmation value (32 bytes).
    #[prost(bytes = "vec", tag = "1")]
    pub mac: Vec<u8>,
}

/// Hub → sender: outcome of pairing.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PairResult {
    /// `true` if the sender is now trusted.
    #[prost(bool, tag = "1")]
    pub ok: bool,
    /// Human-readable reason on failure.
    #[prost(string, tag = "2")]
    pub reason: String,
}

/// Sender → hub: announce a media stream.
///
/// `Debug` redacts `media_key`, so logging a whole [`ControlMessage`] never leaks it. Receivers
/// should turn `media_key` into a [`crate::MediaKey`] (which zeroizes on drop) right away.
#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub struct StreamStart {
    /// Random stream id used in media headers.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// Sample rate of the encoded audio (48 000).
    #[prost(uint32, tag = "2")]
    pub sample_rate: u32,
    /// Channel count of the encoded audio (2).
    #[prost(uint32, tag = "3")]
    pub channels: u32,
    /// Opus frame duration in milliseconds (10 or 20).
    #[prost(uint32, tag = "4")]
    pub frame_ms: u32,
    /// Initial Opus bitrate in bits per second.
    #[prost(uint32, tag = "5")]
    pub bitrate: u32,
    /// Human-readable label (e.g. "System audio", an app name).
    #[prost(string, tag = "6")]
    pub label: String,
    /// The 32-byte [`crate::MediaKey`] for this stream.
    #[prost(bytes = "vec", tag = "7")]
    pub media_key: Vec<u8>,
}

impl std::fmt::Debug for PairSpake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairSpake")
            .field("msg", &format_args!("<{} bytes>", self.msg.len()))
            .finish()
    }
}

impl std::fmt::Debug for PairConfirm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairConfirm")
            .field("mac", &format_args!("<{} bytes>", self.mac.len()))
            .finish()
    }
}

impl std::fmt::Debug for StreamStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamStart")
            .field("stream_id", &self.stream_id)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frame_ms", &self.frame_ms)
            .field("bitrate", &self.bitrate)
            .field("label", &self.label)
            .field(
                "media_key",
                &format_args!("<redacted {} bytes>", self.media_key.len()),
            )
            .finish()
    }
}

/// Hub → sender: the stream is accepted; send media to `udp_port`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StreamAccepted {
    /// The accepted stream.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// UDP port on the hub to send media to.
    #[prost(uint32, tag = "2")]
    pub udp_port: u32,
}

/// Hub → sender: the stream is rejected.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StreamRejected {
    /// The rejected stream.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// Human-readable reason.
    #[prost(string, tag = "2")]
    pub reason: String,
}

/// Either side: the stream ended.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StreamStop {
    /// The stopped stream.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
}

/// Hub → sender (informational, for the sender UI): the hub changed this stream's volume.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetVolume {
    /// Target stream.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// Linear gain (1.0 = unity).
    #[prost(float, tag = "2")]
    pub gain: f32,
}

/// Hub → sender (informational): mute state changed.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetMute {
    /// Target stream.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// `true` if muted.
    #[prost(bool, tag = "2")]
    pub muted: bool,
}

/// Hub → sender (informational): priority (ducking) state changed.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetPriority {
    /// Target stream.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// `true` if this stream ducks the others.
    #[prost(bool, tag = "2")]
    pub priority: bool,
}

/// Keep-alive / RTT probe.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Ping {
    /// Random value echoed in [`Pong`].
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
    /// Sender's monotonic clock in microseconds.
    #[prost(uint64, tag = "2")]
    pub t_us: u64,
}

/// Reply to [`Ping`].
#[derive(Clone, PartialEq, prost::Message)]
pub struct Pong {
    /// The nonce from the [`Ping`].
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
    /// The `t_us` from the [`Ping`], echoed unchanged.
    #[prost(uint64, tag = "2")]
    pub t_us: u64,
}

/// Hub → sender, once per second per stream: receive statistics and a bitrate recommendation.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Stats {
    /// Stream the statistics are for.
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    /// Packet loss over the last interval, in percent.
    #[prost(float, tag = "2")]
    pub loss_pct: f32,
    /// Interarrival jitter estimate in milliseconds.
    #[prost(float, tag = "3")]
    pub jitter_ms: f32,
    /// Current jitter-buffer fill in milliseconds.
    #[prost(float, tag = "4")]
    pub buffer_ms: f32,
    /// Estimated end-to-end latency in milliseconds.
    #[prost(float, tag = "5")]
    pub latency_ms: f32,
    /// Bitrate the hub recommends, in bits per second (0 = no recommendation).
    #[prost(uint32, tag = "6")]
    pub recommended_bitrate: u32,
}

/// Either side: orderly shutdown of the control channel.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Bye {
    /// Human-readable reason.
    #[prost(string, tag = "1")]
    pub reason: String,
}

/// Envelope for every control message.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ControlMessage {
    /// The message. `None` only for malformed/empty input.
    #[prost(
        oneof = "control_message::Body",
        tags = "1, 2, 3, 4, 5, 10, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40"
    )]
    pub body: Option<control_message::Body>,
}

/// The `oneof` of [`ControlMessage`].
pub mod control_message {
    /// All control message variants with their FINAL tags.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        /// Tag 1.
        #[prost(message, tag = "1")]
        Hello(super::Hello),
        /// Tag 2.
        #[prost(message, tag = "2")]
        PairStart(super::PairStart),
        /// Tag 3.
        #[prost(message, tag = "3")]
        PairSpake(super::PairSpake),
        /// Tag 4.
        #[prost(message, tag = "4")]
        PairConfirm(super::PairConfirm),
        /// Tag 5.
        #[prost(message, tag = "5")]
        PairResult(super::PairResult),
        /// Tag 10.
        #[prost(message, tag = "10")]
        StreamStart(super::StreamStart),
        /// Tag 11.
        #[prost(message, tag = "11")]
        StreamAccepted(super::StreamAccepted),
        /// Tag 12.
        #[prost(message, tag = "12")]
        StreamRejected(super::StreamRejected),
        /// Tag 13.
        #[prost(message, tag = "13")]
        StreamStop(super::StreamStop),
        /// Tag 20.
        #[prost(message, tag = "20")]
        SetVolume(super::SetVolume),
        /// Tag 21.
        #[prost(message, tag = "21")]
        SetMute(super::SetMute),
        /// Tag 22.
        #[prost(message, tag = "22")]
        SetPriority(super::SetPriority),
        /// Tag 30.
        #[prost(message, tag = "30")]
        Ping(super::Ping),
        /// Tag 31.
        #[prost(message, tag = "31")]
        Pong(super::Pong),
        /// Tag 32.
        #[prost(message, tag = "32")]
        Stats(super::Stats),
        /// Tag 40.
        #[prost(message, tag = "40")]
        Bye(super::Bye),
    }
}

pub use control_message::Body;

impl ControlMessage {
    /// Wraps a body into a message.
    pub fn new(body: Body) -> Self {
        Self { body: Some(body) }
    }
}

impl From<Body> for ControlMessage {
    fn from(body: Body) -> Self {
        Self::new(body)
    }
}

/// Encodes `msg` as a frame: `u32` BE length prefix followed by the protobuf bytes.
///
/// # Errors
/// [`ProtoError::InvalidMessage`] if `msg.body` is `None` (the peer would reject it), or
/// [`ProtoError::FrameTooLarge`] if the protobuf encoding exceeds
/// [`crate::MAX_CONTROL_FRAME`] bytes.
pub fn encode_frame(msg: &ControlMessage) -> Result<Vec<u8>> {
    if msg.body.is_none() {
        return Err(ProtoError::InvalidMessage("empty control message".into()));
    }
    let len = msg.encoded_len();
    if len > MAX_CONTROL_FRAME {
        return Err(ProtoError::FrameTooLarge {
            len,
            max: MAX_CONTROL_FRAME,
        });
    }
    let mut out = Vec::with_capacity(FRAME_PREFIX_LEN + len);
    // `len <= MAX_CONTROL_FRAME < u32::MAX`, so the cast is lossless.
    out.extend_from_slice(&(len as u32).to_be_bytes());
    msg.encode_raw(&mut out);
    Ok(out)
}

/// Decodes one protobuf payload (without the length prefix) into a [`ControlMessage`].
///
/// # Errors
/// [`ProtoError::FrameTooLarge`], [`ProtoError::Decode`] for malformed protobuf, or
/// [`ProtoError::InvalidMessage`] for a message without a body (e.g. an unknown variant from a
/// newer peer, which prost skips).
pub fn decode_message(payload: &[u8]) -> Result<ControlMessage> {
    if payload.len() > MAX_CONTROL_FRAME {
        return Err(ProtoError::FrameTooLarge {
            len: payload.len(),
            max: MAX_CONTROL_FRAME,
        });
    }
    let msg = ControlMessage::decode(payload).map_err(|e| ProtoError::Decode(e.to_string()))?;
    if msg.body.is_none() {
        return Err(ProtoError::InvalidMessage(
            "control message without a known body".into(),
        ));
    }
    Ok(msg)
}

/// Incremental decoder for a byte stream of frames produced by [`encode_frame`].
///
/// Feed bytes with [`FrameDecoder::push`], then call [`Iterator::next`] until it returns
/// `None` (more bytes needed). An undecodable/empty message yields `Some(Err(..))` for that
/// frame only; decoding continues with the next frame.
///
/// A length prefix above [`crate::MAX_CONTROL_FRAME`] means the stream is corrupt or hostile:
/// it yields [`ProtoError::FrameTooLarge`] without ever allocating the announced length, and
/// the decoder is then **poisoned** (framing is lost): buffered data is dropped, further input
/// is ignored, and the error is returned exactly once; every later `next()` returns `None`, so
/// a loop that logs and skips errors still terminates. After a drain, check
/// [`FrameDecoder::is_poisoned`] (or [`FrameDecoder::poison_error`]) and close the connection.
///
/// `Debug` prints only sizes, never buffered (possibly secret) bytes.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// Start of the unconsumed bytes in `buf`.
    pos: usize,
    /// Set after a fatal framing error.
    poisoned: Option<ProtoError>,
}

impl FrameDecoder {
    /// Creates an empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends received bytes (ignored once the decoder is poisoned).
    pub fn push(&mut self, data: &[u8]) {
        if self.poisoned.is_some() || data.is_empty() {
            return;
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Number of buffered bytes not yet consumed by a complete frame.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// `true` after a fatal framing error (see the type docs).
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }

    /// The fatal framing error that poisoned the decoder, if any.
    pub fn poison_error(&self) -> Option<&ProtoError> {
        self.poisoned.as_ref()
    }

    fn poison(&mut self, err: ProtoError) -> ProtoError {
        self.buf = Vec::new();
        self.pos = 0;
        self.poisoned = Some(err.clone());
        err
    }
}

impl fmt::Debug for FrameDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameDecoder")
            .field("buffered", &self.buffered())
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl Iterator for FrameDecoder {
    type Item = Result<ControlMessage>;

    /// Returns the next complete message, or `None` if more bytes are needed or the decoder
    /// is poisoned.
    fn next(&mut self) -> Option<Self::Item> {
        if self.poisoned.is_some() {
            // The fatal error was returned once; the stream is over.
            return None;
        }
        let pending = &self.buf[self.pos..];
        let (prefix, rest) = pending.split_first_chunk::<FRAME_PREFIX_LEN>()?;
        let len = u32::from_be_bytes(*prefix) as usize;
        if len > MAX_CONTROL_FRAME {
            return Some(Err(self.poison(ProtoError::FrameTooLarge {
                len,
                max: MAX_CONTROL_FRAME,
            })));
        }
        let payload = rest.get(..len)?;
        let result = decode_message(payload);
        self.pos += FRAME_PREFIX_LEN + len;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_secrets() {
        let key = vec![0xAB_u8; 32];
        let msg = ControlMessage::new(Body::StreamStart(StreamStart {
            stream_id: 7,
            sample_rate: 48_000,
            channels: 2,
            frame_ms: 10,
            bitrate: 128_000,
            label: "System audio".into(),
            media_key: key,
        }));
        let text = format!("{msg:?}");
        assert!(text.contains("stream_id: 7"), "{text}");
        assert!(text.contains("<redacted 32 bytes>"), "{text}");
        assert!(!text.contains("171"), "decimal key bytes leaked: {text}");
        assert!(!text.to_lowercase().contains("ab, ab"), "{text}");

        let spake = format!("{:?}", PairSpake { msg: vec![171; 33] });
        assert!(
            spake.contains("<33 bytes>") && !spake.contains("171"),
            "{spake}"
        );
        let confirm = format!("{:?}", PairConfirm { mac: vec![171; 32] });
        assert!(
            confirm.contains("<32 bytes>") && !confirm.contains("171"),
            "{confirm}"
        );
    }

    fn sample_messages() -> Vec<ControlMessage> {
        vec![
            Body::Hello(Hello {
                protocol_version: 0,
                device_id: "ab12-cd34-ef56-7890".into(),
                device_name: "Küche 🎧".into(),
                platform: "linux".into(),
                app_version: "0.1.0".into(),
                role: Role::Sender as i32,
                pairing_required: true,
            }),
            Body::PairStart(PairStart {
                method: PairMethod::Token as i32,
            }),
            Body::PairSpake(PairSpake {
                msg: vec![0x53; 33],
            }),
            Body::PairConfirm(PairConfirm { mac: vec![9; 32] }),
            Body::PairResult(PairResult {
                ok: false,
                reason: "wrong PIN".into(),
            }),
            Body::StreamStart(StreamStart {
                stream_id: 0xDEAD_BEEF,
                sample_rate: 48_000,
                channels: 2,
                frame_ms: 10,
                bitrate: 128_000,
                label: "System audio".into(),
                media_key: vec![1; 32],
            }),
            Body::StreamAccepted(StreamAccepted {
                stream_id: 1,
                udp_port: 47810,
            }),
            Body::StreamRejected(StreamRejected {
                stream_id: 2,
                reason: "duplicate".into(),
            }),
            Body::StreamStop(StreamStop { stream_id: 3 }),
            Body::SetVolume(SetVolume {
                stream_id: 4,
                gain: 0.5,
            }),
            Body::SetMute(SetMute {
                stream_id: 5,
                muted: true,
            }),
            Body::SetPriority(SetPriority {
                stream_id: 6,
                priority: true,
            }),
            Body::Ping(Ping {
                nonce: u64::MAX,
                t_us: 123_456_789,
            }),
            Body::Pong(Pong { nonce: 7, t_us: 8 }),
            Body::Stats(Stats {
                stream_id: 9,
                loss_pct: 1.5,
                jitter_ms: 3.25,
                buffer_ms: 40.0,
                latency_ms: 62.5,
                recommended_bitrate: 96_000,
            }),
            Body::Bye(Bye {
                reason: "shutdown".into(),
            }),
            // All-default bodies still encode the oneof tag and must roundtrip.
            Body::StreamStop(StreamStop::default()),
            Body::Bye(Bye::default()),
        ]
        .into_iter()
        .map(ControlMessage::new)
        .collect()
    }

    fn stream_of(msgs: &[ControlMessage]) -> Vec<u8> {
        msgs.iter().flat_map(|m| encode_frame(m).unwrap()).collect()
    }

    #[test]
    fn every_variant_roundtrips() {
        for msg in sample_messages() {
            let frame = encode_frame(&msg).unwrap();
            let len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
            assert_eq!(len, frame.len() - FRAME_PREFIX_LEN);
            assert_eq!(decode_message(&frame[4..]).unwrap(), msg);
            let mut dec = FrameDecoder::new();
            dec.push(&frame);
            assert_eq!(dec.next(), Some(Ok(msg)));
            assert_eq!(dec.next(), None);
            assert_eq!(dec.buffered(), 0);
        }
    }

    /// Golden bytes: the oneof and field tags are the wire contract.
    #[test]
    fn wire_tags_are_stable() {
        let ping = ControlMessage::new(Body::Ping(Ping { nonce: 1, t_us: 2 }));
        // Field 30, wire type 2 = key 242 = varint F2 01; len 4; nonce (1) = 08 01; t_us (2) = 10 02.
        assert_eq!(
            encode_frame(&ping).unwrap(),
            [0, 0, 0, 7, 0xF2, 0x01, 4, 0x08, 0x01, 0x10, 0x02]
        );
        let bye = ControlMessage::new(Body::Bye(Bye { reason: "x".into() }));
        // Field 40 = key 322 = C2 02; len 3; reason = 0A 01 'x'.
        assert_eq!(
            encode_frame(&bye).unwrap(),
            [0, 0, 0, 6, 0xC2, 0x02, 3, 0x0A, 1, b'x']
        );
        let start = ControlMessage::new(Body::PairStart(PairStart {
            method: PairMethod::Pin as i32,
        }));
        assert_eq!(
            encode_frame(&start).unwrap(),
            [0, 0, 0, 4, 0x12, 2, 0x08, 1]
        );
        let stats = ControlMessage::new(Body::Stats(Stats {
            recommended_bitrate: 1,
            ..Default::default()
        }));
        // Field 32 = key 258 = 82 02; inner field 6 varint = 30 01.
        assert_eq!(
            encode_frame(&stats).unwrap(),
            [0, 0, 0, 5, 0x82, 0x02, 2, 0x30, 1]
        );
    }

    #[test]
    fn encode_rejects_empty_and_oversize() {
        assert!(matches!(
            encode_frame(&ControlMessage::default()),
            Err(ProtoError::InvalidMessage(_))
        ));
        let big = ControlMessage::new(Body::Bye(Bye {
            reason: "x".repeat(MAX_CONTROL_FRAME),
        }));
        assert!(matches!(
            encode_frame(&big),
            Err(ProtoError::FrameTooLarge {
                max: MAX_CONTROL_FRAME,
                ..
            })
        ));
        // The largest allowed payload is accepted and decodes again.
        let bye = |n: usize| {
            ControlMessage::new(Body::Bye(Bye {
                reason: "x".repeat(n),
            }))
        };
        let n = (0..MAX_CONTROL_FRAME)
            .rev()
            .find(|&n| bye(n).encoded_len() <= MAX_CONTROL_FRAME)
            .unwrap();
        assert_eq!(bye(n).encoded_len(), MAX_CONTROL_FRAME);
        assert!(encode_frame(&bye(n + 1)).is_err());
        let fits = bye(n);
        let frame = encode_frame(&fits).unwrap();
        assert!(frame.len() - FRAME_PREFIX_LEN <= MAX_CONTROL_FRAME);
        let mut dec = FrameDecoder::new();
        dec.push(&frame);
        assert_eq!(dec.next(), Some(Ok(fits)));
    }

    #[test]
    fn partial_and_concatenated_input() {
        let msgs = sample_messages();
        let bytes = stream_of(&msgs);
        // Byte by byte.
        let mut dec = FrameDecoder::new();
        let mut got = Vec::new();
        for b in &bytes {
            dec.push(std::slice::from_ref(b));
            got.extend(dec.by_ref().map(|r| r.unwrap()));
        }
        assert_eq!(got, msgs);
        assert_eq!(dec.buffered(), 0);
        // All at once.
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let got: Vec<_> = dec.by_ref().map(|r| r.unwrap()).collect();
        assert_eq!(got, msgs);
        // A trailing partial frame stays buffered.
        let mut dec = FrameDecoder::new();
        dec.push(&bytes[..bytes.len() - 1]);
        assert_eq!(dec.by_ref().count(), msgs.len() - 1);
        assert!(dec.buffered() > 0);
        dec.push(&bytes[bytes.len() - 1..]);
        assert_eq!(dec.next(), Some(Ok(msgs[msgs.len() - 1].clone())));
    }

    #[test]
    fn oversize_prefix_is_rejected_without_allocating_and_poisons() {
        let mut dec = FrameDecoder::new();
        let len = MAX_CONTROL_FRAME + 1;
        dec.push(&(len as u32).to_be_bytes());
        dec.push(&[0; 16]);
        let err = ProtoError::FrameTooLarge {
            len,
            max: MAX_CONTROL_FRAME,
        };
        assert_eq!(dec.next(), Some(Err(err.clone())));
        assert!(dec.is_poisoned());
        assert_eq!(dec.buffered(), 0);
        assert!(dec.buf.capacity() < 1024, "announced length was allocated");
        // Poisoned: later input is ignored and the iterator has ended.
        dec.push(&encode_frame(&sample_messages()[0]).unwrap());
        assert_eq!(dec.buffered(), 0);
        assert_eq!(dec.next(), None);
        assert_eq!(dec.next(), None);
        assert!(dec.is_poisoned());

        let mut dec = FrameDecoder::new();
        dec.push(&u32::MAX.to_be_bytes());
        assert!(matches!(
            dec.next(),
            Some(Err(ProtoError::FrameTooLarge { len, .. })) if len == u32::MAX as usize
        ));
    }

    #[test]
    fn poisoned_decoder_ends_error_skipping_loops() {
        let good = encode_frame(&sample_messages()[0]).unwrap();
        let mut bytes = good.clone();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        bytes.extend_from_slice(&good);
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        // A consumer that logs and skips errors must terminate.
        let (mut oks, mut errs) = (0, 0);
        for r in dec.by_ref() {
            match r {
                Ok(_) => oks += 1,
                Err(_) => errs += 1,
            }
        }
        assert_eq!((oks, errs), (1, 1));
        assert!(dec.is_poisoned());
        assert!(matches!(
            dec.poison_error(),
            Some(ProtoError::FrameTooLarge { .. })
        ));
        dec.push(&good);
        assert_eq!(dec.by_ref().count(), 0);
    }

    #[test]
    fn bad_frames_are_skipped_without_losing_sync() {
        let good = ControlMessage::new(Body::StreamStop(StreamStop { stream_id: 1 }));
        let mut bytes = Vec::new();
        // An empty frame (no body).
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        // Malformed protobuf: a truncated varint.
        bytes.extend_from_slice(&[0, 0, 0, 2, 0x08, 0xFF]);
        // An unknown oneof tag (field 99, varint 1): prost skips it -> empty body.
        bytes.extend_from_slice(&[0, 0, 0, 3, 0x98, 0x06, 0x01]);
        bytes.extend_from_slice(&encode_frame(&good).unwrap());
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        assert!(matches!(
            dec.next(),
            Some(Err(ProtoError::InvalidMessage(_)))
        ));
        assert!(matches!(dec.next(), Some(Err(ProtoError::Decode(_)))));
        assert!(matches!(
            dec.next(),
            Some(Err(ProtoError::InvalidMessage(_)))
        ));
        assert_eq!(dec.next(), Some(Ok(good)));
        assert_eq!(dec.next(), None);
        assert!(!dec.is_poisoned());
    }

    #[test]
    fn decoder_debug_hides_buffered_bytes() {
        let mut dec = FrameDecoder::new();
        dec.push(&[0, 0, 0, 40, 171, 171, 171]);
        let text = format!("{dec:?}");
        assert!(
            text.contains("buffered: 7") && !text.contains("171"),
            "{text}"
        );
    }

    proptest::proptest! {
        /// Any chunking of a frame stream yields exactly the original messages.
        #[test]
        fn random_chunking(cuts in proptest::collection::vec(0usize..2000, 0..40)) {
            let msgs = sample_messages();
            let bytes = stream_of(&msgs);
            let mut cuts: Vec<usize> = cuts.into_iter().map(|c| c % (bytes.len() + 1)).collect();
            cuts.push(0);
            cuts.push(bytes.len());
            cuts.sort_unstable();
            let mut dec = FrameDecoder::new();
            let mut got = Vec::new();
            for w in cuts.windows(2) {
                dec.push(&bytes[w[0]..w[1]]);
                for r in dec.by_ref() {
                    got.push(r.unwrap());
                }
            }
            proptest::prop_assert_eq!(got, msgs);
            proptest::prop_assert_eq!(dec.buffered(), 0);
        }

        /// Arbitrary bytes never panic the decoder, which always terminates.
        #[test]
        fn arbitrary_bytes_never_panic(chunks in proptest::collection::vec(
            proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64), 0..16)) {
            let mut dec = FrameDecoder::new();
            for chunk in &chunks {
                dec.push(chunk);
                // Every frame consumes at least its 4-byte prefix and a poisoned decoder
                // ends, so a plain drain always terminates.
                let n = dec.by_ref().count();
                proptest::prop_assert!(n <= chunks.concat().len() / FRAME_PREFIX_LEN + 1);
            }
            let _ = decode_message(&chunks.concat());
        }
    }

    #[test]
    fn hello_pairing_required_is_field_7() {
        let hello = Hello {
            pairing_required: true,
            ..Default::default()
        };
        // Field 7, wire type 0 (varint) = key 0x38, value 1.
        assert_eq!(hello.encode_to_vec(), vec![0x38, 0x01]);
    }
}
