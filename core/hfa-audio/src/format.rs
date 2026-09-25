//! Audio stream format.

use serde::{Deserialize, Serialize};

/// Sample rate and channel count of an interleaved `f32` stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Frames per second (e.g. 48 000).
    pub sample_rate: u32,
    /// Interleaved channels per frame (e.g. 2).
    pub channels: u16,
}

impl AudioFormat {
    /// The internal and network format: 48 kHz stereo.
    pub const INTERNAL: AudioFormat = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
    };

    /// Creates a format.
    pub const fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }

    /// Number of frames (samples per channel) in `ms` milliseconds, rounded down.
    pub fn frames_for_ms(&self, ms: u32) -> usize {
        (u64::from(self.sample_rate) * u64::from(ms) / 1000) as usize
    }

    /// Number of interleaved samples (frames × channels) in `ms` milliseconds.
    pub fn samples_for_ms(&self, ms: u32) -> usize {
        self.frames_for_ms(ms) * usize::from(self.channels)
    }
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::INTERNAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_frame_math() {
        let f = AudioFormat::INTERNAL;
        assert_eq!(f.frames_for_ms(10), 480);
        assert_eq!(f.frames_for_ms(20), 960);
        assert_eq!(f.samples_for_ms(10), 960);
        assert_eq!(AudioFormat::new(44_100, 1).frames_for_ms(10), 441);
    }
}
