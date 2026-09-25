//! Non-device outputs, paced in real time by a background thread: WAV file and null.
//! Used by `hfa hub --out wav:<path>|null` and the selftest.
//!
//! Both behave like a sound card as far as the hub can tell: every block period
//! (`buffer_ms`, clamped to 1..=100 ms) they pull one block from the ring, scheduled against a
//! monotonic clock without cumulative drift. When the ring runs dry the missing part is
//! zero-filled (counted as underruns) and, for the WAV output, written as silence — just like
//! a device would play it. So hub pacing works identically without audio hardware.
//!
//! A WAV file cannot hold more than 4 GiB of audio (32-bit RIFF sizes; ~3.1 h at
//! [`AudioFormat::INTERNAL`]). The WAV output finalizes the file just below that limit and then
//! keeps consuming (and discarding) the ring at the same pace.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use hfa_audio::AudioFormat;

use crate::pacer::{PacedThread, Pacer};
use crate::ring::{check_ring_channels, PcmSource};
use crate::wav_source::wav_error;
use crate::{AudioOutput, CaptureError, Result};

/// Block period used by the file outputs for a requested `buffer_ms`.
fn block_ms(buffer_ms: u32) -> u32 {
    buffer_ms.clamp(1, 100)
}

/// Largest `data` chunk the WAV output writes, in bytes. `hound` keeps the chunk size (and
/// the RIFF size = data + header) in a `u32` and overflows past it, so stay 1 KiB below.
const MAX_WAV_DATA_BYTES: u64 = u32::MAX as u64 - 1024;

/// Bytes per sample of the WAV output (32-bit float).
const WAV_BYTES_PER_SAMPLE: u64 = 4;

/// Spawns the pull thread: every block period, pull one block and hand it to `consume`.
fn spawn_puller<F>(
    worker: &mut PacedThread,
    name: &str,
    format: AudioFormat,
    buffer_ms: u32,
    mut source: PcmSource,
    mut consume: F,
) -> Result<()>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let block_frames = format.frames_for_ms(block_ms(buffer_ms)).max(1);
    worker.spawn(name, move |stop| {
        let mut pacer = Pacer::new(format.sample_rate, block_frames);
        let mut block = vec![0.0f32; block_frames * usize::from(format.channels.max(1))];
        while pacer.wait_next(&stop) {
            source.pull(&mut block);
            consume(&block);
        }
    })
}

/// Writes the pulled audio to a 32-bit float WAV file in real time.
pub struct WavFileOutput {
    path: PathBuf,
    format: AudioFormat,
    buffer_ms: u32,
    worker: PacedThread,
    /// Set by the pull thread when writing fails.
    error: Arc<AtomicBool>,
}

