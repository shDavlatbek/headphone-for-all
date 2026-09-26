//! Device playback through `cpal` (WASAPI, Core Audio, ALSA, AAudio) on the default host.
//!
//! - [`CpalOutput::open`] picks the device (default or by name) and a config: 48 kHz `f32`
//!   (stereo if possible) when the device supports it, else the device's default config. The
//!   chosen rate/channels are reported by [`AudioOutput::format`]; the hub resamples to them.
//! - The stream lives on a dedicated thread (`cpal::Stream` is `!Send` on some platforms), so
//!   [`CpalOutput`] is `Send`.
//! - The data callback only pulls whole frames from the [`PcmSource`] and converts them to the
//!   device sample format (`f32`, `f64`, `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`).
//!   It never allocates, locks, logs or blocks; it talks to the rest of the program through
//!   atomics and a one-slot `rtrb` hand-off ring only.
//! - Fatal stream errors (device unplugged, stream invalidated) raise an atomic flag that the
//!   owner reads through [`AudioOutput::has_error`]. Xruns are counted
//!   ([`AudioOutput::xruns`]); route changes the backend handled itself (cpal
//!   `ErrorKind::DeviceChanged`: the stream was rerouted and keeps playing, e.g. when a headphone
//!   becomes the default output) are counted in [`CpalOutput::device_changes`] and are not
//!   errors.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, ErrorKind, FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig,
    SupportedBufferSize,
};
use hfa_audio::AudioFormat;

use crate::ring::{check_ring_channels, PcmSource};
use crate::{AudioOutput, CaptureError, Result};

/// Preferred device sample rate (the internal rate: no resampling in the hub).
const PREFERRED_RATE: u32 = 48_000;

/// Frames converted per chunk for non-`f32` devices (scratch buffer, preallocated).
const SCRATCH_FRAMES: usize = 4096;

/// How long `start` waits for the stream thread to build and start the stream.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Plays audio on the default or a named output device.
pub struct CpalOutput {
    device_name: String,
    /// `true` if opened as the default device (re-resolved on `start`).
    is_default: bool,
    format: AudioFormat,
    sample_format: SampleFormat,
    buffer_size: BufferSize,
    buffer_ms: u32,
    shared: Arc<Shared>,
    running: Option<Running>,
}

/// State shared with the stream thread and the callbacks (atomics only).
#[derive(Debug, Default)]
struct Shared {
    /// Set by the error callback on a fatal stream error.
    stream_error: AtomicBool,
    /// Buffer under/overruns reported by the backend.
    xruns: AtomicU64,
    /// Route changes the backend handled without interrupting the stream.
    device_changes: AtomicU64,
    /// Last measured callback → playback delay, in µs (0 = unknown).
    playback_delay_us: AtomicU32,
    /// Device buffer size in frames reported by the stream (0 = unknown).
    buffer_frames: AtomicU32,
}

/// The stream thread of a started output.
struct Running {
    stop_tx: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

fn backend(context: &str, e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(format!("{context}: {e}"))
}

/// Maps a cpal error to the matching [`CaptureError`] variant.
fn cpal_error(context: &str, e: &cpal::Error) -> CaptureError {
    let msg = format!("{context}: {e}");
    match e.kind() {
        ErrorKind::DeviceNotAvailable => CaptureError::NotFound(msg),
        ErrorKind::PermissionDenied => CaptureError::PermissionDenied(msg),
        ErrorKind::UnsupportedConfig => CaptureError::Format(msg),
        _ => CaptureError::Backend(msg),
    }
}

/// Whether a stream error means the stream no longer plays (as opposed to a glitch, or a route
/// change after which cpal documents that "the stream remains active and no rebuild is
/// required").
fn is_fatal(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Xrun | ErrorKind::RealtimeDenied | ErrorKind::DeviceChanged
    )
}

/// Finds an output device of the default host (`None` = the default output device).
fn find_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match name {
        None => host
            .default_output_device()
            .ok_or_else(|| CaptureError::NotFound("no default output device".to_owned())),
        Some(name) => host
            .output_devices()
            .map_err(|e| cpal_error("cannot enumerate output devices", &e))?
            .find(|d| d.to_string() == name)
            .ok_or_else(|| CaptureError::NotFound(format!("output device {name:?}"))),
    }
}

