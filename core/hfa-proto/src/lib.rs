//! # hfa-proto
//!
//! The headphone-for-all wire format. This crate is **pure (sans-IO)**: it never touches sockets,
//! threads, tokio or audio devices. It contains:
//!
//! - [`media`]: the 16-byte UDP media header ([`MediaHeader`]).
//! - [`crypto`]: ChaCha20-Poly1305 sealing of media datagrams ([`MediaSealer`], [`MediaOpener`]).
//! - [`replay`]: the sliding anti-replay window used by [`MediaOpener`].
//! - [`control`]: prost control messages ([`ControlMessage`]) and length-prefixed framing.
//! - [`noise`]: the Noise XX handshake that protects the TCP control channel.
//! - [`pairing`]: SPAKE2 PIN/token pairing bound to the Noise handshake hash.
//! - [`uri`]: the `hfa://pair?...` pairing URI shown as a QR code.
//! - [`identity`]: device fingerprints (`device_id`).
//!
//! See `docs/CONTRACTS.md` §3 for the binding contract.

#![forbid(unsafe_code)]

pub mod control;
pub mod crypto;
pub mod error;
pub mod identity;
pub mod media;
pub mod noise;
pub mod pairing;
pub mod replay;
pub mod uri;

pub use control::{encode_frame, ControlMessage, FrameDecoder};
pub use crypto::{MediaKey, MediaOpener, MediaSealer};
pub use error::ProtoError;
pub use identity::fingerprint;
pub use media::{MediaHeader, FLAG_DTX, FLAG_FEC, FLAG_RESET};
pub use noise::{NoiseHandshake, NoiseTransport, StaticKeypair, NOISE_PATTERN};
pub use pairing::{generate_pin, generate_token, PairingKey, PairingRole, PairingSession};
pub use replay::{ReplayWindow, REPLAY_WINDOW};
pub use uri::PairingUri;

/// Result type used throughout this crate.
pub type Result<T> = std::result::Result<T, ProtoError>;

/// Protocol version carried in every media header and in `Hello`.
pub const PROTOCOL_VERSION: u8 = 0;
/// Default port. TCP control and UDP media use the same number.
pub const DEFAULT_PORT: u16 = 47810;
/// DNS-SD service type advertised by hubs.
pub const SERVICE_TYPE: &str = "_hfa._tcp.local.";
/// Magic bytes at the start of every media datagram ("HF").
pub const MAGIC: [u8; 2] = *b"HF";
/// Length of the plaintext media header in bytes.
pub const MEDIA_HEADER_LEN: usize = 16;
/// Length of the ChaCha20-Poly1305 authentication tag in bytes.
pub const AEAD_TAG_LEN: usize = 16;
/// Largest media datagram we ever send (fits a typical 1500-byte MTU with IP/UDP headers).
pub const MAX_DATAGRAM: usize = 1400;
/// Largest encoded control frame payload (below Noise's 65 535-byte message limit).
pub const MAX_CONTROL_FRAME: usize = 65_000;
