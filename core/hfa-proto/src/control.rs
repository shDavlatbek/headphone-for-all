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
//!                 bool pairing_required = 7; }
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
//! ([`encode_frame`] / [`FrameDecoder`]). Frames larger than [`crate::MAX_CONTROL_FRAME`] are
//! rejected.

use crate::Result;

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
    /// Hub → sender only: `true` if the hub does not trust the sender's static key and the
    /// sender must pair before anything else. Senders send `false`; hubs ignore it. A sender
    /// that does not trust the hub pairs regardless of this flag (see `hfa-core` `control`).
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
pub fn encode_frame(_msg: &ControlMessage) -> Vec<u8> {
    todo!("feat/proto")
}

/// Incremental decoder for a byte stream of frames produced by [`encode_frame`].
///
/// Feed bytes with [`FrameDecoder::push`], then call [`Iterator::next`] until it returns
/// `None` (more bytes needed). A frame longer than [`crate::MAX_CONTROL_FRAME`] or an
/// undecodable/empty message yields `Some(Err(..))`.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Creates an empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends received bytes.
    pub fn push(&mut self, _data: &[u8]) {
        let _ = &self.buf;
        todo!("feat/proto")
    }

    /// Number of buffered bytes not yet consumed by a complete frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

impl Iterator for FrameDecoder {
    type Item = Result<ControlMessage>;

    /// Returns the next complete message, or `None` if more bytes are needed.
    fn next(&mut self) -> Option<Self::Item> {
        todo!("feat/proto")
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

    #[test]
    fn hello_pairing_required_is_field_7() {
        use prost::Message;
        let hello = Hello {
            pairing_required: true,
            ..Default::default()
        };
        // Field 7, wire type 0 (varint) = key 0x38, value 1.
        assert_eq!(hello.encode_to_vec(), vec![0x38, 0x01]);
    }
}