/// Sample formats the callback can convert to.
fn is_supported_sample_format(f: SampleFormat) -> bool {
    matches!(
        f,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
            | SampleFormat::U64
    )
}

/// A candidate stream configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Choice {
    sample_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
    buffer: SupportedBufferSize,
}

/// One supported range, as far as the choice is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeInfo {
    min_rate: u32,
    max_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
    buffer: SupportedBufferSize,
}

/// Picks the config: 48 kHz `f32` stereo, else 48 kHz `f32` with the fewest channels ≥ 2 (or
/// mono), else the device default (if its sample format is supported), else 48 kHz in any
/// supported sample format, else any supported range at its maximum rate.
fn choose_config(ranges: &[RangeInfo], default: Option<Choice>) -> Option<Choice> {
    let at_48k = |r: &&RangeInfo| r.min_rate <= PREFERRED_RATE && PREFERRED_RATE <= r.max_rate;
    let supported = |r: &&RangeInfo| is_supported_sample_format(r.sample_format);
    // Channel preference: 2, then 3, 4, ..., then 1.
    let channel_rank = |c: u16| if c >= 2 { c - 2 } else { u16::MAX };
    let pick = |r: &RangeInfo, rate: u32| Choice {
        sample_rate: rate,
        channels: r.channels,
        sample_format: r.sample_format,
        buffer: r.buffer,
    };

    if let Some(r) = ranges
        .iter()
        .filter(at_48k)
        .filter(|r| r.sample_format == SampleFormat::F32 && r.channels > 0)
        .min_by_key(|r| channel_rank(r.channels))
    {
        return Some(pick(r, PREFERRED_RATE));
    }
    if let Some(d) = default.filter(|d| is_supported_sample_format(d.sample_format)) {
        return Some(d);
    }
    if let Some(r) = ranges
        .iter()
        .filter(at_48k)
        .filter(supported)
        .filter(|r| r.channels > 0)
        .min_by_key(|r| channel_rank(r.channels))
    {
        return Some(pick(r, PREFERRED_RATE));
    }
    ranges
        .iter()
        .filter(supported)
        .filter(|r| r.channels > 0)
        .min_by_key(|r| channel_rank(r.channels))
        .map(|r| pick(r, r.max_rate))
}

/// The device buffer size for `buffer_ms` at `rate`, clamped into the supported range.
/// `BufferSize::Default` when the range is unknown or `buffer_ms` is 0.
fn buffer_size_for(buffer_ms: u32, rate: u32, supported: SupportedBufferSize) -> BufferSize {
    if buffer_ms == 0 {
        return BufferSize::Default;
    }
    let frames = u32::try_from(u64::from(rate) * u64::from(buffer_ms) / 1000).unwrap_or(u32::MAX);
    match supported {
        SupportedBufferSize::Range { min, max } if min <= max && max > 0 => {
            BufferSize::Fixed(frames.clamp(min.max(1), max))
        }
        _ => BufferSize::Default,
    }
}

impl CpalOutput {
    /// Opens the named device (`None` = default output) and chooses its stream config
    /// (48 kHz `f32` preferred, else the device default; see the module docs).
    ///
    /// # Errors
    /// [`CaptureError::NotFound`] if there is no such device (or no default device),
    /// [`CaptureError::Format`] if no config with a usable sample format exists,
    /// [`CaptureError::Backend`] for other cpal failures.
    pub fn open(device: Option<&str>, buffer_ms: u32) -> Result<Self> {
        let dev = find_device(device)?;
        let device_name = dev.to_string();
        let ranges: Vec<RangeInfo> = dev
            .supported_output_configs()
            .map_err(|e| {
                cpal_error(
                    &format!("cannot query output configs of {device_name:?}"),
                    &e,
                )
            })?
            .map(|r| RangeInfo {
                min_rate: r.min_sample_rate(),
                max_rate: r.max_sample_rate(),
                channels: r.channels(),
                sample_format: r.sample_format(),
                buffer: *r.buffer_size(),
            })
            .collect();
        let default = dev.default_output_config().ok().map(|c| Choice {
            sample_rate: c.sample_rate(),
            channels: c.channels(),
            sample_format: c.sample_format(),
            buffer: *c.buffer_size(),
        });
        let choice = choose_config(&ranges, default).ok_or_else(|| {
            CaptureError::Format(format!(
                "output device {device_name:?} offers no usable config ({ranges:?})"
            ))
        })?;
        tracing::info!(device = %device_name, ?choice, buffer_ms, "opened output device");
        Ok(Self {
            device_name,
            is_default: device.is_none(),
            format: AudioFormat::new(choice.sample_rate, choice.channels),
            sample_format: choice.sample_format,
            buffer_size: buffer_size_for(buffer_ms, choice.sample_rate, choice.buffer),
            buffer_ms,
            shared: Arc::new(Shared::default()),
            running: None,
        })
    }

