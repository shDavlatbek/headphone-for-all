//! WAV-file capture source: plays a file in real time on a background thread, looping.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use hfa_audio::{AudioError, AudioFormat};

use crate::pacer::{PacedThread, Pacer};
use crate::ring::PcmSink;
use crate::tone::{block_period, BLOCK_MS};
use crate::{CaptureError, CaptureSource, Result};

/// Maps a `hound` error: I/O failures become [`CaptureError::Io`], everything else (bad
/// header, unsupported encoding) [`CaptureError::Audio`]`(`[`AudioError::Wav`]`)`.
pub(crate) fn wav_error(path: &Path, e: hound::Error) -> CaptureError {
    match e {
        hound::Error::IoError(io) => CaptureError::Io(format!("{}: {io}", path.display())),
        other => CaptureError::Audio(AudioError::Wav(format!("{}: {other}", path.display()))),
    }
}

/// Samples decoded per read from the file (a whole number of frames is taken from it).
const DECODE_CHUNK: usize = 4096;

/// Reads a WAV file (8/16/24/32-bit integer or 32-bit float PCM) as interleaved `f32`, a block
/// at a time, starting over at the end. Only one block is in memory, whatever the file's
/// length (a long recording would otherwise take 4 bytes per sample, gigabytes for an hour).
struct LoopingReader {
    path: PathBuf,
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    spec: hound::WavSpec,
    /// Samples per pass: the file's whole frames (a trailing partial frame is skipped).
    whole: usize,
    /// Samples read in the current pass.
    pos: usize,
    /// Integer samples are decoded as `i32` and scaled by this.
    int_scale: f32,
    /// Integer decode buffer.
    ints: Vec<i32>,
}

impl LoopingReader {
    fn open(path: &Path) -> Result<Self> {
        let reader = hound::WavReader::open(path).map_err(|e| wav_error(path, e))?;
        let spec = reader.spec();
        let channels = usize::from(spec.channels.max(1));
        let whole = reader.len() as usize / channels * channels;
        Ok(Self {
            path: path.to_owned(),
            reader,
            spec,
            whole,
            pos: 0,
            int_scale: 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32,
            ints: Vec::new(),
        })
    }

    fn format(&self) -> AudioFormat {
        AudioFormat::new(self.spec.sample_rate, self.spec.channels)
    }

    /// Starts the next pass from the beginning of the file (opened again: the reader cannot
    /// seek reliably at every bit depth). The file must not have changed its format.
    fn rewind(&mut self) -> Result<()> {
        let again = Self::open(&self.path)?;
        if again.spec != self.spec || again.whole != self.whole {
            return Err(CaptureError::Format(format!(
                "{}: the WAV file changed while it was playing",
                self.path.display()
            )));
        }
        *self = again;
        Ok(())
    }

    /// Fills `out` with the next samples, going round the file as often as needed.
    fn fill(&mut self, out: &mut [f32]) -> Result<()> {
        if self.whole == 0 {
            return Err(CaptureError::Format(format!(
                "{}: WAV file contains no audio",
                self.path.display()
            )));
        }
        let mut filled = 0;
        while filled < out.len() {
            if self.pos == self.whole {
                self.rewind()?;
            }
            let n = (out.len() - filled)
                .min(self.whole - self.pos)
                .min(DECODE_CHUNK);
            self.decode(&mut out[filled..filled + n])?;
            filled += n;
            self.pos += n;
        }
        Ok(())
    }

