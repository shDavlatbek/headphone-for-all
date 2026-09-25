//! The hub mixer: per-source gain and mute, a master gain, priority ducking and a soft
//! limiter. All gain changes are ramped over one frame so there are no clicks.
//!
//! Ducking: while any priority source's level is above `duck_threshold_db`, every
//! non-priority source is attenuated by `duck_db`, with `attack_ms`/`release_ms` smoothing.

use serde::{Deserialize, Serialize};

use crate::meter::Level;

/// Identifier of a mixer input (the hub uses the stream id).
pub type SourceId = u32;

/// Mixer configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MixerConfig {
    /// Sample rate in Hz (needed for attack/release time constants).
    pub sample_rate: u32,
    /// Interleaved channel count of inputs and output.
    pub channels: u16,
    /// Attenuation applied to non-priority sources while ducking, in dB (negative).
    pub duck_db: f32,
    /// Level above which a priority source triggers ducking, in dBFS.
    pub duck_threshold_db: f32,
    /// Ducking attack time in ms.
    pub attack_ms: f32,
    /// Ducking release time in ms.
    pub release_ms: f32,
    /// Enable the soft limiter on the master bus.
    pub limiter: bool,
}

impl Default for MixerConfig {
    /// 48 kHz stereo, duck −12 dB above −40 dBFS, attack 10 ms, release 300 ms, limiter on.
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            duck_db: -12.0,
            duck_threshold_db: -40.0,
            attack_ms: 10.0,
            release_ms: 300.0,
            limiter: true,
        }
    }
}

/// The mixer. Runs on the hub mixer thread.
#[derive(Debug)]
pub struct Mixer {
    config: MixerConfig,
    frame_frames: usize,
}

impl Mixer {
    /// Creates a mixer processing `frame_frames` frames per [`Mixer::mix`] call.
    pub fn new(config: MixerConfig, frame_frames: usize) -> Self {
        Self {
            config,
            frame_frames,
        }
    }

    /// The configuration.
    pub fn config(&self) -> &MixerConfig {
        &self.config
    }

    /// Frames per mix call.
    pub fn frame_frames(&self) -> usize {
        self.frame_frames
    }

    /// Adds a source (gain 1.0, unmuted, not priority). No-op if it exists.
    pub fn add_source(&mut self, _id: SourceId) {
        todo!("feat/audio")
    }

    /// Removes a source. No-op if unknown.
    pub fn remove_source(&mut self, _id: SourceId) {
        todo!("feat/audio")
    }

    /// Sets a source's linear gain (clamped to 0.0..=4.0).
    pub fn set_gain(&mut self, _id: SourceId, _gain: f32) {
        todo!("feat/audio")
    }

    /// Mutes or unmutes a source.
    pub fn set_muted(&mut self, _id: SourceId, _muted: bool) {
        todo!("feat/audio")
    }

    /// Marks a source as priority (it ducks the others).
    pub fn set_priority(&mut self, _id: SourceId, _priority: bool) {
        todo!("feat/audio")
    }

    /// Sets the master linear gain (clamped to 0.0..=4.0).
    pub fn set_master_gain(&mut self, _gain: f32) {
        todo!("feat/audio")
    }

    /// Mixes one frame. Each input slice holds `frame_frames * channels` interleaved samples;
    /// sources without an input this tick contribute silence. `out` (same length) is
    /// overwritten. Must not allocate.
    pub fn mix(&mut self, _inputs: &[(SourceId, &[f32])], _out: &mut [f32]) {
        todo!("feat/audio")
    }

    /// Post-gain level of every source from the last [`Mixer::mix`] call.
    pub fn levels(&self) -> Vec<(SourceId, Level)> {
        todo!("feat/audio")
    }

    /// Level of the master output from the last [`Mixer::mix`] call.
    pub fn master_level(&self) -> Level {
        todo!("feat/audio")
    }
}