    /// Name of the opened device.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Requested buffer size in ms.
    pub fn buffer_ms(&self) -> u32 {
        self.buffer_ms
    }

    /// Sample format of the device stream (the ring always carries `f32`).
    pub fn sample_format(&self) -> SampleFormat {
        self.sample_format
    }

    /// `true` once cpal reported a fatal stream error since `start` (e.g. the device was
    /// unplugged or the stream was invalidated). Same as [`AudioOutput::has_error`]. The owner
    /// should then stop this output and open a new one. Xruns, "realtime denied" and route
    /// changes the backend handled itself are not fatal.
    pub fn has_stream_error(&self) -> bool {
        self.shared.stream_error.load(Ordering::Relaxed)
    }

    /// Number of route changes since `start` after which the stream kept playing (cpal
    /// `ErrorKind::DeviceChanged`, e.g. a headphone became the default output). The stream
    /// format is unchanged; nothing needs to be reopened.
    pub fn device_changes(&self) -> u64 {
        self.shared.device_changes.load(Ordering::Relaxed)
    }

    fn config(&self, buffer_size: BufferSize) -> StreamConfig {
        StreamConfig {
            channels: self.format.channels,
            sample_rate: self.format.sample_rate,
            buffer_size,
        }
    }
}

/// Everything the stream thread needs to build the stream.
struct StreamParams {
    device: Option<String>,
    sample_format: SampleFormat,
    configs: Vec<StreamConfig>,
    shared: Arc<Shared>,
}