    /// Decodes exactly `out.len()` samples (they exist: the caller stays within `whole`).
    fn decode(&mut self, out: &mut [f32]) -> Result<()> {
        let path = &self.path;
        let truncated = || {
            CaptureError::Audio(AudioError::Wav(format!(
                "{}: the WAV file ends before its declared length",
                path.display()
            )))
        };
        match self.spec.sample_format {
            hound::SampleFormat::Float => {
                let mut samples = self.reader.samples::<f32>();
                for o in out.iter_mut() {
                    *o = samples
                        .next()
                        .ok_or_else(truncated)?
                        .map_err(|e| wav_error(path, e))?;
                }
            }
            hound::SampleFormat::Int => {
                self.ints.clear();
                let mut samples = self.reader.samples::<i32>();
                for _ in 0..out.len() {
                    let v = samples
                        .next()
                        .ok_or_else(truncated)?
                        .map_err(|e| wav_error(path, e))?;
                    self.ints.push(v);
                }
                for (o, &v) in out.iter_mut().zip(&self.ints) {
                    *o = v as f32 * self.int_scale;
                }
            }
        }
        Ok(())
    }
}

/// Streams a WAV file into the sink at real-time pace (10 ms blocks), looping at the end.
///
/// The samples are delivered in the file's own format ([`CaptureSource::format`]); the sender
/// converts them to the internal format. The file is read while it plays (one block in
/// memory), so its length does not matter. A read error while playing (the file was
/// truncated or replaced by one in another format) ends the playback and is reported by
/// [`CaptureSource::error`].
pub struct WavFileSource {
    path: PathBuf,
    format: AudioFormat,
    frames: usize,
    /// The reader checked by `open`, used by the first `start`.
    reader: Option<LoopingReader>,
    worker: PacedThread,
    /// Set by the worker when reading failed.
    failed: Arc<OnceLock<String>>,
}

