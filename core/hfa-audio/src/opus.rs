//! Opus codec wrapper over `opusic-sys` (libopus built statically from bundled source).
//!
//! PCM is interleaved `f32`. "Frame size" follows libopus terminology: samples **per channel**.

use serde::{Deserialize, Serialize};

use crate::Result;

/// Maximum size of one Opus packet in bytes (RFC 6716).
pub const MAX_OPUS_PACKET: usize = 1275;

/// Encoder configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpusConfig {
    /// 8 000, 12 000, 16 000, 24 000 or 48 000 Hz.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u16,
    /// Target bitrate in bits per second (6 000..=510 000).
    pub bitrate: u32,
    /// Frame duration in ms: 10 or 20 (5, 40, 60 are also accepted by libopus).
    pub frame_ms: u32,
    /// Enable in-band forward error correction.
    pub fec: bool,
    /// Expected packet loss in percent (0..=100); tunes FEC.
    pub expected_loss_pct: u8,
    /// Use `OPUS_APPLICATION_RESTRICTED_LOWDELAY` instead of `OPUS_APPLICATION_AUDIO`.
    pub low_delay: bool,
}

impl Default for OpusConfig {
    /// 48 kHz stereo, 128 kbit/s, 10 ms, FEC on with 5 % expected loss, `APPLICATION_AUDIO`.
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            bitrate: 128_000,
            frame_ms: 10,
            fec: true,
            expected_loss_pct: 5,
            low_delay: false,
        }
    }
}

/// Opus encoder. `Send` (moved into the sender's encoder thread), not `Sync`.
pub struct OpusEncoder {
    config: OpusConfig,
}

impl OpusEncoder {
    /// Creates an encoder.
    ///
    /// # Errors
    /// [`crate::AudioError::InvalidConfig`] or [`crate::AudioError::Opus`].
    pub fn new(_config: OpusConfig) -> Result<Self> {
        todo!("feat/audio")
    }

    /// The configuration the encoder currently uses (reflects `set_*` calls).
    pub fn config(&self) -> &OpusConfig {
        &self.config
    }

    /// Encodes exactly one frame (`frame_samples() * channels` interleaved samples) into `out`
    /// and returns the packet length in bytes. `out` should be at least [`MAX_OPUS_PACKET`].
    ///
    /// # Errors
    /// [`crate::AudioError::BufferSize`] or [`crate::AudioError::Opus`].
    pub fn encode(&mut self, _pcm: &[f32], _out: &mut [u8]) -> Result<usize> {
        todo!("feat/audio")
    }

    /// Changes the target bitrate (bits per second).
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] if libopus rejects the value.
    pub fn set_bitrate(&mut self, _bitrate: u32) -> Result<()> {
        todo!("feat/audio")
    }

    /// Changes the expected packet loss percentage (0..=100).
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] if libopus rejects the value.
    pub fn set_expected_loss(&mut self, _pct: u8) -> Result<()> {
        todo!("feat/audio")
    }

    /// Enables or disables in-band FEC.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] if libopus rejects the value.
    pub fn set_fec(&mut self, _enabled: bool) -> Result<()> {
        todo!("feat/audio")
    }

    /// Frame size in samples **per channel** (e.g. 480 for 10 ms at 48 kHz).
    pub fn frame_samples(&self) -> usize {
        todo!("feat/audio")
    }
}

/// Opus decoder with FEC recovery and packet-loss concealment. `Send`, not `Sync`.
pub struct OpusDecoder {
    sample_rate: u32,
    channels: u16,
}

impl OpusDecoder {
    /// Creates a decoder producing `sample_rate`/`channels` interleaved `f32`.
    ///
    /// # Errors
    /// [`crate::AudioError::InvalidConfig`] or [`crate::AudioError::Opus`].
    pub fn new(_sample_rate: u32, _channels: u16) -> Result<Self> {
        todo!("feat/audio")
    }

    /// Output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Output channel count.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Decodes one packet into `out` (interleaved) and returns the number of frames
    /// (samples per channel) written. `out` must hold at least one full frame.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] on a corrupt packet.
    pub fn decode(&mut self, _packet: &[u8], _out: &mut [f32]) -> Result<usize> {
        todo!("feat/audio")
    }

    /// Recovers the *previous*, lost frame from the FEC data in `next_packet`. The number of
    /// frames recovered equals `out.len() / channels`. Returns frames written.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`].
    pub fn decode_fec(&mut self, _next_packet: &[u8], _out: &mut [f32]) -> Result<usize> {
        todo!("feat/audio")
    }

    /// Packet-loss concealment: synthesizes `out.len() / channels` frames. Returns frames
    /// written.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`].
    pub fn conceal(&mut self, _out: &mut [f32]) -> Result<usize> {
        todo!("feat/audio")
    }

    /// Resets decoder state (after a stream reset).
    pub fn reset(&mut self) {
        todo!("feat/audio")
    }
}
