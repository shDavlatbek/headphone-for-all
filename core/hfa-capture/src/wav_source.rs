//! WAV-file capture source: plays a file in real time on a background thread, looping.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hfa_audio::{AudioError, AudioFormat};

use crate::pacer::{PacedThread, Pacer};
use crate::ring::PcmSink;
use crate::tone::BLOCK_MS;
use crate::{CaptureError, CaptureSource, Result};

/// Maps a `hound` error: I/O failures become [`CaptureError::Io`], everything else (bad
/// header, unsupported encoding) [`CaptureError::Audio`]`(`[`AudioError::Wav`]`)`.
pub(crate) fn wav_error(path: &Path, e: hound::Error) -> CaptureError {
    match e {
        hound::Error::IoError(io) => CaptureError::Io(format!("{}: {io}", path.display())),
        other => CaptureError::Audio(AudioError::Wav(format!("{}: {other}", path.display()))),
    }
}

/// Reads a whole WAV file (8/16/24/32-bit integer or 32-bit float PCM) as interleaved `f32`.
fn read_file(path: &Path) -> Result<(AudioFormat, Vec<f32>)> {
    let reader = hound::WavReader::open(path).map_err(|e| wav_error(path, e))?;
    let spec = reader.spec();
    let format = AudioFormat::new(spec.sample_rate, spec.channels);
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<Vec<_>, _>>()
        }
    }
    .map_err(|e| wav_error(path, e))?;
    Ok((format, samples))
}

/// Streams a WAV file into the sink at real-time pace (10 ms blocks), looping at the end.
///
/// The samples are delivered in the file's own format ([`CaptureSource::format`]); the sender
/// converts them to the internal format.
pub struct WavFileSource {
    path: PathBuf,
    format: AudioFormat,
    samples: Arc<[f32]>,
    worker: PacedThread,
}

impl WavFileSource {
    /// Opens and fully reads `path` (so `format` is known before `start`).
    ///
    /// # Errors
    /// [`CaptureError::Audio`] / [`CaptureError::Io`] if the file cannot be read,
    /// [`CaptureError::Format`] if it has no channels, a zero sample rate or no audio.
    pub fn open(path: &Path) -> Result<Self> {
        let (format, samples) = read_file(path)?;
        if format.channels == 0 || format.sample_rate == 0 {
            return Err(CaptureError::Format(format!(
                "{}: invalid WAV format {format:?}",
                path.display()
            )));
        }
        let whole = samples.len() / usize::from(format.channels) * usize::from(format.channels);
        if whole == 0 {
            return Err(CaptureError::Format(format!(
                "{}: WAV file contains no audio",
                path.display()
            )));
        }
        tracing::debug!(path = %path.display(), ?format, frames = whole / usize::from(format.channels), "opened WAV source");
        Ok(Self {
            path: path.to_owned(),
            format,
            samples: samples[..whole].into(),
            worker: PacedThread::default(),
        })
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Length of the file in frames.
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.format.channels)
    }
}

impl CaptureSource for WavFileSource {
    fn describe(&self) -> String {
        format!("WAV file {}", self.path.display())
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, mut sink: PcmSink) -> Result<()> {
        let format = self.format;
        let samples = Arc::clone(&self.samples);
        self.worker.spawn("hfa-wav-source", move |stop| {
            let mut pacer = Pacer::new(format.sample_rate, format.frames_for_ms(BLOCK_MS));
            let mut block = vec![0.0f32; pacer.block_frames() * usize::from(format.channels)];
            let mut pos = 0usize;
            while pacer.wait_next(&stop) {
                // Fill the block from the file, wrapping around (the file may be shorter
                // than a block).
                let mut filled = 0;
                while filled < block.len() {
                    let n = (block.len() - filled).min(samples.len() - pos);
                    block[filled..filled + n].copy_from_slice(&samples[pos..pos + n]);
                    filled += n;
                    pos = (pos + n) % samples.len();
                }
                sink.push(&block);
            }
        })
    }

    fn stop(&mut self) {
        self.worker.stop();
    }
}

impl Drop for WavFileSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::pacer::assert_real_time;
    use crate::ring::pcm_ring_with_channels;

    fn write_wav(path: &Path, spec: hound::WavSpec, samples: &[f32]) {
        let mut w = hound::WavWriter::create(path, spec).expect("create");
        for &s in samples {
            match spec.sample_format {
                hound::SampleFormat::Float => w.write_sample(s).expect("write"),
                hound::SampleFormat::Int => w
                    .write_sample((s * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
                    .expect("write"),
            }
        }
        w.finalize().expect("finalize");
    }

    #[test]
    fn plays_a_float_file_in_real_time_and_loops() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("in.wav");
        // 25 ms of distinct stereo samples at 8 kHz (200 frames): shorter than 100 ms of
        // playback, so the source must loop, and not a multiple of the 80-frame block.
        let content: Vec<f32> = (0..400).map(|i| (i as f32 - 200.0) / 256.0).collect();
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        write_wav(&path, spec, &content);

        let mut src = WavFileSource::open(&path).expect("open");
        assert_eq!(src.format(), AudioFormat::new(8000, 2));
        assert_eq!(src.frames(), 200);
        assert!(src.describe().contains("in.wav"));
        let (sink, mut ring) = pcm_ring_with_channels(16_000, 2);
        let t0 = Instant::now();
        src.start(sink).expect("start");
        std::thread::sleep(Duration::from_millis(500));
        src.stop();
        let elapsed = t0.elapsed();

        let got = ring.available();
        // ~500 ms at 8 kHz stereo = ~4000 frames, in whole 10 ms (80-frame) blocks.
        assert_eq!(got % 160, 0);
        assert_real_time(got / 2, 8000, elapsed, 80, 0.05);
        let mut buf = vec![0.0; got];
        ring.pull(&mut buf);
        for (i, v) in buf.iter().enumerate() {
            assert_eq!(*v, content[i % content.len()], "sample {i}");
        }
    }

    #[test]
    fn reads_16_bit_integer_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pcm16.wav");
        let content = [0.5f32, -0.5, 0.25, -1.0, 0.0, 0.999];
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &content);
        let (format, samples) = read_file(&path).expect("read");
        assert_eq!(format, AudioFormat::new(16_000, 1));
        assert_eq!(samples.len(), content.len());
        for (a, b) in samples.iter().zip(content) {
            assert!((a - b).abs() < 1.0 / 32768.0 + 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn rejects_missing_empty_and_garbage_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            WavFileSource::open(&dir.path().join("missing.wav")),
            Err(CaptureError::Io(_))
        ));

        let empty = dir.path().join("empty.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        write_wav(&empty, spec, &[]);
        assert!(matches!(
            WavFileSource::open(&empty),
            Err(CaptureError::Format(_))
        ));

        let garbage = dir.path().join("garbage.wav");
        std::fs::write(&garbage, b"definitely not a RIFF file").expect("write");
        assert!(matches!(
            WavFileSource::open(&garbage),
            Err(CaptureError::Audio(AudioError::Wav(_)))
        ));
    }
}