impl WavFileSource {
    /// Opens `path` and decodes its first block (so `format` is known before `start` and a
    /// broken file fails here).
    ///
    /// # Errors
    /// [`CaptureError::Audio`] / [`CaptureError::Io`] if the file cannot be read,
    /// [`CaptureError::Format`] if it has no channels, a zero sample rate or no audio.
    pub fn open(path: &Path) -> Result<Self> {
        let mut reader = LoopingReader::open(path)?;
        let format = reader.format();
        if format.channels == 0 || format.sample_rate == 0 {
            return Err(CaptureError::Format(format!(
                "{}: invalid WAV format {format:?}",
                path.display()
            )));
        }
        let frames = reader.whole / usize::from(format.channels);
        if frames == 0 {
            return Err(CaptureError::Format(format!(
                "{}: WAV file contains no audio",
                path.display()
            )));
        }
        let first = format.samples_for_ms(BLOCK_MS).min(reader.whole);
        reader.fill(&mut vec![0.0; first])?;
        reader.rewind()?;
        tracing::debug!(path = %path.display(), ?format, frames, "opened WAV source");
        Ok(Self {
            path: path.to_owned(),
            format,
            frames,
            reader: Some(reader),
            worker: PacedThread::default(),
            failed: Arc::new(OnceLock::new()),
        })
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Length of the file in frames.
    pub fn frames(&self) -> usize {
        self.frames
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
        if self.worker.is_running() {
            return Err(CaptureError::AlreadyRunning);
        }
        let format = self.format;
        let mut reader = match self.reader.take() {
            Some(reader) => reader,
            None => {
                // Started again after `stop`: from the beginning, in the same format.
                let reader = LoopingReader::open(&self.path)?;
                if reader.format() != format {
                    return Err(CaptureError::Format(format!(
                        "{}: the WAV file changed its format since it was opened",
                        self.path.display()
                    )));
                }
                reader
            }
        };
        self.failed = Arc::new(OnceLock::new());
        let failed = Arc::clone(&self.failed);
        self.worker
            .spawn("hfa-wav-source", block_period(), move |stop| {
                let mut pacer = Pacer::new(format.sample_rate, format.frames_for_ms(BLOCK_MS));
                let mut block = vec![0.0f32; pacer.block_frames() * usize::from(format.channels)];
                while pacer.wait_next(&stop) {
                    if let Err(e) = reader.fill(&mut block) {
                        tracing::warn!(error = %e, "WAV source cannot read its file; it stops");
                        let _ = failed.set(e.to_string());
                        return;
                    }
                    sink.push(&block);
                }
            })
    }

    fn stop(&mut self) {
        self.worker.stop();
    }

    fn error(&self) -> Option<String> {
        self.failed.get().cloned()
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
    use crate::pacer::{assert_not_ahead_of_real_time, assert_paced_in_real_time};
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
        // Room for 4 s: the measurement below runs for about 1 s.
        let (sink, mut ring) = pcm_ring_with_channels(64_000, 2);
        let overruns = sink.stats();
        let t0 = Instant::now();
        src.start(sink).expect("start");
        // 10 ms (80-frame) blocks, produced in real time while the thread runs.
        assert_paced_in_real_time(
            || ring.available() / 2,
            8000,
            80,
            Duration::from_secs(1),
            0.05,
        );
        src.stop();
        let elapsed = t0.elapsed();

        let got = ring.available();
        assert_eq!(overruns.count(), 0);
        assert_eq!(got % 160, 0, "whole 10 ms blocks");
        assert_not_ahead_of_real_time(got / 2, 8000, elapsed);
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
        let mut reader = LoopingReader::open(&path).expect("open");
        assert_eq!(reader.format(), AudioFormat::new(16_000, 1));
        let mut samples = vec![0.0; content.len()];
        reader.fill(&mut samples).expect("read");
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

    #[test]
    fn long_files_stream_block_by_block_and_loop_exactly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("long.wav");
        // 3 channels, 5003 frames (15009 samples): several decode chunks per pass, and a pass
        // that is not a multiple of the block.
        let content: Vec<f32> = (0..15_009).map(|i| (i % 1000) as f32 / 1000.0).collect();
        let spec = hound::WavSpec {
            channels: 3,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        write_wav(&path, spec, &content);
        let src = WavFileSource::open(&path).expect("open");
        assert_eq!(src.frames(), 5003);
        let mut reader = LoopingReader::open(&path).expect("open");
        let mut block = vec![0.0; 480 * 3];
        let mut expected = content.iter().cycle();
        for pass in 0..40 {
            reader.fill(&mut block).expect("fill");
            for (i, v) in block.iter().enumerate() {
                assert_eq!(Some(v), expected.next(), "block {pass} sample {i}");
            }
        }
        // Only one block (plus a decode buffer) is held, not the file.
        assert!(reader.ints.capacity() <= DECODE_CHUNK);
    }

    #[test]
    fn a_file_that_breaks_while_playing_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cut.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &vec![0.25; 8000 * 2 * 4]);
        let mut src = WavFileSource::open(&path).expect("open");
        // Cut the file short behind the source's back (it declares 4 s of audio).
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for writing")
            .set_len(44 + 400)
            .expect("truncate");
        let (sink, _ring) = pcm_ring_with_channels(8000 * 2 * 8, 2);
        src.start(sink).expect("start");
        assert_eq!(src.error(), None);
        let t0 = Instant::now();
        while src.error().is_none() {
            assert!(t0.elapsed() < Duration::from_secs(3), "no error reported");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            src.error().is_some_and(|e| e.contains("cut.wav")),
            "{:?}",
            src.error()
        );
        src.stop();
    }

    #[test]
    fn starts_again_from_the_beginning_after_stop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("again.wav");
        let content: Vec<f32> = (0..8000).map(|i| i as f32 / 8000.0).collect();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        write_wav(&path, spec, &content);
        let mut src = WavFileSource::open(&path).expect("open");
        for _ in 0..2 {
            let (sink, mut ring) = pcm_ring_with_channels(8000, 1);
            src.start(sink).expect("start");
            let (other, _) = pcm_ring_with_channels(8, 1);
            assert_eq!(src.start(other), Err(CaptureError::AlreadyRunning));
            let t0 = Instant::now();
            while ring.available() < 80 {
                assert!(t0.elapsed() < Duration::from_secs(2), "nothing played");
                std::thread::sleep(Duration::from_millis(5));
            }
            src.stop();
            let mut first = vec![0.0; 80];
            ring.pull(&mut first);
            assert_eq!(first, content[..80]);
            assert_eq!(src.error(), None);
        }
    }
}
