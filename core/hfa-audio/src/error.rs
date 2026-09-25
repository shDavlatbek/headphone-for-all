//! The single error type of `hfa-audio`.

use thiserror::Error;

/// Every error `hfa-audio` can return.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AudioError {
    /// libopus returned an error code.
    #[error("opus error {code}: {message}")]
    Opus {
        /// The (negative) libopus error code.
        code: i32,
        /// `opus_strerror` text.
        message: String,
    },
    /// A configuration value is out of range (sample rate, channels, frame size, bitrate...).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// An input/output buffer has the wrong size.
    #[error("buffer size mismatch: expected {expected} samples, got {got}")]
    BufferSize {
        /// Required number of samples.
        expected: usize,
        /// Provided number of samples.
        got: usize,
    },
    /// The resampler failed.
    #[error("resampler error: {0}")]
    Resample(String),
    /// Reading or writing a WAV file failed.
    #[error("wav error: {0}")]
    Wav(String),
}
