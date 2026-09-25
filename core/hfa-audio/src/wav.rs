//! Thin helpers over `hound` for 32-bit float WAV files.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::error::AudioError;
use crate::format::AudioFormat;
use crate::Result;

fn wav_err(e: hound::Error) -> AudioError {
    AudioError::Wav(e.to_string())
}

/// Writes interleaved `f32` samples to a 32-bit float WAV file.
pub struct WavWriter {
    inner: hound::WavWriter<BufWriter<File>>,
    format: AudioFormat,
}

impl WavWriter {
    /// Creates (truncates) `path`.
    ///
    /// # Errors
    /// [`crate::AudioError::Wav`].
    pub fn create(path: &Path, format: AudioFormat) -> Result<Self> {
        if format.channels == 0 || format.sample_rate == 0 {
            return Err(AudioError::Wav(format!("invalid format {format:?}")));
        }
        let spec = hound::WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let inner = hound::WavWriter::create(path, spec).map_err(wav_err)?;
        Ok(Self { inner, format })
    }

    /// The file's format.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Appends interleaved samples.
    ///
    /// # Errors
    /// [`crate::AudioError::Wav`].
    pub fn write(&mut self, interleaved: &[f32]) -> Result<()> {
        for &s in interleaved {
            self.inner.write_sample(s).map_err(wav_err)?;
        }
        Ok(())
    }

    /// Flushes and finalizes the header.
    ///
    /// # Errors
    /// [`crate::AudioError::Wav`].
    pub fn finalize(self) -> Result<()> {
        self.inner.finalize().map_err(wav_err)
    }
}

/// Reads a whole WAV file (16/24/32-bit int or 32-bit float) as interleaved `f32` in [-1, 1].
///
/// Integer samples are scaled by `1 / 2^(bits−1)` (8-bit files are accepted too).
///
/// # Errors
/// [`crate::AudioError::Wav`].
pub fn read_wav(path: &Path) -> Result<(AudioFormat, Vec<f32>)> {
    let reader = hound::WavReader::open(path).map_err(wav_err)?;
    let spec = reader.spec();
    let format = AudioFormat::new(spec.sample_rate, spec.channels);
    let samples = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .into_samples::<f32>()
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(wav_err)?,
        (hound::SampleFormat::Int, bits @ 1..=32) => {
            let scale = 1.0 / (1_u64 << (bits - 1)) as f64;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| (f64::from(v) * scale) as f32))
                .collect::<std::result::Result<Vec<f32>, _>>()
                .map_err(wav_err)?
        }
        (fmt, bits) => {
            return Err(AudioError::Wav(format!(
                "unsupported sample format {fmt:?} with {bits} bits"
            )))
        }
    };
    Ok((format, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let fmt = AudioFormat::new(44_100, 2);
        let data: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.01).sin() * 0.9).collect();
        let mut w = WavWriter::create(&path, fmt).unwrap();
        assert_eq!(w.format(), fmt);
        w.write(&data[..300]).unwrap();
        w.write(&data[300..]).unwrap();
        w.finalize().unwrap();
        let (f, back) = read_wav(&path).unwrap();
        assert_eq!(f, fmt);
        assert_eq!(back, data);
    }

    #[test]
    fn reads_int16_and_int24() {
        let dir = tempfile::tempdir().unwrap();
        for (bits, full) in [(16_u16, 32_767_i32), (24, 8_388_607)] {
            let path = dir.path().join(format!("i{bits}.wav"));
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: bits,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for v in [0, full, -full - 1, full / 2] {
                w.write_sample(v).unwrap();
            }
            w.finalize().unwrap();
            let (f, s) = read_wav(&path).unwrap();
            assert_eq!(f, AudioFormat::new(48_000, 1));
            assert_eq!(s.len(), 4);
            assert_eq!(s[0], 0.0);
            assert!((s[1] - 1.0).abs() < 1e-4);
            assert_eq!(s[2], -1.0);
            assert!((s[3] - 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.wav");
        assert!(matches!(read_wav(&missing), Err(AudioError::Wav(_))));
        let bad = dir.path().join("bad.wav");
        std::fs::write(&bad, b"not a wav file").unwrap();
        assert!(matches!(read_wav(&bad), Err(AudioError::Wav(_))));
        assert!(matches!(
            WavWriter::create(&dir.path().join("x.wav"), AudioFormat::new(48_000, 0)),
            Err(AudioError::Wav(_))
        ));
    }
}
