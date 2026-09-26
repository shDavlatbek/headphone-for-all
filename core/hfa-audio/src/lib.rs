//! # hfa-audio
//!
//! Pure DSP and codec building blocks (no networking, no device I/O):
//!
//! - [`format`](mod@format): [`AudioFormat`] (the internal format is 48 kHz stereo interleaved `f32`).
//! - [`convert`]: sample-format and channel conversion.
//! - [`opus`]: Opus encoder/decoder with FEC and PLC (libopus built from bundled source).
//! - [`jitter`]: adaptive jitter buffer.
//! - [`drift`]: PI clock-drift controller.
//! - [`resample`]: variable-ratio resampler (`rubato`).
//! - [`splice`]: inaudible latency cuts (pitch-aligned crossfade around discarded frames).
//! - [`mixer`]: per-source gain/mute, priority ducking, soft limiter.
//! - [`meter`]: peak/RMS level meters.
//! - [`tone`]: sine generator (tests, selftest).
//! - [`wav`]: WAV read/write helpers (`hound`).
//!
//! See `docs/CONTRACTS.md` §4 for the binding contract.

pub mod convert;
pub mod drift;
pub mod error;
pub mod format;
pub mod jitter;
pub mod meter;
pub mod mixer;
pub mod opus;
pub mod resample;
pub mod splice;
pub mod tone;
pub mod wav;

pub use drift::{DriftConfig, DriftController};
pub use error::AudioError;
pub use format::AudioFormat;
pub use jitter::{JitterBuffer, JitterConfig, JitterStats, Pop, PushResult};
pub use meter::{Level, LevelMeter};
pub use mixer::{Mixer, MixerConfig, SourceId};
pub use opus::{packet_has_fec, OpusConfig, OpusDecoder, OpusEncoder};
pub use resample::StreamResampler;
pub use splice::Splicer;
pub use tone::SineGenerator;

/// Result type used throughout this crate.
pub type Result<T> = std::result::Result<T, AudioError>;

// Compile-time guarantees other crates rely on: codec, resampler and mixer state is moved into
// the sender encoder thread / hub mixer thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<OpusEncoder>();
    assert_send::<OpusDecoder>();
    assert_send::<StreamResampler>();
    assert_send::<Mixer>();
    assert_send::<JitterBuffer>();
    assert_send::<DriftController>();
    assert_send::<Splicer>();
};