/// Builds and starts the stream on the stream thread, trying each config in turn (the fixed
/// buffer size first, then the default one), reports the outcome on `ready`, then keeps the
/// stream alive until `stop_rx` fires or disconnects.
fn run_stream(
    params: StreamParams,
    source: PcmSource,
    ready: &mpsc::SyncSender<Result<()>>,
    stop_rx: &mpsc::Receiver<()>,
) {
    match open_stream(params, source) {
        Ok(stream) => {
            if ready.send(Ok(())).is_ok() {
                // Park until `stop` (or the output is dropped, which closes the channel).
                let _ = stop_rx.recv();
            }
            drop(stream);
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }
}

/// Each attempt gets a fresh one-slot hand-off ring whose consumer moves into the callback;
/// the source is pushed only once a stream was built, so a rejected config does not lose it.
fn open_stream(params: StreamParams, source: PcmSource) -> Result<cpal::Stream> {
    let device = find_device(params.device.as_deref())?;
    let mut last_err = CaptureError::Backend("no stream config to try".to_owned());
    for config in &params.configs {
        let (mut handoff_tx, handoff_rx) = rtrb::RingBuffer::<PcmSource>::new(1);
        match build_stream(
            &device,
            config,
            params.sample_format,
            handoff_rx,
            &params.shared,
        ) {
            Ok(stream) => {
                if handoff_tx.push(source).is_err() {
                    // Unreachable: the one-slot ring is empty.
                    return Err(CaptureError::Backend("source hand-off failed".to_owned()));
                }
                stream
                    .play()
                    .map_err(|e| cpal_error("cannot start output stream", &e))?;
                if let Ok(frames) = stream.buffer_size() {
                    params.shared.buffer_frames.store(frames, Ordering::Relaxed);
                }
                return Ok(stream);
            }
            Err(e) => {
                tracing::debug!(?config, "output stream config rejected: {e}");
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// Builds the output stream for the device sample format.
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    handoff: rtrb::Consumer<PcmSource>,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream> {
    let state = CallbackState {
        handoff,
        source: None,
        shared: Arc::clone(shared),
    };
    match sample_format {
        SampleFormat::F32 => build_f32(device, config, state),
        SampleFormat::F64 => build_converted::<f64>(device, config, state),
        SampleFormat::I8 => build_converted::<i8>(device, config, state),
        SampleFormat::I16 => build_converted::<i16>(device, config, state),
        SampleFormat::I32 => build_converted::<i32>(device, config, state),
        SampleFormat::I64 => build_converted::<i64>(device, config, state),
        SampleFormat::U8 => build_converted::<u8>(device, config, state),
        SampleFormat::U16 => build_converted::<u16>(device, config, state),
        SampleFormat::U32 => build_converted::<u32>(device, config, state),
        SampleFormat::U64 => build_converted::<u64>(device, config, state),
        other => Err(CaptureError::Format(format!(
            "unsupported device sample format {other}"
        ))),
    }
}

/// Owned by the data callback. Real-time safe: atomics and a lock-free `rtrb` pop only.
struct CallbackState {
    handoff: rtrb::Consumer<PcmSource>,
    source: Option<PcmSource>,
    shared: Arc<Shared>,
}

impl CallbackState {
    /// The ring to play from, once it has been handed over.
    fn source(&mut self) -> Option<&mut PcmSource> {
        if self.source.is_none() {
            self.source = self.handoff.pop().ok();
        }
        self.source.as_mut()
    }

    /// Records the callback → playback delay reported by the backend.
    fn note_timing(&self, info: &OutputCallbackInfo) {
        let ts = info.timestamp();
        if let Some(delay) = ts.playback.checked_duration_since(ts.callback) {
            let us = u32::try_from(delay.as_micros()).unwrap_or(u32::MAX);
            self.shared.playback_delay_us.store(us, Ordering::Relaxed);
        }
    }
}

fn error_callback(shared: &Arc<Shared>) -> impl FnMut(cpal::Error) + Send + 'static {
    let shared = Arc::clone(shared);
    // Not a data callback, but it may run on the audio thread: atomics only, no logging.
    move |err| record_stream_error(&shared, err.kind())
}

/// Records a stream error reported by cpal in the shared atomics (real-time safe).
fn record_stream_error(shared: &Shared, kind: ErrorKind) {
    if is_fatal(kind) {
        shared.stream_error.store(true, Ordering::Relaxed);
    } else if kind == ErrorKind::Xrun {
        shared.xruns.fetch_add(1, Ordering::Relaxed);
    } else if kind == ErrorKind::DeviceChanged {
        shared.device_changes.fetch_add(1, Ordering::Relaxed);
    }
}

fn build_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    mut state: CallbackState,
) -> Result<cpal::Stream> {
    let on_error = error_callback(&state.shared);
    device
        .build_output_stream::<f32, _, _>(
            *config,
            move |data: &mut [f32], info: &OutputCallbackInfo| {
                state.note_timing(info);
                match state.source() {
                    Some(source) => {
                        source.pull(data);
                    }
                    None => data.fill(0.0),
                }
            },
            on_error,
            None,
        )
        .map_err(|e| cpal_error("cannot build f32 output stream", &e))
}

fn build_converted<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut state: CallbackState,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let on_error = error_callback(&state.shared);
    // Preallocated here, never in the callback. Chunks are whole frames.
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * usize::from(config.channels.max(1))];
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], info: &OutputCallbackInfo| {
                state.note_timing(info);
                match state.source() {
                    Some(source) => {
                        for chunk in data.chunks_mut(scratch.len()) {
                            let pcm = &mut scratch[..chunk.len()];
                            source.pull(pcm);
                            convert_into(pcm, chunk);
                        }
                    }
                    None => data.fill(T::EQUILIBRIUM),
                }
            },
            on_error,
            None,
        )
        .map_err(|e| cpal_error("cannot build output stream", &e))
}

/// Converts `f32` samples (clamped to [-1, 1]) to the device sample type.
fn convert_into<T: FromSample<f32>>(pcm: &[f32], out: &mut [T]) {
    for (o, &v) in out.iter_mut().zip(pcm) {
        *o = T::from_sample_(v.clamp(-1.0, 1.0));
    }
}