impl WavFileOutput {
    /// Creates the output (the file is created, or truncated, on `start`).
    ///
    /// # Errors
    /// [`CaptureError::Io`] if the parent directory does not exist,
    /// [`CaptureError::Format`] for a zero sample rate or channel count.
    pub fn create(path: &Path, format: AudioFormat, buffer_ms: u32) -> Result<Self> {
        if format.sample_rate == 0 || format.channels == 0 {
            return Err(CaptureError::Format(format!(
                "invalid WAV output format {format:?}"
            )));
        }
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if !parent.is_dir() {
            return Err(CaptureError::Io(format!(
                "{}: directory {} does not exist",
                path.display(),
                parent.display()
            )));
        }
        Ok(Self {
            path: path.to_owned(),
            format,
            buffer_ms,
            worker: PacedThread::default(),
            error: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pull block size in ms.
    pub fn buffer_ms(&self) -> u32 {
        self.buffer_ms
    }
}

impl AudioOutput for WavFileOutput {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, source: PcmSource) -> Result<()> {
        if self.worker.is_running() {
            return Err(CaptureError::AlreadyRunning);
        }
        check_ring_channels(&source, self.format)?;
        let spec = hound::WavSpec {
            channels: self.format.channels,
            sample_rate: self.format.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let path = self.path.clone();
        let writer: hound::WavWriter<BufWriter<File>> =
            hound::WavWriter::create(&path, spec).map_err(|e| wav_error(&path, e))?;
        self.error.store(false, Ordering::Relaxed);
        let mut writer = WavSink::new(
            writer,
            path,
            self.format.channels,
            MAX_WAV_DATA_BYTES,
            Arc::clone(&self.error),
        );
        spawn_puller(
            &mut self.worker,
            "hfa-wav-out",
            self.format,
            self.buffer_ms,
            source,
            move |block| writer.write(block),
        )
    }

    fn stop(&mut self) {
        // Joining drops the pull closure, which finalizes the file (see `WavSink::drop`).
        self.worker.stop();
    }

    fn latency_ms(&self) -> Option<f32> {
        Some(block_ms(self.buffer_ms) as f32)
    }

    /// `true` after a write error since `start` (disk full, file removed...). Reaching the WAV
    /// size limit is not an error: the file is complete and the output keeps pacing.
    fn has_error(&self) -> bool {
        self.error.load(Ordering::Relaxed)
    }

    /// `false`: `start` truncates the file, which would lose the recording so far.
    fn restartable(&self) -> bool {
        false
    }
}

impl Drop for WavFileOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The WAV writer owned by the pull thread. It stops writing after the first error (raising
/// the shared error flag) or once the size limit is reached, and finalizes the header when
/// dropped (or when the limit is reached).
struct WavSink {
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    path: PathBuf,
    /// Samples still allowed in the file (whole frames, below the RIFF size limit).
    remaining_samples: u64,
    error: Arc<AtomicBool>,
}

impl WavSink {
    fn new(
        writer: hound::WavWriter<BufWriter<File>>,
        path: PathBuf,
        channels: u16,
        max_data_bytes: u64,
        error: Arc<AtomicBool>,
    ) -> Self {
        let channels = u64::from(channels.max(1));
        Self {
            writer: Some(writer),
            path,
            remaining_samples: max_data_bytes / WAV_BYTES_PER_SAMPLE / channels * channels,
            error,
        }
    }

    fn write(&mut self, block: &[f32]) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let n = usize::try_from(self.remaining_samples)
            .unwrap_or(usize::MAX)
            .min(block.len());
        let mut result = Ok(());
        for &s in &block[..n] {
            result = writer.write_sample(s);
            if result.is_err() {
                break;
            }
        }
        if let Err(e) = result {
            tracing::error!(path = %self.path.display(), "WAV output write failed, dropping audio: {e}");
            self.error.store(true, Ordering::Relaxed);
            self.writer = None;
            return;
        }
        // `n <= remaining_samples`, both fit in u64.
        self.remaining_samples -= n as u64;
        if self.remaining_samples == 0 || n < block.len() {
            tracing::warn!(
                path = %self.path.display(),
                "WAV size limit (4 GiB) reached: file finalized, the rest of the audio is discarded"
            );
            self.finalize();
        }
    }

    /// Writes the final header and closes the file (no-op if already closed).
    fn finalize(&mut self) {
        if let Some(writer) = self.writer.take() {
            if let Err(e) = writer.finalize() {
                tracing::error!(path = %self.path.display(), "WAV output finalize failed: {e}");
                self.error.store(true, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        self.finalize();
    }
}

/// Discards the pulled audio in real time.
pub struct NullOutput {
    format: AudioFormat,
    buffer_ms: u32,
    worker: PacedThread,
}

impl NullOutput {
    /// Creates a null output.
    pub fn new(format: AudioFormat, buffer_ms: u32) -> Self {
        Self {
            format,
            buffer_ms,
            worker: PacedThread::default(),
        }
    }

    /// Pull block size in ms.
    pub fn buffer_ms(&self) -> u32 {
        self.buffer_ms
    }
}

impl AudioOutput for NullOutput {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, source: PcmSource) -> Result<()> {
        if self.worker.is_running() {
            return Err(CaptureError::AlreadyRunning);
        }
        check_ring_channels(&source, self.format)?;
        spawn_puller(
            &mut self.worker,
            "hfa-null-out",
            self.format,
            self.buffer_ms,
            source,
            |_| {},
        )
    }

    fn stop(&mut self) {
        self.worker.stop();
    }

    fn latency_ms(&self) -> Option<f32> {
        Some(block_ms(self.buffer_ms) as f32)
    }
}

impl Drop for NullOutput {
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
    use crate::{open_output, OutputTarget};

    #[test]
    fn null_output_consumes_at_real_time_pace() {
        let format = AudioFormat::INTERNAL;
        let mut out = open_output(&OutputTarget::Null, 10).expect("open");
        assert_eq!(out.format(), format);
        assert_eq!(out.latency_ms(), Some(10.0));
        let (mut sink, source) = pcm_ring_with_channels(format.samples_for_ms(3000), 2);
        let underruns = source.stats();
        // Fill 2 s of audio up front.
        let two_s = vec![0.1f32; format.samples_for_ms(2000)];
        assert_eq!(sink.push(&two_s), two_s.len());
        let t0 = Instant::now();
        out.start(source).expect("start");
        std::thread::sleep(Duration::from_millis(500));
        out.stop();
        let elapsed = t0.elapsed();
        let consumed_frames = (two_s.len() - (sink.capacity() - sink.free())) / 2;
        // ~500 ms = ~24000 frames, in whole 10 ms blocks.
        assert_real_time(consumed_frames, 48_000, elapsed, 480, 0.05);
        assert_eq!(underruns.count(), 0);

        // Stopped: consumption stops.
        let free = sink.free();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(sink.free(), free);
    }

    #[test]
    fn null_output_zero_fills_when_starved_and_rejects_double_start() {
        let mut out = NullOutput::new(AudioFormat::new(8000, 1), 5);
        let (_sink, source) = pcm_ring_with_channels(100, 1);
        let underruns = source.stats();
        let t0 = Instant::now();
        out.start(source).expect("start");
        let (_sink2, source2) = pcm_ring_with_channels(100, 1);
        assert_eq!(out.start(source2).err(), Some(CaptureError::AlreadyRunning));
        std::thread::sleep(Duration::from_millis(200));
        drop(out);
        // ~200 ms at 8 kHz mono = ~1600 samples zero-filled, in 5 ms (40-sample) blocks.
        let n = underruns.count() as usize;
        assert_eq!(n % 40, 0);
        assert_real_time(n, 8000, t0.elapsed(), 40, 0.05);
    }

    #[test]
    fn wav_output_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.wav");
        let format = AudioFormat::new(16_000, 2);
        let mut out = WavFileOutput::create(&path, format, 20).expect("create");
        assert_eq!(out.path(), path);
        assert_eq!(out.format(), format);

        // 100 ms of distinct samples, then the output runs dry and writes silence.
        let content: Vec<f32> = (0..format.samples_for_ms(100))
            .map(|i| ((i % 200) as f32 - 100.0) / 128.0)
            .collect();
        let (mut sink, source) = pcm_ring_with_channels(format.samples_for_ms(1000), 2);
        assert_eq!(sink.push(&content), content.len());
        let t0 = Instant::now();
        out.start(source).expect("start");
        std::thread::sleep(Duration::from_millis(300));
        out.stop();
        let elapsed = t0.elapsed();

        let mut reader = hound::WavReader::open(&path).expect("open written file");
        let spec = reader.spec();
        assert_eq!((spec.sample_rate, spec.channels), (16_000, 2));
        assert_eq!(spec.sample_format, hound::SampleFormat::Float);
        let written: Vec<f32> = reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .expect("samples");
        // Real-time length: ~300 ms of audio, in whole 20 ms blocks.
        assert_eq!(written.len() % format.samples_for_ms(20), 0);
        assert_real_time(written.len() / 2, 16_000, elapsed, 320, 0.05);
        assert_eq!(&written[..content.len()], &content[..]);
        assert!(written[content.len()..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn wav_output_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no/such/dir/out.wav");
        assert!(matches!(
            open_output(&OutputTarget::WavFile(missing), 10),
            Err(CaptureError::Io(_))
        ));
        assert!(matches!(
            WavFileOutput::create(&dir.path().join("x.wav"), AudioFormat::new(0, 2), 10),
            Err(CaptureError::Format(_))
        ));
        // A relative file name in the current directory is accepted.
        assert!(WavFileOutput::create(Path::new("x.wav"), AudioFormat::INTERNAL, 10).is_ok());
    }

    #[test]
    fn outputs_reject_a_ring_with_another_channel_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wav = WavFileOutput::create(&dir.path().join("x.wav"), AudioFormat::INTERNAL, 10)
            .expect("create");
        let (_s, mono) = pcm_ring_with_channels(64, 1);
        assert!(matches!(
            wav.start(mono),
            Err(CaptureError::InvalidArgument(_))
        ));
        assert!(!dir.path().join("x.wav").exists(), "nothing created");
        let mut null = NullOutput::new(AudioFormat::new(48_000, 6), 10);
        let (_s, stereo) = pcm_ring_with_channels(64, 2);
        assert!(matches!(
            null.start(stereo),
            Err(CaptureError::InvalidArgument(_))
        ));
        // The matching layout still starts.
        let (_s, six) = pcm_ring_with_channels(64, 6);
        null.start(six).expect("start");
        null.stop();
    }

    #[test]
    fn wav_sink_finalizes_a_readable_file_at_the_size_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("capped.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = hound::WavWriter::create(&path, spec).expect("create");
        let error = Arc::new(AtomicBool::new(false));
        // 83 bytes: room for 20 samples = 10 whole stereo frames (not 20.75 samples).
        let mut sink = WavSink::new(writer, path.clone(), 2, 83, Arc::clone(&error));
        let content: Vec<f32> = (0..32).map(|i| i as f32 / 64.0).collect();
        for block in content.chunks(8) {
            sink.write(block);
        }
        assert!(sink.writer.is_none(), "finalized at the limit");
        // Further blocks are consumed and discarded without error.
        sink.write(&content[..8]);
        drop(sink);
        assert!(!error.load(Ordering::Relaxed), "the limit is not an error");

        let mut reader = hound::WavReader::open(&path).expect("readable after the limit");
        let written: Vec<f32> = reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .expect("samples");
        assert_eq!(written, content[..20]);
    }

    #[test]
    fn default_cap_stays_below_the_riff_limit() {
        // Whole frames for every channel count, and data + the largest hound header fit a u32.
        for channels in [1u16, 2, 6, 8] {
            let c = u64::from(channels);
            let samples = MAX_WAV_DATA_BYTES / WAV_BYTES_PER_SAMPLE / c * c;
            let bytes = samples * WAV_BYTES_PER_SAMPLE;
            assert!(bytes + 1024 <= u64::from(u32::MAX));
            assert!(bytes > u64::from(u32::MAX) - 1024 - 64, "no needless waste");
        }
    }

    /// A write error (here: the Linux "disk full" device) raises `has_error` through the trait.
    #[cfg(target_os = "linux")]
    #[test]
    fn wav_write_errors_are_visible_through_the_trait() {
        let full = Path::new("/dev/full");
        if !full.exists() {
            eprintln!("no /dev/full: skipped");
            return;
        }
        let mut out = WavFileOutput::create(full, AudioFormat::INTERNAL, 10).expect("create");
        let (mut sink, source) =
            pcm_ring_with_channels(AudioFormat::INTERNAL.samples_for_ms(1000), 2);
        sink.push(&vec![0.1f32; AudioFormat::INTERNAL.samples_for_ms(500)]);
        let out_dyn: &mut dyn AudioOutput = &mut out;
        assert!(!out_dyn.has_error());
        assert!(!out_dyn.restartable(), "a restart would truncate the file");
        out_dyn.start(source).expect("start");
        // 10 ms blocks of 3840 bytes overflow the 8 KiB write buffer within a few blocks.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !out_dyn.has_error() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(out_dyn.has_error());
        out_dyn.stop();
    }
}