impl AudioOutput for CpalOutput {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, source: PcmSource) -> Result<()> {
        if self.running.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        check_ring_channels(&source, self.format)?;
        let mut configs = vec![self.config(self.buffer_size)];
        if self.buffer_size != BufferSize::Default {
            configs.push(self.config(BufferSize::Default));
        }
        self.shared.stream_error.store(false, Ordering::Relaxed);
        self.shared.xruns.store(0, Ordering::Relaxed);
        self.shared.device_changes.store(0, Ordering::Relaxed);
        self.shared.playback_delay_us.store(0, Ordering::Relaxed);
        self.shared.buffer_frames.store(0, Ordering::Relaxed);
        let params = StreamParams {
            device: (!self.is_default).then(|| self.device_name.clone()),
            sample_format: self.sample_format,
            configs,
            shared: Arc::clone(&self.shared),
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("hfa-cpal-out".to_owned())
            .spawn(move || run_stream(params, source, &ready_tx, &stop_rx))
            .map_err(|e| backend("cannot spawn output thread", e))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => {
                tracing::info!(device = %self.device_name, format = ?self.format, "output started");
                self.running = Some(Running { stop_tx, thread });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                // Timed out (or the thread died): leave it detached; dropping `stop_tx`
                // makes it drop the stream if it ever gets one.
                drop(stop_tx);
                Err(CaptureError::Backend(format!(
                    "output device {:?} did not start within {START_TIMEOUT:?}",
                    self.device_name
                )))
            }
        }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.stop_tx.send(());
            if running.thread.join().is_err() {
                tracing::error!("output stream thread panicked");
            }
            tracing::info!(device = %self.device_name, "output stopped");
        }
    }

    fn latency_ms(&self) -> Option<f32> {
        self.running.as_ref()?;
        let delay_us = self.shared.playback_delay_us.load(Ordering::Relaxed);
        if delay_us > 0 {
            return Some(delay_us as f32 / 1000.0);
        }
        // No timestamps from the backend: assume two device buffers are queued.
        let frames = match self.shared.buffer_frames.load(Ordering::Relaxed) {
            0 => match self.buffer_size {
                BufferSize::Fixed(f) => f,
                BufferSize::Default => return Some(self.buffer_ms as f32 * 2.0),
            },
            f => f,
        };
        Some(2.0 * frames as f32 * 1000.0 / self.format.sample_rate.max(1) as f32)
    }

    fn has_error(&self) -> bool {
        self.has_stream_error()
    }

    fn xruns(&self) -> u64 {
        self.shared.xruns.load(Ordering::Relaxed)
    }
}

impl Drop for CpalOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Names of all output devices of the default cpal host.
///
/// # Errors
/// [`CaptureError::Backend`].
pub fn list_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .output_devices()
        .map_err(|e| cpal_error("cannot enumerate output devices", &e))?;
    Ok(devices.map(|d| d.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open_output, OutputTarget};

    fn range(min: u32, max: u32, channels: u16, sample_format: SampleFormat) -> RangeInfo {
        RangeInfo {
            min_rate: min,
            max_rate: max,
            channels,
            sample_format,
            buffer: SupportedBufferSize::Range { min: 64, max: 4096 },
        }
    }

    fn default_choice(rate: u32, channels: u16, sample_format: SampleFormat) -> Choice {
        Choice {
            sample_rate: rate,
            channels,
            sample_format,
            buffer: SupportedBufferSize::Unknown,
        }
    }

    #[test]
    fn prefers_48k_f32_stereo() {
        let ranges = [
            range(44_100, 44_100, 2, SampleFormat::F32),
            range(8_000, 192_000, 6, SampleFormat::F32),
            range(8_000, 192_000, 2, SampleFormat::I16),
            range(8_000, 192_000, 2, SampleFormat::F32),
            range(8_000, 192_000, 1, SampleFormat::F32),
        ];
        let c = choose_config(&ranges, Some(default_choice(44_100, 2, SampleFormat::I16)))
            .expect("choice");
        assert_eq!(
            (c.sample_rate, c.channels, c.sample_format),
            (48_000, 2, SampleFormat::F32)
        );
    }

    #[test]
    fn prefers_multichannel_over_mono_at_48k_f32() {
        let ranges = [
            range(48_000, 48_000, 1, SampleFormat::F32),
            range(48_000, 48_000, 8, SampleFormat::F32),
            range(48_000, 48_000, 4, SampleFormat::F32),
        ];
        let c = choose_config(&ranges, None).expect("choice");
        assert_eq!(c.channels, 4);
        let c = choose_config(&ranges[..1], None).expect("choice");
        assert_eq!(c.channels, 1);
    }

    #[test]
    fn falls_back_to_the_device_default_then_anything_usable() {
        // No 48 kHz f32: the default config wins.
        let ranges = [
            range(44_100, 44_100, 2, SampleFormat::I16),
            range(8_000, 96_000, 2, SampleFormat::I32),
        ];
        let d = default_choice(44_100, 2, SampleFormat::I16);
        assert_eq!(choose_config(&ranges, Some(d)), Some(d));
        // Default unusable (DSD): 48 kHz in another format.
        let dsd = default_choice(44_100, 2, SampleFormat::DsdU8);
        let c = choose_config(&ranges, Some(dsd)).expect("choice");
        assert_eq!(
            (c.sample_rate, c.sample_format),
            (48_000, SampleFormat::I32)
        );
        // Nothing at 48 kHz and no default: highest rate of a usable range.
        let c = choose_config(&ranges[..1], None).expect("choice");
        assert_eq!(
            (c.sample_rate, c.sample_format),
            (44_100, SampleFormat::I16)
        );
        // Nothing usable at all.
        assert_eq!(
            choose_config(&[range(44_100, 44_100, 2, SampleFormat::DsdU32)], Some(dsd)),
            None
        );
    }

    #[test]
    fn buffer_size_is_clamped_to_the_supported_range() {
        let r = SupportedBufferSize::Range { min: 64, max: 4096 };
        assert_eq!(buffer_size_for(10, 48_000, r), BufferSize::Fixed(480));
        assert_eq!(buffer_size_for(1, 8_000, r), BufferSize::Fixed(64));
        assert_eq!(buffer_size_for(1000, 48_000, r), BufferSize::Fixed(4096));
        assert_eq!(buffer_size_for(0, 48_000, r), BufferSize::Default);
        assert_eq!(
            buffer_size_for(10, 48_000, SupportedBufferSize::Unknown),
            BufferSize::Default
        );
    }

    #[test]
    fn converts_to_integer_formats_with_clamping() {
        let pcm = [0.0f32, 1.0, -1.0, 2.0, -2.0, 0.5];
        let mut i16s = [0i16; 6];
        convert_into(&pcm, &mut i16s);
        assert_eq!(i16s[0], 0);
        assert_eq!(i16s[1], i16::MAX);
        assert_eq!(i16s[2], i16::MIN);
        assert_eq!(i16s[3], i16::MAX, "clamped");
        assert_eq!(i16s[4], i16::MIN, "clamped");
        assert!((i16s[5] - 16_384).abs() <= 1);
        let mut u16s = [0u16; 6];
        convert_into(&pcm, &mut u16s);
        assert_eq!(u16s[0], 32_768, "silence is mid-scale");
        assert_eq!(u16s[2], 0);
        assert_eq!(u16s[1], u16::MAX);
        let mut f64s = [0f64; 6];
        convert_into(&pcm, &mut f64s);
        assert_eq!(f64s, [0.0, 1.0, -1.0, 1.0, -1.0, 0.5]);
    }

    #[test]
    fn only_errors_that_stop_the_stream_are_fatal() {
        for kind in [
            ErrorKind::Xrun,
            ErrorKind::RealtimeDenied,
            ErrorKind::DeviceChanged,
        ] {
            assert!(!is_fatal(kind), "{kind:?} must not be fatal");
        }
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::BackendError,
        ] {
            assert!(is_fatal(kind), "{kind:?} must be fatal");
        }
    }

    /// An output that was never opened on a real device (no hardware needed).
    fn detached_output(format: AudioFormat) -> CpalOutput {
        CpalOutput {
            device_name: "hfa-test: no such device".to_owned(),
            is_default: false,
            format,
            sample_format: SampleFormat::F32,
            buffer_size: BufferSize::Default,
            buffer_ms: 20,
            shared: Arc::new(Shared::default()),
            running: None,
        }
    }

    #[test]
    fn stream_errors_are_visible_through_the_trait() {
        let out = detached_output(AudioFormat::INTERNAL);
        let dyn_out: &dyn AudioOutput = &out;
        assert!(!dyn_out.has_error());
        record_stream_error(&out.shared, ErrorKind::Xrun);
        record_stream_error(&out.shared, ErrorKind::Xrun);
        record_stream_error(&out.shared, ErrorKind::DeviceChanged);
        record_stream_error(&out.shared, ErrorKind::RealtimeDenied);
        let dyn_out: &dyn AudioOutput = &out;
        assert_eq!(dyn_out.xruns(), 2);
        assert_eq!(out.device_changes(), 1);
        assert!(!dyn_out.has_error(), "route changes and xruns keep playing");
        record_stream_error(&out.shared, ErrorKind::DeviceNotAvailable);
        let dyn_out: &dyn AudioOutput = &out;
        assert!(dyn_out.has_error());
        assert!(out.has_stream_error());
    }

    #[test]
    fn start_rejects_a_ring_with_another_channel_count() {
        let mut out = detached_output(AudioFormat::new(48_000, 6));
        let (_sink, stereo) = crate::pcm_ring_with_channels(64, 2);
        assert!(matches!(
            out.start(stereo),
            Err(CaptureError::InvalidArgument(_))
        ));
        assert_eq!(out.latency_ms(), None, "not started");
    }

    #[test]
    fn missing_devices_are_errors_not_panics() {
        // This must hold on machines with and without audio hardware (CI containers have
        // none): opening the default device either works or fails cleanly.
        let devices = list_devices();
        match open_output(&OutputTarget::Default, 20) {
            Ok(out) => {
                assert!(out.format().sample_rate > 0 && out.format().channels > 0);
                assert_eq!(out.latency_ms(), None, "not started");
            }
            Err(e) => {
                assert!(
                    matches!(
                        e,
                        CaptureError::NotFound(_)
                            | CaptureError::Backend(_)
                            | CaptureError::Format(_)
                    ),
                    "unexpected error {e:?}"
                );
                if let Ok(devices) = &devices {
                    eprintln!("no usable default output ({e}); devices: {devices:?}");
                }
            }
        }
        let name = "hfa-test: this device does not exist";
        assert!(matches!(
            open_output(&OutputTarget::Device(name.to_owned()), 20),
            Err(CaptureError::NotFound(_) | CaptureError::Backend(_))
        ));
    }

    /// Exercises the real stream thread + data callback when a device that needs no hardware
    /// exists (the ALSA `null` PCM on Linux). Skipped elsewhere.
    #[test]
    fn plays_through_a_real_stream_on_the_null_device() {
        let Ok(devices) = list_devices() else { return };
        let Some(name) = devices
            .into_iter()
            .find(|d| d.starts_with("Discard all samples"))
        else {
            eprintln!("no ALSA null device: skipped");
            return;
        };
        let mut out = match CpalOutput::open(Some(&name), 20) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("null device not usable ({e}): skipped");
                return;
            }
        };
        let format = out.format();
        assert!(format.sample_rate > 0 && format.channels > 0);
        let (mut sink, source) =
            crate::pcm_ring_with_channels(format.samples_for_ms(2000), format.channels);
        let pushed = sink.push(&vec![0.1f32; format.samples_for_ms(1000)]);
        out.start(source).expect("start");
        let (_s, source3) = crate::pcm_ring_with_channels(16, format.channels);
        assert_eq!(out.start(source3).err(), Some(CaptureError::AlreadyRunning));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let consumed = pushed - (sink.capacity() - sink.free());
        assert!(consumed > 0, "the device callback pulled nothing");
        assert!(out.latency_ms().is_some_and(|ms| ms > 0.0));
        assert!(!out.has_error());
        out.stop();
        out.stop();
        assert_eq!(out.latency_ms(), None);
        let free = sink.free();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(sink.free(), free, "stopped");
    }
}
