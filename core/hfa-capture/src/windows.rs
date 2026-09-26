//! Windows capture: WASAPI loopback of the default render endpoint, process loopback via
//! `ActivateAudioInterfaceAsync` + `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` (include or
//! exclude a process tree; `exclude_self` uses exclude mode with our own PID), and audio session
//! enumeration for `list_apps`.
//!
//! This file is owned by `feat/capture-windows`. It exposes exactly the four `pub(crate)`
//! functions of the platform-module interface (see `docs/CONTRACTS.md` §5).
//!
//! # Design
//!
//! Every capture mode is driven directly through the `windows` crate (not cpal), so that the
//! system mix and the per-process modes share one code path:
//!
//! - **System mix** ([`open_system`]`(false)`): `IMMDeviceEnumerator` → default render endpoint →
//!   `IAudioClient::Initialize(AUDCLNT_STREAMFLAGS_LOOPBACK …)`.
//! - **Process loopback** ([`open_system`]`(true)`, [`open_process`]): the virtual
//!   `VAD\Process_Loopback` device, activated asynchronously with an
//!   `AUDIOCLIENT_ACTIVATION_PARAMS` blob, following Microsoft's `ApplicationLoopback` sample.
//!
//! Each opened source owns one worker thread. The thread enters the COM multithreaded apartment,
//! activates and initialises the audio client, reports the resulting format back to
//! [`open_system`]/[`open_process`] (so [`CaptureSource::format`] is known before `start`), then
//! waits for `start` to hand it the [`PcmSink`]. No COM pointer ever crosses a thread boundary
//! except the freshly activated client handed over between two MTA threads (see
//! [`ActivatedClient`]).
//!
//! The capture loop waits on `[stop event, audio event]` with a short timeout and drains every
//! available packet with `GetBuffer`/`ReleaseBuffer`, pushing into the ring without allocating,
//! locking or logging. The timeout makes the loop robust on systems where loopback streams do not
//! signal the event. When the device is invalidated (e.g. a headset is unplugged or the audio
//! service restarts), the worker re-opens the stream and carries on. System loopback also
//! registers an `IMMNotificationClient`: a stream opened on an `IMMDevice` is *not* rerouted by
//! Windows when the user picks another default output, so `OnDefaultDeviceChanged` signals a third
//! event and the worker re-opens on the new default endpoint. Unexpected capture errors are
//! retried a few times before the worker gives up; a later `start` then prepares a fresh worker.
//!
//! We always ask WASAPI for **32-bit float, 48 kHz, stereo** with `AUTOCONVERTPCM` (the engine
//! resamples and down-mixes). If the endpoint refuses that, the system-mix mode falls back to the
//! engine mix format and converts to stereo `f32` itself; process loopback falls back to 16-bit
//! PCM (the format the Microsoft sample uses).

use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hfa_audio::AudioFormat;
use windows::core::{implement, IUnknown, Interface, Owned, Ref, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND, E_ACCESSDENIED, E_INVALIDARG,
    E_NOINTERFACE, E_NOTIMPL, E_POINTER, HANDLE, PROPERTYKEY, REGDB_E_CLASSNOTREG,
    RPC_E_CHANGED_MODE, S_OK, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, ActivateAudioInterfaceAsync, AudioSessionStateExpired, EDataFlow, ERole,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
    AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, DEVICE_STATE, DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, BLOB, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, OpenProcess,
    QueryFullProcessImageNameW, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VT_BLOB;

use crate::ring::PcmSink;
use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource};

/// Sample rate we ask WASAPI to convert to (the internal format, so the sender does no work).
const TARGET_RATE: u32 = 48_000;
/// Channels we always deliver.
const OUT_CHANNELS: usize = 2;
/// Requested WASAPI buffer duration in 100 ns units (100 ms of headroom; capture latency is
/// driven by the engine period, not by this size).
const BUFFER_DURATION_HNS: i64 = 1_000_000;
/// Longest wait for one capture event before polling anyway (loopback streams on some Windows
/// builds never signal the event).
const WAIT_TIMEOUT_MS: u32 = 20;
/// How long `ActivateAudioInterfaceAsync` may take before we give up.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
/// Slice in which the activation wait checks the stop event, so `stop` never blocks for long.
const ACTIVATION_POLL: Duration = Duration::from_millis(50);
/// Consecutive unexpected capture failures (without a delivered packet in between) after which
/// the worker gives up.
const MAX_FAILED_RESTARTS: u32 = 5;
/// Re-open attempts that land on a different output format before the worker gives up (a device
/// switch can briefly refuse the preferred float/48 kHz/stereo format).
const MAX_FORMAT_MISMATCHES: u32 = 10;
/// Pause between attempts to re-open an invalidated stream.
const REOPEN_BACKOFF_MS: u32 = 500;
/// Minimum scratch size (frames) used for format conversion and silence.
const MIN_SCRATCH_FRAMES: usize = 4_800;
/// OS requirement shown in errors and capability notes.
const PROCESS_LOOPBACK_REQUIREMENT: &str =
    "process loopback needs Windows 10 build 20348 or newer (it usually works from build 19041, version 2004)";

// ---------------------------------------------------------------------------------------------
// Platform-module interface
// ---------------------------------------------------------------------------------------------

/// Capture capabilities on Windows.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        system_mix: true,
        per_app: true,
        mutes_local_output: false,
        notes: format!(
            "System audio uses WASAPI loopback of the default output device. Per-app capture and \
             \"system audio excluding this app\" use WASAPI process loopback; {PROCESS_LOOPBACK_REQUIREMENT}. \
             Local speakers keep playing while captured. Loopback delivers no data while nothing \
             is playing."
        ),
    }
}

/// Opens system-mix capture; with `exclude_self` the current process tree is excluded (process
/// loopback in exclude mode), so a device that is both hub and sender never captures itself.
pub(crate) fn open_system(exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError> {
    let mode = if exclude_self {
        Mode::Process {
            pid: std::process::id(),
            include_tree: false,
        }
    } else {
        Mode::SystemLoopback
    };
    Ok(Box::new(WasapiCapture::open(mode)?))
}

/// Opens capture of one process tree (process loopback in include mode).
pub(crate) fn open_process(pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError> {
    if pid == 0 {
        return Err(CaptureError::InvalidArgument(
            "pid 0 is the System Idle Process and cannot be captured".to_owned(),
        ));
    }
    ensure_process_exists(pid)?;
    Ok(Box::new(WasapiCapture::open(Mode::Process {
        pid,
        include_tree: true,
    })?))
}

/// Lists processes that own an audio session on an active render endpoint (deduplicated by
/// pid, sorted by name). System sounds, pid 0 and this process are skipped.
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError> {
    // Run on a fresh thread so we never change (or collide with) the COM apartment of the
    // caller, which may be a UI thread already initialised as STA.
    let worker = std::thread::Builder::new()
        .name("hfa-wasapi-sessions".to_owned())
        .spawn(|| {
            let _com = ComApartment::enter_mta()?;
            enumerate_sessions()
        })
        .map_err(|e| CaptureError::Backend(format!("cannot spawn session thread: {e}")))?;
    worker
        .join()
        .map_err(|_| CaptureError::Backend("audio session enumeration panicked".to_owned()))?
}

// ---------------------------------------------------------------------------------------------
// Capture source
// ---------------------------------------------------------------------------------------------

/// What a [`WasapiCapture`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Loopback of the default render endpoint (everything the device plays).
    SystemLoopback,
    /// Process loopback: include or exclude the process tree rooted at `pid`.
    Process { pid: u32, include_tree: bool },
}

impl Mode {
    fn describe(self) -> String {
        match self {
            Mode::SystemLoopback => "System audio (WASAPI loopback)".to_owned(),
            Mode::Process {
                include_tree: false,
                ..
            } => "System audio excluding this app (WASAPI process loopback)".to_owned(),
            Mode::Process {
                pid,
                include_tree: true,
            } => format!("Process {pid} (WASAPI process loopback)"),
        }
    }
}

/// A WASAPI capture source (system loopback or process loopback).
struct WasapiCapture {
    mode: Mode,
    format: AudioFormat,
    worker: Option<Worker>,
}

/// Handles to one capture worker thread.
struct Worker {
    thread: JoinHandle<()>,
    stop: Arc<Event>,
    /// Hands the sink to the prepared thread; `None` once started.
    start_tx: Option<SyncSender<PcmSink>>,
    /// Answer of the thread to the start request.
    ack_rx: Receiver<Result<(), CaptureError>>,
}

impl WasapiCapture {
    /// Prepares the worker (activation + `Initialize`) so errors surface at open time.
    fn open(mode: Mode) -> Result<Self, CaptureError> {
        let (worker, format) = Worker::spawn(mode)?;
        tracing::info!(
            mode = %mode.describe(),
            rate = format.sample_rate,
            channels = format.channels,
            "WASAPI capture opened"
        );
        Ok(Self {
            mode,
            format,
            worker: Some(worker),
        })
    }
}

impl Worker {
    /// Spawns and prepares a worker; returns once the stream is initialised.
    fn spawn(mode: Mode) -> Result<(Self, AudioFormat), CaptureError> {
        let stop = Arc::new(Event::new(true)?);
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<AudioFormat, CaptureError>>(1);
        let (start_tx, start_rx) = mpsc::sync_channel::<PcmSink>(1);
        let (ack_tx, ack_rx) = mpsc::sync_channel::<Result<(), CaptureError>>(1);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("hfa-wasapi-capture".to_owned())
            .spawn(move || worker_main(mode, &thread_stop, &init_tx, &start_rx, &ack_tx))
            .map_err(|e| CaptureError::Backend(format!("cannot spawn capture thread: {e}")))?;
        let format = match init_rx.recv() {
            Ok(Ok(format)) => format,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(CaptureError::Backend(
                    "capture thread exited during initialisation".to_owned(),
                ));
            }
        };
        let worker = Worker {
            thread,
            stop,
            start_tx: Some(start_tx),
            ack_rx,
        };
        Ok((worker, format))
    }

    /// Signals the thread to stop and joins it.
    fn shutdown(mut self) {
        self.stop.set();
        // Unblocks a prepared-but-not-started thread waiting for its sink.
        self.start_tx = None;
        if self.thread.join().is_err() {
            tracing::error!("WASAPI capture thread panicked");
        }
    }
}

impl CaptureSource for WasapiCapture {
    fn describe(&self) -> String {
        self.mode.describe()
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, sink: PcmSink) -> crate::Result<()> {
        if let Some(worker) = self.worker.as_ref() {
            if worker.start_tx.is_none() {
                if !worker.thread.is_finished() {
                    return Err(CaptureError::AlreadyRunning);
                }
                // The capture thread gave up (see `run_worker`): reap it and prepare a fresh one
                // below instead of reporting a dead source as running.
                tracing::warn!(
                    mode = %self.mode.describe(),
                    "WASAPI capture thread had exited; restarting it"
                );
                if let Some(dead) = self.worker.take() {
                    dead.shutdown();
                }
            }
        }
        if self.worker.is_none() {
            // Restart after `stop`: prepare a fresh stream. The sender configured itself from
            // `format()`, so the format must not change underneath it.
            let (worker, format) = Worker::spawn(self.mode)?;
            if format != self.format {
                worker.shutdown();
                return Err(CaptureError::Format(format!(
                    "capture format changed from {:?} to {format:?} after restart",
                    self.format
                )));
            }
            self.worker = Some(worker);
        }
        let worker = self
            .worker
            .as_mut()
            .ok_or_else(|| CaptureError::Backend("capture worker missing".to_owned()))?;
        let sent = worker
            .start_tx
            .take()
            .map(|tx| tx.send(sink).is_ok())
            .unwrap_or(false);
        let ack = if sent {
            worker.ack_rx.recv().unwrap_or_else(|_| {
                Err(CaptureError::Backend(
                    "capture thread exited before starting".to_owned(),
                ))
            })
        } else {
            Err(CaptureError::Backend(
                "capture thread exited before starting".to_owned(),
            ))
        };
        if ack.is_err() {
            if let Some(worker) = self.worker.take() {
                worker.shutdown();
            }
        }
        ack
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
            tracing::info!(mode = %self.mode.describe(), "WASAPI capture stopped");
        }
    }

    fn error(&self) -> Option<String> {
        // A started worker only exits on its own when it gave up (see `run_worker`: repeated
        // failures, or a re-opened endpoint with another format).
        self.worker
            .as_ref()
            .filter(|w| w.start_tx.is_none() && w.thread.is_finished())
            .map(|_| {
                "the Windows audio capture stopped (the device kept failing or changed its \
                 format); start the sender again"
                    .to_owned()
            })
    }
}

impl Drop for WasapiCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------------------------

/// Why the capture loop returned.
#[derive(Debug)]
enum LoopExit {
    /// The stop event was signalled.
    Stopped,
    /// The endpoint went away (`AUDCLNT_E_DEVICE_INVALIDATED` and friends): re-open.
    Invalidated,
    /// Another endpoint became the default output (system loopback only): re-open on it.
    DefaultChanged,
    /// Any other failure: re-open after a pause, a bounded number of times.
    Failed(windows::core::Error),
}

/// Result of [`reopen`].
enum Reopened {
    /// A started stream with the expected output format.
    Stream(Stream),
    /// The stop event fired while re-opening.
    Stopped,
    /// The endpoint kept delivering another output format.
    FormatChanged(AudioFormat),
}

/// Body of the capture thread. Every COM object lives inside [`run_worker`], so it is released
/// before `_com` uninitialises COM.
fn worker_main(
    mode: Mode,
    stop: &Event,
    init_tx: &SyncSender<Result<AudioFormat, CaptureError>>,
    start_rx: &Receiver<PcmSink>,
    ack_tx: &SyncSender<Result<(), CaptureError>>,
) {
    let _com = match ComApartment::enter_mta() {
        Ok(com) => com,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    run_worker(mode, stop, init_tx, start_rx, ack_tx);
}

fn run_worker(
    mode: Mode,
    stop: &Event,
    init_tx: &SyncSender<Result<AudioFormat, CaptureError>>,
    start_rx: &Receiver<PcmSink>,
    ack_tx: &SyncSender<Result<(), CaptureError>>,
) {
    // Register before opening, so a default-device change between open and the first wait is
    // not missed. Only system loopback is bound to a concrete endpoint.
    let watch = match mode {
        Mode::SystemLoopback => match DefaultDeviceWatch::register() {
            Ok(watch) => Some(watch),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "cannot watch default output changes; capture stays on the current device"
                );
                None
            }
        },
        Mode::Process { .. } => None,
    };
    let reroute = watch.as_ref().map(DefaultDeviceWatch::event);
    let mut stream = match Stream::open(mode, stop) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    let format = stream.format.output();
    if init_tx.send(Ok(format)).is_err() {
        return;
    }
    // Wait for `start` (or for the owner to drop the sender on stop/drop).
    let Ok(mut sink) = start_rx.recv() else {
        return;
    };
    if let Err(first) = stream.start() {
        // The endpoint may have changed between open and start: retry once with a fresh stream.
        let retried = Stream::open(mode, stop).and_then(|fresh| {
            if fresh.format.output() == format {
                fresh.start().map(|()| fresh)
            } else {
                Err(CaptureError::Format(
                    "capture format changed before start".to_owned(),
                ))
            }
        });
        match retried {
            Ok(fresh) => stream = fresh,
            Err(_) => {
                let _ = ack_tx.send(Err(first));
                return;
            }
        }
    }
    let _ = ack_tx.send(Ok(()));

    let _mmcss = MmcssRegistration::pro_audio();
    let mut scratch = vec![0.0_f32; scratch_len(stream.buffer_frames)];
    let mut push = |samples: &[f32]| {
        sink.push(samples);
    };
    let mut failures = 0_u32;
    loop {
        let mut delivered = false;
        let exit = capture_loop(
            &stream,
            &mut push,
            stop,
            reroute,
            &mut scratch,
            &mut delivered,
        );
        if delivered {
            failures = 0;
        }
        match exit {
            LoopExit::Stopped => return,
            LoopExit::Invalidated => {
                tracing::warn!(mode = %mode.describe(), "audio endpoint invalidated; re-opening");
            }
            LoopExit::DefaultChanged => {
                tracing::info!(
                    mode = %mode.describe(),
                    "default output device changed; re-opening loopback on it"
                );
            }
            LoopExit::Failed(e) => {
                failures += 1;
                if failures > MAX_FAILED_RESTARTS {
                    tracing::error!(
                        error = %e,
                        mode = %mode.describe(),
                        "WASAPI capture keeps failing; giving up (start again to retry)"
                    );
                    return;
                }
                tracing::warn!(
                    error = %e,
                    mode = %mode.describe(),
                    attempt = failures,
                    "WASAPI capture failed; re-opening"
                );
                if stop.wait(REOPEN_BACKOFF_MS) {
                    return;
                }
            }
        }
        drop(stream);
        match reopen(mode, format, stop) {
            Reopened::Stream(fresh) => {
                stream = fresh;
                let needed = scratch_len(stream.buffer_frames);
                if scratch.len() < needed {
                    scratch.resize(needed, 0.0);
                }
                tracing::info!(mode = %mode.describe(), "WASAPI capture resumed");
            }
            Reopened::Stopped => return,
            Reopened::FormatChanged(got) => {
                tracing::error!(
                    mode = %mode.describe(),
                    expected = ?format,
                    got = ?got,
                    "re-opened endpoint delivers a different format; capture stopped \
                     (stop and re-open the source to adopt the new format)"
                );
                return;
            }
        }
    }
}

/// Re-opens and starts a stream with the same output format, retrying every
/// [`REOPEN_BACKOFF_MS`] until it works, the stop event fires, or the endpoint has delivered a
/// different format [`MAX_FORMAT_MISMATCHES`] times in a row.
fn reopen(mode: Mode, format: AudioFormat, stop: &Event) -> Reopened {
    let mut mismatches = 0_u32;
    loop {
        match Stream::open(mode, stop).and_then(|s| s.start().map(|()| s)) {
            Ok(stream) if stream.format.output() == format => return Reopened::Stream(stream),
            Ok(stream) => {
                mismatches += 1;
                let got = stream.format.output();
                if mismatches >= MAX_FORMAT_MISMATCHES {
                    return Reopened::FormatChanged(got);
                }
                tracing::debug!(expected = ?format, got = ?got, "re-opened with another format; retrying");
            }
            Err(e) => tracing::debug!(error = %e, "re-opening capture stream failed; retrying"),
        }
        if stop.wait(REOPEN_BACKOFF_MS) {
            return Reopened::Stopped;
        }
    }
}

/// Scratch buffer length (samples) for a stream buffer of `buffer_frames`.
fn scratch_len(buffer_frames: u32) -> usize {
    (buffer_frames as usize).max(MIN_SCRATCH_FRAMES) * OUT_CHANNELS
}

/// The real-time loop: wait, drain, push. No allocation, locking or logging in here.
///
/// Waits on `[stop, audio]`, plus `reroute` (signalled when the default output changes) when
/// given. Sets `delivered` once any packet has been pushed.
fn capture_loop<F: FnMut(&[f32])>(
    stream: &Stream,
    push: &mut F,
    stop: &Event,
    reroute: Option<&Event>,
    scratch: &mut [f32],
    delivered: &mut bool,
) -> LoopExit {
    let all = [
        stop.raw(),
        stream.event.raw(),
        reroute.map_or_else(HANDLE::default, Event::raw),
    ];
    let handles = if reroute.is_some() {
        &all[..]
    } else {
        &all[..2]
    };
    let reroute_woke = WAIT_EVENT(WAIT_OBJECT_0.0 + 2);
    loop {
        // SAFETY: every handle in `handles` is a valid event handle owned by `stop`, `stream`
        // or `reroute`, all of which outlive this call.
        let woke = unsafe { WaitForMultipleObjects(handles, false, WAIT_TIMEOUT_MS) };
        if woke == WAIT_OBJECT_0 {
            return LoopExit::Stopped;
        }
        if woke == WAIT_FAILED {
            return LoopExit::Failed(windows::core::Error::from_thread());
        }
        if reroute.is_some() && woke == reroute_woke {
            return LoopExit::DefaultChanged;
        }
        // Audio event or timeout: drain whatever is there.
        match drain_packets(stream, push, scratch) {
            Ok(drained) => *delivered |= drained,
            Err(e) => {
                return if is_device_lost(e.code()) {
                    LoopExit::Invalidated
                } else {
                    LoopExit::Failed(e)
                };
            }
        }
    }
}

/// Reads every available packet and pushes it as stereo `f32`. Returns whether any packet was
/// read.
fn drain_packets<F: FnMut(&[f32])>(
    stream: &Stream,
    push: &mut F,
    scratch: &mut [f32],
) -> windows::core::Result<bool> {
    let mut any = false;
    loop {
        // SAFETY: `stream.capture` is a valid, initialised capture client of a started stream.
        let pending = unsafe { stream.capture.GetNextPacketSize()? };
        if pending == 0 {
            return Ok(any);
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0_u32;
        let mut flags = 0_u32;
        // SAFETY: all out-pointers point at live locals; the optional position outputs are
        // not requested.
        unsafe {
            stream
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?
        };
        any = true;
        let frame_count = frames as usize;
        let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
        if frame_count > 0 {
            if silent || data.is_null() {
                push_silence(push, frame_count * OUT_CHANNELS, scratch);
            } else {
                let len = frame_count * stream.format.block_align;
                // SAFETY: after a successful GetBuffer, `data` points at `frames` frames of
                // `block_align` bytes each, valid until ReleaseBuffer below.
                let bytes = unsafe { std::slice::from_raw_parts(data, len) };
                push_converted(push, &stream.format, bytes, scratch);
            }
        }
        // SAFETY: releases exactly the frames obtained by the GetBuffer call above.
        unsafe { stream.capture.ReleaseBuffer(frames)? };
    }
}

/// Pushes `samples` zeros (in scratch-sized chunks, without allocating).
fn push_silence<F: FnMut(&[f32])>(push: &mut F, samples: usize, scratch: &mut [f32]) {
    let mut left = samples;
    while left > 0 {
        let n = left.min(scratch.len());
        let Some(chunk) = scratch.get_mut(..n).filter(|chunk| !chunk.is_empty()) else {
            return;
        };
        chunk.fill(0.0);
        push(chunk);
        left -= n;
    }
}

/// Converts `bytes` (native frames) to stereo `f32` and pushes them (in scratch-sized chunks
/// unless WASAPI already delivers aligned stereo `f32`).
fn push_converted<F: FnMut(&[f32])>(
    push: &mut F,
    format: &NativeFormat,
    bytes: &[u8],
    scratch: &mut [f32],
) {
    if format.kind == SampleKind::F32
        && format.channels == OUT_CHANNELS
        && bytes.as_ptr().align_offset(std::mem::align_of::<f32>()) == 0
    {
        // Fast path: WASAPI already delivers what we need.
        // SAFETY: the pointer is 4-byte aligned (checked above), the region is `bytes.len()`
        // bytes long and every bit pattern is a valid f32.
        let samples =
            unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), bytes.len() / 4) };
        push(samples);
        return;
    }
    let chunk_frames = scratch.len() / OUT_CHANNELS;
    if chunk_frames == 0 || format.block_align == 0 {
        return;
    }
    for chunk in bytes.chunks(chunk_frames * format.block_align) {
        let frames = convert_to_stereo_f32(format, chunk, scratch);
        if let Some(out) = scratch
            .get(..frames * OUT_CHANNELS)
            .filter(|out| !out.is_empty())
        {
            push(out);
        }
    }
}

/// An initialised (not necessarily started) WASAPI capture stream.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    /// Auto-reset event signalled by WASAPI when a buffer is ready.
    event: Event,
    format: NativeFormat,
    buffer_frames: u32,
}

impl Stream {
    /// Activates and initialises a stream for `mode`. A pending process-loopback activation is
    /// abandoned as soon as `stop` is signalled.
    fn open(mode: Mode, stop: &Event) -> Result<Self, CaptureError> {
        match mode {
            Mode::SystemLoopback => Self::open_system_loopback(),
            Mode::Process { pid, include_tree } => {
                let loopback_mode = if include_tree {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                };
                Self::open_process_loopback(pid, loopback_mode, stop)
            }
        }
    }

    fn open_system_loopback() -> Result<Self, CaptureError> {
        let device = default_render_device()?;
        let preferred = float_format(TARGET_RATE, OUT_CHANNELS as u16);
        let convert_flags =
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        let client = activate_client(&device)?;
        // SAFETY: `preferred` is a complete WAVEFORMATEX (cbSize 0) living on this stack frame.
        match unsafe { Self::initialize(client, &preferred, convert_flags) } {
            Ok(stream) => return Ok(stream),
            Err(e) => tracing::debug!(
                error = %e,
                "endpoint refused float32/48 kHz/stereo loopback; using its mix format"
            ),
        }
        // A client whose Initialize failed must not be reused: activate a fresh one.
        let client = activate_client(&device)?;
        let mix = MixFormat::of(&client)?;
        // SAFETY: `mix` owns the complete (possibly extensible) format until it drops.
        unsafe { Self::initialize(client, mix.as_ptr(), 0) }
    }

    fn open_process_loopback(
        pid: u32,
        mode: PROCESS_LOOPBACK_MODE,
        stop: &Event,
    ) -> Result<Self, CaptureError> {
        let float = float_format(TARGET_RATE, OUT_CHANNELS as u16);
        let pcm16 = pcm16_format(TARGET_RATE, OUT_CHANNELS as u16);
        let mut last = None;
        for format in [&float, &pcm16] {
            let client = activate_process_loopback(pid, mode, stop)?;
            // GetMixFormat is not supported on the process-loopback device: always pass an
            // explicit format and let AUTOCONVERTPCM convert.
            // SAFETY: `format` is a complete WAVEFORMATEX (cbSize 0) on this stack frame.
            match unsafe { Self::initialize(client, format, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM) } {
                Ok(stream) => return Ok(stream),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            CaptureError::Backend("process loopback initialisation failed".to_owned())
        }))
    }

    /// `IAudioClient::Initialize` in shared loopback + event mode, then fetch the capture client.
    ///
    /// # Safety
    /// `format` must point at a valid `WAVEFORMATEX` followed by `cbSize` extension bytes in the
    /// same allocation, valid for the duration of the call.
    unsafe fn initialize(
        client: IAudioClient,
        format: *const WAVEFORMATEX,
        extra_flags: u32,
    ) -> Result<Self, CaptureError> {
        // SAFETY: guaranteed by the caller.
        let native = unsafe { NativeFormat::from_waveformat(format) }?;
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | extra_flags;
        // SAFETY: `format` points at a valid WAVEFORMATEX (plus its extension when cbSize says
        // so) that outlives the call (caller contract); periodicity must be 0 in shared mode.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                BUFFER_DURATION_HNS,
                0,
                format,
                None,
            )
        }
        .map_err(|e| map_error("IAudioClient::Initialize", &e))?;
        let event = Event::new(false)?;
        // SAFETY: the client is initialised with EVENTCALLBACK; the event handle stays valid
        // for the lifetime of the stream (it is dropped after the client).
        unsafe { client.SetEventHandle(event.raw()) }
            .map_err(|e| map_error("IAudioClient::SetEventHandle", &e))?;
        // SAFETY: the client is initialised.
        let buffer_frames = unsafe { client.GetBufferSize() }
            .map_err(|e| map_error("IAudioClient::GetBufferSize", &e))?;
        // SAFETY: the client is initialised; IAudioCaptureClient is a service of capture and
        // loopback streams.
        let capture: IAudioCaptureClient = unsafe { client.GetService() }
            .map_err(|e| map_error("IAudioClient::GetService(IAudioCaptureClient)", &e))?;
        Ok(Self {
            client,
            capture,
            event,
            format: native,
            buffer_frames,
        })
    }

    fn start(&self) -> Result<(), CaptureError> {
        // SAFETY: the client is initialised and has an event handle.
        unsafe { self.client.Start() }.map_err(|e| map_error("IAudioClient::Start", &e))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stopping an initialised (possibly never started or already invalidated)
        // client is allowed; the error, if any, is irrelevant during teardown.
        let _ = unsafe { self.client.Stop() };
    }
}

/// The default render (output) endpoint.
fn default_render_device() -> Result<IMMDevice, CaptureError> {
    let enumerator = device_enumerator()?;
    // SAFETY: the enumerator is a valid COM object on this MTA thread.
    unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(|e| {
        if e.code() == ERROR_NOT_FOUND.to_hresult() {
            CaptureError::NotFound("no default audio output device".to_owned())
        } else {
            map_error("IMMDeviceEnumerator::GetDefaultAudioEndpoint", &e)
        }
    })
}

fn device_enumerator() -> Result<IMMDeviceEnumerator, CaptureError> {
    // SAFETY: COM is initialised on this thread (MTA); MMDeviceEnumerator is an in-proc class.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| map_error("CoCreateInstance(MMDeviceEnumerator)", &e))
}

fn activate_client(device: &IMMDevice) -> Result<IAudioClient, CaptureError> {
    // SAFETY: `device` is a valid endpoint; no activation parameters are needed.
    unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }
        .map_err(|e| map_error("IMMDevice::Activate(IAudioClient)", &e))
}

/// The engine mix format of an endpoint (CoTaskMem allocation freed on drop).
struct MixFormat(*mut WAVEFORMATEX);

impl MixFormat {
    fn of(client: &IAudioClient) -> Result<Self, CaptureError> {
        // SAFETY: the client is activated; the returned pointer is owned by us (CoTaskMem).
        let ptr = unsafe { client.GetMixFormat() }
            .map_err(|e| map_error("IAudioClient::GetMixFormat", &e))?;
        if ptr.is_null() {
            return Err(CaptureError::Backend(
                "GetMixFormat returned null".to_owned(),
            ));
        }
        Ok(Self(ptr))
    }

    /// The owned format (header plus `cbSize` extension bytes), valid until `self` drops.
    fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.0.cast_const()
    }
}

impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: the pointer came from GetMixFormat (CoTaskMemAlloc) and is freed once.
        unsafe { CoTaskMemFree(Some(self.0.cast_const().cast())) };
    }
}

// ---------------------------------------------------------------------------------------------
// Default output change notifications
// ---------------------------------------------------------------------------------------------

/// `IMMNotificationClient` that signals `changed` when the default console render endpoint
/// changes. A loopback stream activated on a concrete `IMMDevice` is not rerouted by Windows (only
/// streams activated on the default-device interface are), and the old endpoint usually stays
/// valid, so without this the capture would silently stay on the previous device.
#[implement(IMMNotificationClient)]
struct DefaultDeviceListener {
    changed: Arc<Event>,
}

impl IMMNotificationClient_Impl for DefaultDeviceListener_Impl {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &PCWSTR,
        _new_state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _default_device_id: &PCWSTR,
    ) -> windows::core::Result<()> {
        // We open `GetDefaultAudioEndpoint(eRender, eConsole)`; the callback also fires once
        // per other role and for capture devices. SetEvent does not block.
        if is_our_default_change(flow, role) {
            self.changed.set();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device_id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// `true` for the default-device change that affects system loopback.
fn is_our_default_change(flow: EDataFlow, role: ERole) -> bool {
    flow == eRender && role == eConsole
}

/// Registration of a [`DefaultDeviceListener`], unregistered on drop (before the enumerator is
/// released). Lives on the capture thread (MTA).
struct DefaultDeviceWatch {
    enumerator: IMMDeviceEnumerator,
    listener: IMMNotificationClient,
    /// Auto-reset event signalled by the listener.
    changed: Arc<Event>,
}

impl DefaultDeviceWatch {
    fn register() -> Result<Self, CaptureError> {
        let changed = Arc::new(Event::new(false)?);
        let enumerator = device_enumerator()?;
        let listener: IMMNotificationClient = DefaultDeviceListener {
            changed: Arc::clone(&changed),
        }
        .into();
        // SAFETY: valid enumerator and listener on this MTA thread; the registration is undone
        // in Drop while both are still alive.
        unsafe { enumerator.RegisterEndpointNotificationCallback(&listener) }.map_err(|e| {
            map_error(
                "IMMDeviceEnumerator::RegisterEndpointNotificationCallback",
                &e,
            )
        })?;
        Ok(Self {
            enumerator,
            listener,
            changed,
        })
    }

    fn event(&self) -> &Event {
        &self.changed
    }
}

impl Drop for DefaultDeviceWatch {
    fn drop(&mut self) {
        // SAFETY: unregisters the listener registered in `register` on the same enumerator; not
        // called from inside a notification callback.
        if let Err(e) = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.listener)
        } {
            tracing::debug!(error = %e, "UnregisterEndpointNotificationCallback failed");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Process loopback activation
// ---------------------------------------------------------------------------------------------

/// A freshly activated `IAudioClient` handed from the activation callback (an MTA worker
/// thread) to the capture thread (also MTA).
struct ActivatedClient(IAudioClient);

// SAFETY: the callback runs on an MTA worker thread and the receiving capture thread has entered
// the MTA too; interface pointers may be used from any thread of the same apartment.
unsafe impl Send for ActivatedClient {}

/// Completion handler for `ActivateAudioInterfaceAsync`. `#[implement]` makes it agile
/// (`IAgileObject` + free-threaded marshaller), which the API requires.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    done: SyncSender<Result<ActivatedClient, HRESULT>>,
    /// The activation parameters the pending operation points at. The OS holds a reference to
    /// this handler until `ActivateCompleted` has run, so owning them here keeps them alive for
    /// the whole operation even if the waiter gave up (timeout or stop).
    _params: ActivationParams,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let outcome = match operation.as_ref() {
            Some(op) => activation_outcome(op),
            None => Err(E_POINTER),
        };
        // Capacity 1 and exactly one completion: never blocks. If the waiter already gave up
        // (timeout), the result is simply dropped (releasing the client).
        let _ = self.done.try_send(outcome);
        Ok(())
    }
}

/// Reads the activation result inside the completion callback (as the docs require).
fn activation_outcome(
    operation: &IActivateAudioInterfaceAsyncOperation,
) -> Result<ActivatedClient, HRESULT> {
    let mut activate_hr = S_OK;
    let mut interface: Option<IUnknown> = None;
    // SAFETY: both out-pointers point at live locals.
    unsafe { operation.GetActivateResult(&mut activate_hr, &mut interface) }
        .map_err(|e| e.code())?;
    activate_hr.ok().map_err(|e| e.code())?;
    let unknown = interface.ok_or(E_NOINTERFACE)?;
    unknown
        .cast::<IAudioClient>()
        .map(ActivatedClient)
        .map_err(|e| e.code())
}

/// The process-loopback `AUDIOCLIENT_ACTIVATION_PARAMS` and the `VT_BLOB` `PROPVARIANT` that
/// points at them.
///
/// The `PROPVARIANT` is only a **view** of `params` and must never be cleared: the windows
/// crate implements `Drop` for `PROPVARIANT` with `PropVariantClear`, which for `VT_BLOB` frees
/// `pBlobData` with `CoTaskMemFree` — here a pointer into this Rust allocation, i.e. heap
/// corruption. Hence `ManuallyDrop` (nothing in the blob needs freeing).
struct ActivationParamsData {
    params: AUDIOCLIENT_ACTIVATION_PARAMS,
    prop: ManuallyDrop<PROPVARIANT>,
}

/// Owner of a heap-allocated [`ActivationParamsData`] at a fixed address (the `PROPVARIANT`
/// points into the same allocation, so it must never move). Freed on drop.
struct ActivationParams(NonNull<ActivationParamsData>);

impl ActivationParams {
    fn new(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Self {
        let data = Box::new(ActivationParamsData {
            params: AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                    ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                        TargetProcessId: pid,
                        ProcessLoopbackMode: mode,
                    },
                },
            },
            prop: ManuallyDrop::new(blob_propvariant(std::ptr::null_mut(), 0)),
        });
        let ptr = NonNull::from(Box::leak(data));
        let raw = ptr.as_ptr();
        // SAFETY: `raw` comes from a leaked Box, so it is valid, aligned and uniquely owned by
        // us; we only take field addresses and write the PROPVARIANT in place (`write` drops
        // nothing; the placeholder is a `ManuallyDrop` view owning nothing anyway).
        unsafe {
            let blob_data = std::ptr::addr_of_mut!((*raw).params).cast::<u8>();
            std::ptr::addr_of_mut!((*raw).prop).write(ManuallyDrop::new(blob_propvariant(
                blob_data,
                std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            )));
        }
        Self(ptr)
    }

    /// The `VT_BLOB` PROPVARIANT, valid while `self` lives.
    fn prop(&self) -> *const PROPVARIANT {
        // SAFETY: the allocation is live until `self` drops; only a field address is taken
        // (`ManuallyDrop<T>` is `repr(transparent)`, so the cast is exact).
        unsafe { std::ptr::addr_of!((*self.0.as_ptr()).prop).cast::<PROPVARIANT>() }
    }
}

impl Drop for ActivationParams {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::leak` in `new` and is reclaimed exactly once.
        // The PROPVARIANT is a `ManuallyDrop` view whose blob points into this allocation, so
        // it is deliberately not dropped (the windows crate's `Drop` would `PropVariantClear`
        // it and hand `pBlobData` to `CoTaskMemFree`).
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// A `VT_BLOB` PROPVARIANT pointing at `len` bytes at `data` (not owned by the PROPVARIANT).
///
/// The result must never be dropped or `PropVariantClear`ed (that would free `data` with
/// `CoTaskMemFree`): wrap it in `ManuallyDrop`, as [`ActivationParams`] does.
fn blob_propvariant(data: *mut u8, len: u32) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: len,
                        pBlobData: data,
                    },
                },
            }),
        },
    }
}

/// The error returned when `stop` interrupts a pending activation.
fn activation_cancelled() -> CaptureError {
    CaptureError::Backend("process loopback activation cancelled by stop".to_owned())
}

/// Activates an `IAudioClient` on the process-loopback virtual device and waits for it (up to
/// [`ACTIVATION_TIMEOUT`], returning early when `stop` is signalled).
fn activate_process_loopback(
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
    stop: &Event,
) -> Result<IAudioClient, CaptureError> {
    if stop.wait(0) {
        return Err(activation_cancelled());
    }
    let params = ActivationParams::new(pid, mode);
    let prop = params.prop();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    // The handler owns `params`: the heap allocation `prop` points at does not move and lives
    // as long as the handler, i.e. until the OS has delivered the completion.
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        done: done_tx,
        _params: params,
    }
    .into();
    // SAFETY: the device path is a static wide string, the IID matches IAudioClient, `prop`
    // points into the allocation owned by `handler` (kept alive by our reference until after
    // the wait, and by the API's reference until the callback has run, whatever happens to
    // the wait), and `handler` is an agile COM object.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(prop),
            &handler,
        )
    }
    .map_err(|e| process_loopback_error(e.code()))?;
    let deadline = Instant::now() + ACTIVATION_TIMEOUT;
    let outcome = loop {
        match done_rx.recv_timeout(ACTIVATION_POLL) {
            Ok(result) => break result.map_err(process_loopback_error),
            Err(RecvTimeoutError::Disconnected) => {
                break Err(CaptureError::Backend(
                    "process loopback activation handler was released without completing"
                        .to_owned(),
                ))
            }
            Err(RecvTimeoutError::Timeout) => {
                if stop.wait(0) {
                    break Err(activation_cancelled());
                }
                if Instant::now() >= deadline {
                    break Err(CaptureError::Backend(
                        "process loopback activation timed out".to_owned(),
                    ));
                }
            }
        }
    };
    drop(operation);
    drop(handler);
    outcome.map(|ActivatedClient(client)| client)
}

/// Maps a process-loopback activation failure to a clear error.
fn process_loopback_error(hr: HRESULT) -> CaptureError {
    if hr == E_ACCESSDENIED {
        CaptureError::PermissionDenied(format!("process loopback activation denied ({hr})"))
    } else {
        CaptureError::Unsupported(format!(
            "process loopback activation failed ({hr}); {PROCESS_LOOPBACK_REQUIREMENT}"
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// Session enumeration
// ---------------------------------------------------------------------------------------------

/// Enumerates audio sessions on every active render endpoint. Runs on an MTA thread.
fn enumerate_sessions() -> Result<Vec<CaptureApp>, CaptureError> {
    let enumerator = device_enumerator()?;
    // SAFETY: valid enumerator on this MTA thread.
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|e| map_error("IMMDeviceEnumerator::EnumAudioEndpoints", &e))?;
    // SAFETY: valid collection.
    let count = unsafe { devices.GetCount() }
        .map_err(|e| map_error("IMMDeviceCollection::GetCount", &e))?;
    let own_pid = std::process::id();
    let mut found = Vec::new();
    for index in 0..count {
        // SAFETY: `index < count`.
        let device = match unsafe { devices.Item(index) } {
            Ok(device) => device,
            Err(e) => {
                tracing::debug!(error = %e, index, "skipping audio endpoint");
                continue;
            }
        };
        if let Err(e) = sessions_of(&device, own_pid, &mut found) {
            tracing::debug!(error = %e, index, "cannot enumerate sessions of endpoint");
        }
    }
    Ok(dedupe_apps(found))
}

/// Appends the capturable sessions of one endpoint to `out`.
fn sessions_of(
    device: &IMMDevice,
    own_pid: u32,
    out: &mut Vec<CaptureApp>,
) -> Result<(), CaptureError> {
    // SAFETY: valid endpoint; no activation parameters.
    let manager = unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }
        .map_err(|e| map_error("IMMDevice::Activate(IAudioSessionManager2)", &e))?;
    // SAFETY: valid session manager.
    let sessions = unsafe { manager.GetSessionEnumerator() }
        .map_err(|e| map_error("IAudioSessionManager2::GetSessionEnumerator", &e))?;
    // SAFETY: valid session enumerator.
    let count = unsafe { sessions.GetCount() }
        .map_err(|e| map_error("IAudioSessionEnumerator::GetCount", &e))?;
    for index in 0..count {
        // SAFETY: `index < count`.
        let Ok(control) = (unsafe { sessions.GetSession(index) }) else {
            continue;
        };
        let Ok(control) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        // SAFETY: valid session control. S_OK means "this is the system sounds session".
        if unsafe { control.IsSystemSoundsSession() } == S_OK {
            continue;
        }
        // SAFETY: valid session control.
        if unsafe { control.GetState() }.is_ok_and(|state| state == AudioSessionStateExpired) {
            continue;
        }
        // SAFETY: valid session control. For multi-process sessions this still returns the
        // initial process (with AUDCLNT_S_NO_SINGLE_PROCESS, a success code).
        let Ok(pid) = (unsafe { control.GetProcessId() }) else {
            continue;
        };
        if pid == 0 || pid == own_pid {
            continue;
        }
        let name = process_image_path(pid)
            .map(|path| display_name_from_path(&path))
            .filter(|name| !name.is_empty())
            .or_else(|| session_display_name(&control))
            .unwrap_or_else(|| format!("Process {pid}"));
        out.push(CaptureApp { pid, name });
    }
    Ok(())
}

/// The session's display name, unless empty or an unresolved `@resource` reference.
fn session_display_name(control: &IAudioSessionControl2) -> Option<String> {
    // SAFETY: valid session control; the returned string is CoTaskMem-allocated and ours.
    let raw = unsafe { control.GetDisplayName() }.ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a valid NUL-terminated wide string until freed below.
    let name = unsafe { raw.to_string() }.ok();
    // SAFETY: frees the CoTaskMem string exactly once.
    unsafe { CoTaskMemFree(Some(raw.0.cast_const().cast())) };
    name.map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty() && !n.starts_with('@'))
}

/// Full image path of a process, if we may query it.
fn process_image_path(pid: u32) -> Option<String> {
    let process = open_process_handle(pid).ok()?;
    let mut buf = [0_u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `process` is a valid handle with PROCESS_QUERY_LIMITED_INFORMATION; `buf` holds
    // `len` wide chars and `len` is updated to the number written.
    unsafe {
        QueryFullProcessImageNameW(
            *process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    let written = buf.get(..len as usize)?;
    Some(String::from_utf16_lossy(written))
}

/// Opens a process with `PROCESS_QUERY_LIMITED_INFORMATION`.
fn open_process_handle(pid: u32) -> windows::core::Result<Owned<HANDLE>> {
    // SAFETY: plain Win32 call; the returned handle is owned and closed by `Owned`.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? };
    // SAFETY: `handle` was just opened by us and is closed exactly once by `Owned`.
    Ok(unsafe { Owned::new(handle) })
}

/// Fails with `NotFound` if no process `pid` exists (access denied means it exists).
fn ensure_process_exists(pid: u32) -> Result<(), CaptureError> {
    match open_process_handle(pid) {
        Ok(_) => Ok(()),
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Ok(()),
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => {
            Err(CaptureError::NotFound(format!("no process with pid {pid}")))
        }
        Err(e) => Err(map_error("OpenProcess", &e)),
    }
}

/// `C:\Program Files\Foo\foo.exe` → `foo`.
fn display_name_from_path(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    };
    stem.trim().to_owned()
}

/// Keeps the first entry per pid and sorts by name (case-insensitive), then pid.
fn dedupe_apps(mut apps: Vec<CaptureApp>) -> Vec<CaptureApp> {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(app.pid));
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.pid.cmp(&b.pid))
    });
    apps
}

// ---------------------------------------------------------------------------------------------
// Formats and sample conversion (pure helpers)
// ---------------------------------------------------------------------------------------------

/// A 32-bit float `WAVEFORMATEX` (`cbSize = 0`).
fn float_format(sample_rate: u32, channels: u16) -> WAVEFORMATEX {
    plain_format(WAVE_FORMAT_IEEE_FLOAT as u16, sample_rate, channels, 32)
}

/// A 16-bit integer PCM `WAVEFORMATEX` (`cbSize = 0`).
fn pcm16_format(sample_rate: u32, channels: u16) -> WAVEFORMATEX {
    plain_format(WAVE_FORMAT_PCM as u16, sample_rate, channels, 16)
}

fn plain_format(tag: u16, sample_rate: u32, channels: u16, bits: u16) -> WAVEFORMATEX {
    let block_align = channels * (bits / 8);
    WAVEFORMATEX {
        wFormatTag: tag,
        nChannels: channels,
        nSamplesPerSec: sample_rate,
        nAvgBytesPerSec: sample_rate * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

/// Sample encoding of a native WASAPI buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleKind {
    /// Unsigned 8-bit PCM.
    U8,
    /// Signed 16-bit PCM.
    I16,
    /// Signed 24-bit PCM, packed in 3 bytes.
    I24,
    /// Signed 32-bit PCM container (also 24-in-32, which is left-justified).
    I32,
    /// 32-bit IEEE float.
    F32,
}

/// Layout of the frames WASAPI delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeFormat {
    kind: SampleKind,
    channels: usize,
    sample_rate: u32,
    /// Bytes per frame.
    block_align: usize,
}

impl NativeFormat {
    /// What we push into the ring: stereo `f32` at the native rate.
    fn output(&self) -> AudioFormat {
        AudioFormat::new(self.sample_rate, OUT_CHANNELS as u16)
    }

    /// Parses a `WAVEFORMATEX` (reading the `WAVEFORMATEXTENSIBLE` extension when present).
    ///
    /// # Safety
    /// `ptr` must point at a readable `WAVEFORMATEX` followed by `cbSize` extension bytes in the
    /// same allocation (any alignment).
    unsafe fn from_waveformat(ptr: *const WAVEFORMATEX) -> Result<Self, CaptureError> {
        // SAFETY: readable per the caller contract; `read_unaligned` copes with packed(1).
        let format = unsafe { std::ptr::read_unaligned(ptr) };
        let tag = format.wFormatTag;
        let sub_format = if u32::from(tag) == WAVE_FORMAT_EXTENSIBLE
            && usize::from(format.cbSize)
                >= std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()
        {
            // SAFETY: cbSize says the extensible fields follow the header in the same
            // allocation (caller contract); `read_unaligned` copes with the packed layout.
            let extensible =
                unsafe { std::ptr::read_unaligned(ptr.cast::<WAVEFORMATEXTENSIBLE>()) };
            Some(extensible.SubFormat)
        } else {
            None
        };
        Self::from_fields(
            tag,
            format.nChannels,
            format.nSamplesPerSec,
            format.wBitsPerSample,
            format.nBlockAlign,
            sub_format,
        )
    }

    /// Validates the raw fields of a wave format.
    fn from_fields(
        tag: u16,
        channels: u16,
        sample_rate: u32,
        bits: u16,
        block_align: u16,
        sub_format: Option<GUID>,
    ) -> Result<Self, CaptureError> {
        let bad = |why: String| CaptureError::Format(format!("WASAPI format: {why}"));
        let is_float = match (u32::from(tag), sub_format) {
            (WAVE_FORMAT_IEEE_FLOAT, _) => true,
            (WAVE_FORMAT_PCM, _) => false,
            (WAVE_FORMAT_EXTENSIBLE, Some(sub)) if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT => true,
            (WAVE_FORMAT_EXTENSIBLE, Some(sub)) if sub == KSDATAFORMAT_SUBTYPE_PCM => false,
            (WAVE_FORMAT_EXTENSIBLE, sub) => {
                return Err(bad(format!("unsupported extensible sub-format {sub:?}")))
            }
            (other, _) => return Err(bad(format!("unsupported format tag {other:#x}"))),
        };
        let kind = match (is_float, bits) {
            (true, 32) => SampleKind::F32,
            (false, 8) => SampleKind::U8,
            (false, 16) => SampleKind::I16,
            (false, 24) => SampleKind::I24,
            (false, 32) => SampleKind::I32,
            (float, bits) => {
                let what = if float { "float" } else { "integer" };
                return Err(bad(format!("unsupported {bits}-bit {what} samples")));
            }
        };
        if channels == 0 {
            return Err(bad("zero channels".to_owned()));
        }
        if sample_rate == 0 {
            return Err(bad("zero sample rate".to_owned()));
        }
        let expected_align = usize::from(channels) * usize::from(bits / 8);
        if usize::from(block_align) != expected_align {
            return Err(bad(format!(
                "block align {block_align} does not match {channels} x {bits}-bit samples"
            )));
        }
        Ok(Self {
            kind,
            channels: usize::from(channels),
            sample_rate,
            block_align: expected_align,
        })
    }

    /// Bytes per single-channel sample.
    fn sample_bytes(&self) -> usize {
        self.block_align / self.channels.max(1)
    }
}

/// Decodes one little-endian sample to `f32` in [-1, 1]. `bytes` must hold at least
/// `kind`'s sample size; missing bytes decode as silence.
fn decode_sample(kind: SampleKind, bytes: &[u8]) -> f32 {
    match (kind, bytes) {
        (SampleKind::U8, [b, ..]) => (f32::from(*b) - 128.0) / 128.0,
        (SampleKind::I16, [b0, b1, ..]) => f32::from(i16::from_le_bytes([*b0, *b1])) / 32_768.0,
        (SampleKind::I24, [b0, b1, b2, ..]) => {
            // Place the 24 bits in the top of an i32, then shift back to sign-extend.
            let v = i32::from_le_bytes([0, *b0, *b1, *b2]) >> 8;
            v as f32 / 8_388_608.0
        }
        (SampleKind::I32, [b0, b1, b2, b3, ..]) => {
            i32::from_le_bytes([*b0, *b1, *b2, *b3]) as f32 / 2_147_483_648.0
        }
        (SampleKind::F32, [b0, b1, b2, b3, ..]) => f32::from_le_bytes([*b0, *b1, *b2, *b3]),
        _ => 0.0,
    }
}

/// Converts whole native frames from `src` into interleaved stereo `f32` in `dst` and returns
/// the number of frames written (limited by both buffers; a trailing partial frame is ignored).
///
/// Mono is duplicated to both channels; with more than two channels the first two (front
/// left/right in the standard WAVE channel order) are kept. This path only runs when the
/// endpoint refused `AUTOCONVERTPCM` to stereo, which is rare. Allocation-free.
fn convert_to_stereo_f32(format: &NativeFormat, src: &[u8], dst: &mut [f32]) -> usize {
    let sample_bytes = format.sample_bytes();
    if format.block_align == 0 || sample_bytes == 0 {
        return 0;
    }
    let right_offset = if format.channels > 1 { sample_bytes } else { 0 };
    let mut frames = 0;
    for (frame, out) in src
        .chunks_exact(format.block_align)
        .zip(dst.chunks_exact_mut(OUT_CHANNELS))
    {
        let left = decode_sample(format.kind, frame);
        let right = frame
            .get(right_offset..)
            .map_or(left, |rest| decode_sample(format.kind, rest));
        if let [l, r] = out {
            *l = left;
            *r = right;
        }
        frames += 1;
    }
    frames
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// `true` for HRESULTs meaning "the endpoint went away; re-open it".
fn is_device_lost(hr: HRESULT) -> bool {
    hr == AUDCLNT_E_DEVICE_INVALIDATED || hr == AUDCLNT_E_SERVICE_NOT_RUNNING
}

/// Maps a failed WASAPI/COM call to a [`CaptureError`].
fn map_error(what: &str, error: &windows::core::Error) -> CaptureError {
    let hr = error.code();
    let detail = format!("{what} failed: {error} ({hr})");
    if hr == E_ACCESSDENIED || hr == ERROR_ACCESS_DENIED.to_hresult() {
        CaptureError::PermissionDenied(detail)
    } else if hr == AUDCLNT_E_UNSUPPORTED_FORMAT || hr == E_INVALIDARG {
        CaptureError::Format(detail)
    } else if hr == E_NOTIMPL || hr == E_NOINTERFACE || hr == REGDB_E_CLASSNOTREG {
        CaptureError::Unsupported(detail)
    } else if hr == ERROR_NOT_FOUND.to_hresult() || is_device_lost(hr) {
        CaptureError::NotFound(detail)
    } else {
        CaptureError::Backend(detail)
    }
}

// ---------------------------------------------------------------------------------------------
// RAII helpers
// ---------------------------------------------------------------------------------------------

/// A Win32 event object, closed on drop.
struct Event(Owned<HANDLE>);

// SAFETY: event objects are kernel objects; SetEvent/WaitFor* on the same handle are
// thread-safe, and the handle is only closed when the last owner drops it.
unsafe impl Send for Event {}
// SAFETY: see `Send`; `&Event` only permits SetEvent/wait, which are thread-safe.
unsafe impl Sync for Event {}

impl Event {
    /// Creates an unnamed, initially non-signalled event.
    fn new(manual_reset: bool) -> Result<Self, CaptureError> {
        // SAFETY: plain Win32 call without security attributes or name.
        let handle = unsafe { CreateEventW(None, manual_reset, false, PCWSTR::null()) }
            .map_err(|e| map_error("CreateEventW", &e))?;
        // SAFETY: we own the new handle; `Owned` closes it exactly once.
        Ok(Self(unsafe { Owned::new(handle) }))
    }

    fn raw(&self) -> HANDLE {
        *self.0
    }

    fn set(&self) {
        // SAFETY: valid event handle.
        if let Err(e) = unsafe { SetEvent(self.raw()) } {
            tracing::error!(error = %e, "SetEvent failed");
        }
    }

    /// Waits up to `ms`; `true` if the event is (or becomes) signalled.
    fn wait(&self, ms: u32) -> bool {
        // SAFETY: valid event handle.
        let r: WAIT_EVENT = unsafe { WaitForSingleObject(self.raw(), ms) };
        r == WAIT_OBJECT_0
    }
}

/// COM initialisation of the current thread, balanced with `CoUninitialize` on drop.
struct ComApartment {
    uninit: bool,
    /// COM initialisation is per thread: keep this guard `!Send`.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComApartment {
    /// Enters the multithreaded apartment. A thread already in an STA keeps it (and we do not
    /// uninitialise it).
    fn enter_mta() -> Result<Self, CaptureError> {
        // SAFETY: plain COM initialisation of the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let uninit = if hr.is_ok() {
            // S_OK or S_FALSE: both must be balanced by CoUninitialize.
            true
        } else if hr == RPC_E_CHANGED_MODE {
            false
        } else {
            return Err(CaptureError::Backend(format!(
                "CoInitializeEx failed ({hr})"
            )));
        };
        Ok(Self {
            uninit,
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful CoInitializeEx on this same thread (the guard is
            // !Send); all COM objects of this thread were released before the guard drops.
            unsafe { CoUninitialize() };
        }
    }
}

/// MMCSS "Pro Audio" registration of the capture thread (best effort).
struct MmcssRegistration(Option<HANDLE>);

impl MmcssRegistration {
    fn pro_audio() -> Self {
        let mut task_index = 0_u32;
        // SAFETY: static task name; `task_index` is a live local.
        let handle = unsafe {
            AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task_index)
        };
        match handle {
            Ok(handle) => Self(Some(handle)),
            Err(e) => {
                tracing::debug!(error = %e, "MMCSS registration failed; continuing without it");
                Self(None)
            }
        }
    }
}

impl Drop for MmcssRegistration {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // SAFETY: `handle` came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::Audio::{eCapture, eCommunications, eMultimedia};

    fn fmt(kind: SampleKind, channels: usize) -> NativeFormat {
        let bytes = match kind {
            SampleKind::U8 => 1,
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
            SampleKind::I32 | SampleKind::F32 => 4,
        };
        NativeFormat {
            kind,
            channels,
            sample_rate: 48_000,
            block_align: bytes * channels,
        }
    }

    fn parse(f: &WAVEFORMATEX) -> NativeFormat {
        // SAFETY: a complete WAVEFORMATEX with cbSize 0.
        unsafe { NativeFormat::from_waveformat(f) }.expect("parse")
    }

    fn parse_ext(ext: &WAVEFORMATEXTENSIBLE) -> NativeFormat {
        // SAFETY: the pointer covers the whole WAVEFORMATEXTENSIBLE (header + 22 bytes).
        unsafe { NativeFormat::from_waveformat(std::ptr::from_ref(ext).cast()) }.expect("parse")
    }

    #[test]
    fn float_format_describes_48k_stereo_f32() {
        let f = float_format(48_000, 2);
        assert_eq!({ f.wFormatTag }, WAVE_FORMAT_IEEE_FLOAT as u16);
        assert_eq!({ f.nChannels }, 2);
        assert_eq!({ f.nSamplesPerSec }, 48_000);
        assert_eq!({ f.wBitsPerSample }, 32);
        assert_eq!({ f.nBlockAlign }, 8);
        assert_eq!({ f.nAvgBytesPerSec }, 384_000);
        assert_eq!({ f.cbSize }, 0);
        let parsed = parse(&f);
        assert_eq!(parsed, fmt(SampleKind::F32, 2));
        assert_eq!(parsed.output(), AudioFormat::INTERNAL);
    }

    #[test]
    fn pcm16_format_is_consistent() {
        let f = pcm16_format(44_100, 2);
        assert_eq!({ f.wFormatTag }, WAVE_FORMAT_PCM as u16);
        assert_eq!({ f.nBlockAlign }, 4);
        assert_eq!({ f.nAvgBytesPerSec }, 176_400);
        let parsed = parse(&f);
        assert_eq!(parsed.kind, SampleKind::I16);
        assert_eq!(parsed.output(), AudioFormat::new(44_100, 2));
    }

    #[test]
    fn extensible_formats_use_the_sub_format() {
        let extensible = |rate, channels, bits, sub_format| {
            let mut header = plain_format(WAVE_FORMAT_EXTENSIBLE as u16, rate, channels, bits);
            header.cbSize = 22;
            WAVEFORMATEXTENSIBLE {
                Format: header,
                SubFormat: sub_format,
                ..Default::default()
            }
        };
        let parsed = parse_ext(&extensible(48_000, 6, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT));
        assert_eq!(parsed, fmt(SampleKind::F32, 6));

        let parsed = parse_ext(&extensible(96_000, 2, 24, KSDATAFORMAT_SUBTYPE_PCM));
        assert_eq!(parsed.kind, SampleKind::I24);
        assert_eq!(parsed.block_align, 6);
        assert_eq!(parsed.output(), AudioFormat::new(96_000, 2));

        // A 16-bit extensible header whose cbSize is too small to carry a sub-format.
        let mut short = extensible(48_000, 2, 16, KSDATAFORMAT_SUBTYPE_PCM);
        short.Format.cbSize = 0;
        // SAFETY: the pointer covers the whole struct.
        let parsed = unsafe { NativeFormat::from_waveformat(std::ptr::from_ref(&short).cast()) };
        assert!(matches!(parsed, Err(CaptureError::Format(_))));
    }

    #[test]
    fn rejects_unusable_formats() {
        // 64-bit float.
        assert!(NativeFormat::from_fields(3, 2, 48_000, 64, 16, None).is_err());
        // Zero channels / rate.
        assert!(NativeFormat::from_fields(3, 0, 48_000, 32, 0, None).is_err());
        assert!(NativeFormat::from_fields(3, 2, 0, 32, 8, None).is_err());
        // Inconsistent block align.
        assert!(NativeFormat::from_fields(1, 2, 48_000, 16, 3, None).is_err());
        // Unknown tag, and extensible without a readable sub-format.
        assert!(NativeFormat::from_fields(0x55, 2, 48_000, 16, 4, None).is_err());
        assert!(NativeFormat::from_fields(0xFFFE, 2, 48_000, 16, 4, None).is_err());
        assert!(matches!(
            NativeFormat::from_fields(1, 2, 48_000, 12, 3, None),
            Err(CaptureError::Format(_))
        ));
    }

    #[test]
    fn decodes_integer_samples() {
        assert_eq!(decode_sample(SampleKind::U8, &[128]), 0.0);
        assert_eq!(decode_sample(SampleKind::U8, &[0]), -1.0);
        assert_eq!(
            decode_sample(SampleKind::I16, &i16::MIN.to_le_bytes()),
            -1.0
        );
        assert_eq!(
            decode_sample(SampleKind::I16, &16_384_i16.to_le_bytes()),
            0.5
        );
        // 24-bit: 0x800000 is the most negative value, 0x400000 is +0.5.
        assert_eq!(decode_sample(SampleKind::I24, &[0x00, 0x00, 0x80]), -1.0);
        assert_eq!(decode_sample(SampleKind::I24, &[0x00, 0x00, 0x40]), 0.5);
        assert_eq!(
            decode_sample(SampleKind::I24, &[0xFF, 0xFF, 0xFF]),
            -1.0 / 8_388_608.0
        );
        assert_eq!(
            decode_sample(SampleKind::I32, &i32::MIN.to_le_bytes()),
            -1.0
        );
        assert_eq!(
            decode_sample(SampleKind::F32, &0.25_f32.to_le_bytes()),
            0.25
        );
        // Too few bytes decode as silence instead of panicking.
        assert_eq!(decode_sample(SampleKind::I32, &[1, 2]), 0.0);
    }

    #[test]
    fn converts_mono_to_stereo() {
        let format = fmt(SampleKind::I16, 1);
        let src: Vec<u8> = [16_384_i16, -16_384]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut dst = [9.0_f32; 4];
        assert_eq!(convert_to_stereo_f32(&format, &src, &mut dst), 2);
        assert_eq!(dst, [0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn keeps_front_pair_of_multichannel_audio() {
        let format = fmt(SampleKind::F32, 6);
        let frame = [0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        let src: Vec<u8> = frame
            .iter()
            .chain(frame.iter())
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut dst = [0.0_f32; 4];
        assert_eq!(convert_to_stereo_f32(&format, &src, &mut dst), 2);
        assert_eq!(dst, [0.1, 0.2, 0.1, 0.2]);
    }

    #[test]
    fn conversion_is_bounded_by_both_buffers() {
        let format = fmt(SampleKind::I24, 2);
        // Three whole frames plus two stray bytes.
        let mut src = vec![0_u8; 3 * 6];
        src.extend_from_slice(&[1, 2]);
        let mut big = [1.0_f32; 16];
        assert_eq!(convert_to_stereo_f32(&format, &src, &mut big), 3);
        assert!(big[..6].iter().all(|s| *s == 0.0));
        assert!(big[6..].iter().all(|s| *s == 1.0));
        let mut small = [1.0_f32; 4];
        assert_eq!(convert_to_stereo_f32(&format, &src, &mut small), 2);
    }

    /// Collects every push: the chunk lengths and the concatenated samples.
    #[derive(Default)]
    struct Collected {
        chunks: Vec<usize>,
        samples: Vec<f32>,
    }

    impl Collected {
        fn sink(&mut self) -> impl FnMut(&[f32]) + '_ {
            |chunk: &[f32]| {
                self.chunks.push(chunk.len());
                self.samples.extend_from_slice(chunk);
            }
        }
    }

    #[test]
    fn scratch_len_has_a_floor_and_is_stereo() {
        assert_eq!(scratch_len(0), MIN_SCRATCH_FRAMES * 2);
        assert_eq!(scratch_len(480), MIN_SCRATCH_FRAMES * 2);
        assert_eq!(scratch_len(10_000), 20_000);
    }

    #[test]
    fn silence_is_split_into_scratch_sized_chunks() {
        let mut scratch = vec![7.0_f32; scratch_len(0)];
        let mut got = Collected::default();
        push_silence(&mut got.sink(), 10_000 * OUT_CHANNELS, &mut scratch);
        assert_eq!(got.chunks, vec![9_600, 9_600, 800]);
        assert_eq!(got.samples.len(), 20_000);
        assert!(got.samples.iter().all(|s| *s == 0.0));

        // Zero samples push nothing; an empty scratch must not spin forever.
        let mut got = Collected::default();
        push_silence(&mut got.sink(), 0, &mut scratch);
        push_silence(&mut got.sink(), 100, &mut []);
        assert!(got.chunks.is_empty());
    }

    #[test]
    fn converted_packets_keep_order_across_scratch_chunks() {
        let format = fmt(SampleKind::I16, 2);
        let frames = 10_000_usize;
        // Left = +n, right = -n (small values, exactly representable after scaling).
        let values: Vec<i16> = (0..frames)
            .flat_map(|n| {
                let v = (n % 16_000) as i16;
                [v, -v]
            })
            .collect();
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut scratch = vec![0.0_f32; scratch_len(0)];
        let mut got = Collected::default();
        push_converted(&mut got.sink(), &format, &bytes, &mut scratch);
        assert_eq!(got.chunks, vec![9_600, 9_600, 800]);
        let expected: Vec<f32> = values.iter().map(|v| f32::from(*v) / 32_768.0).collect();
        assert_eq!(got.samples, expected);
    }

    #[test]
    fn converted_mono_is_duplicated_across_chunks() {
        let format = fmt(SampleKind::F32, 1);
        let frames = 5_000_usize;
        let values: Vec<f32> = (0..frames).map(|n| n as f32 / 8_192.0).collect();
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut scratch = vec![0.0_f32; scratch_len(0)];
        let mut got = Collected::default();
        push_converted(&mut got.sink(), &format, &bytes, &mut scratch);
        assert_eq!(got.chunks, vec![9_600, 400]);
        let expected: Vec<f32> = values.iter().flat_map(|v| [*v, *v]).collect();
        assert_eq!(got.samples, expected);
    }

    #[test]
    fn aligned_stereo_f32_takes_the_fast_path() {
        let format = fmt(SampleKind::F32, 2);
        // 10_000 frames, more than the scratch holds: the fast path pushes it in one go.
        let values: Vec<f32> = (0..20_000).map(|n| n as f32 / 32_768.0).collect();
        let mut scratch = vec![0.0_f32; scratch_len(0)];
        // SAFETY: plain reinterpretation of an f32 buffer as its bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), values.len() * 4) };
        let mut got = Collected::default();
        push_converted(&mut got.sink(), &format, bytes, &mut scratch);
        assert_eq!(got.chunks, vec![20_000]);
        assert_eq!(got.samples, values);

        // The same data at an odd address goes through the converting path, chunked.
        let mut shifted = vec![0_u8; bytes.len() + 1];
        shifted[1..].copy_from_slice(bytes);
        let unaligned = &shifted[1..];
        assert_ne!(unaligned.as_ptr().align_offset(4), 0);
        let mut got = Collected::default();
        push_converted(&mut got.sink(), &format, unaligned, &mut scratch);
        assert_eq!(got.chunks, vec![9_600, 9_600, 800]);
        assert_eq!(got.samples, values);
    }

    #[test]
    fn default_device_listener_signals_only_console_render_changes() {
        let changed = Arc::new(Event::new(false).expect("event"));
        let listener: IMMNotificationClient = DefaultDeviceListener {
            changed: Arc::clone(&changed),
        }
        .into();
        let fire = |flow, role| {
            // SAFETY: calling our own COM object through its interface; a null device id is
            // allowed (no default device) and never read.
            unsafe { listener.OnDefaultDeviceChanged(flow, role, PCWSTR::null()) }
                .expect("callback");
        };
        fire(eCapture, eConsole);
        fire(eRender, eMultimedia);
        fire(eRender, eCommunications);
        assert!(!changed.wait(0));
        fire(eRender, eConsole);
        assert!(changed.wait(0));
        // Auto-reset: consumed by the wait above.
        assert!(!changed.wait(0));
        assert!(is_our_default_change(eRender, eConsole));
        assert!(!is_our_default_change(eCapture, eConsole));
    }

    #[test]
    fn activation_params_are_a_self_contained_blob() {
        let params = ActivationParams::new(1234, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE);
        let prop = params.prop();
        // SAFETY: `prop` points at the live PROPVARIANT built by `new`, whose `vt` is VT_BLOB.
        let (vt, blob) = unsafe {
            let inner = &(*prop).Anonymous.Anonymous;
            (inner.vt, inner.Anonymous.blob)
        };
        assert_eq!(vt, VT_BLOB);
        assert_eq!(
            blob.cbSize as usize,
            std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()
        );
        // SAFETY: the blob points at the AUDIOCLIENT_ACTIVATION_PARAMS in the same allocation.
        let decoded = unsafe { &*blob.pBlobData.cast::<AUDIOCLIENT_ACTIVATION_PARAMS>() };
        assert_eq!(
            decoded.ActivationType,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
        );
        // SAFETY: ProcessLoopbackParams is the active union member for this activation type.
        let loopback = unsafe { decoded.Anonymous.ProcessLoopbackParams };
        assert_eq!(loopback.TargetProcessId, 1234);
        assert_eq!(
            loopback.ProcessLoopbackMode,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
        );
    }

    #[test]
    fn activation_handler_reports_a_missing_operation_and_owns_the_params() {
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
            done: done_tx,
            _params: ActivationParams::new(1, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
        }
        .into();
        // SAFETY: calling our own COM object; a null operation is handled (E_POINTER).
        unsafe { handler.ActivateCompleted(None) }.expect("callback");
        assert!(matches!(done_rx.try_recv(), Ok(Err(hr)) if hr == E_POINTER));
        // Releasing the last reference drops the sender (and frees the params).
        drop(handler);
        assert!(matches!(
            done_rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn activation_returns_immediately_once_stopped() {
        let stop = Event::new(true).expect("event");
        stop.set();
        let started = Instant::now();
        let result = activate_process_loopback(
            std::process::id(),
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            &stop,
        );
        assert!(matches!(result, Err(CaptureError::Backend(m)) if m.contains("cancelled")));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn display_names_strip_directories_and_exe() {
        assert_eq!(
            display_name_from_path(r"C:\Program Files\Spotify\Spotify.exe"),
            "Spotify"
        );
        assert_eq!(display_name_from_path(r"C:\Games\my.game.EXE"), "my.game");
        assert_eq!(display_name_from_path("plain"), "plain");
        assert_eq!(display_name_from_path(r"C:\x\.exe"), ".exe");
        assert_eq!(display_name_from_path(r"C:\x\tool.bin"), "tool.bin");
    }

    #[test]
    fn apps_are_deduplicated_and_sorted() {
        let app = |pid, name: &str| CaptureApp {
            pid,
            name: name.to_owned(),
        };
        let apps = dedupe_apps(vec![
            app(40, "zoom"),
            app(8, "Chrome"),
            app(40, "zoom (other endpoint)"),
            app(12, "chrome"),
        ]);
        assert_eq!(
            apps,
            vec![app(8, "Chrome"), app(12, "chrome"), app(40, "zoom")]
        );
    }

    #[test]
    fn errors_map_to_meaningful_variants() {
        let err = |hr: HRESULT| map_error("x", &windows::core::Error::from_hresult(hr));
        assert!(matches!(
            err(E_ACCESSDENIED),
            CaptureError::PermissionDenied(_)
        ));
        assert!(matches!(
            err(AUDCLNT_E_UNSUPPORTED_FORMAT),
            CaptureError::Format(_)
        ));
        assert!(matches!(err(E_NOTIMPL), CaptureError::Unsupported(_)));
        assert!(matches!(
            err(AUDCLNT_E_DEVICE_INVALIDATED),
            CaptureError::NotFound(_)
        ));
        assert!(matches!(err(E_POINTER), CaptureError::Backend(_)));
        assert!(
            matches!(process_loopback_error(E_NOTIMPL), CaptureError::Unsupported(m) if m.contains("20348"))
        );
        assert!(matches!(
            process_loopback_error(E_ACCESSDENIED),
            CaptureError::PermissionDenied(_)
        ));
        assert!(is_device_lost(AUDCLNT_E_DEVICE_INVALIDATED));
        assert!(!is_device_lost(E_INVALIDARG));
    }

    #[test]
    fn capabilities_advertise_system_and_per_app() {
        let caps = capabilities();
        assert!(caps.system_mix && caps.per_app && !caps.mutes_local_output);
        assert!(caps.notes.contains("20348"));
    }

    #[test]
    fn describes_each_mode() {
        assert!(Mode::SystemLoopback.describe().contains("loopback"));
        let excl = Mode::Process {
            pid: 1,
            include_tree: false,
        };
        assert!(excl.describe().contains("excluding"));
        let proc_ = Mode::Process {
            pid: 42,
            include_tree: true,
        };
        assert!(proc_.describe().contains("42"));
    }

    #[test]
    fn open_process_validates_the_pid() {
        assert!(matches!(
            open_process(0),
            Err(CaptureError::InvalidArgument(_))
        ));
        // PIDs are multiples of 4 below 2^32; this one never exists.
        assert!(matches!(
            open_process(0xFFFF_FFF0),
            Err(CaptureError::NotFound(_))
        ));
    }

    #[test]
    fn event_signals_across_threads() {
        let event = Arc::new(Event::new(true).expect("event"));
        assert!(!event.wait(0));
        let remote = Arc::clone(&event);
        std::thread::spawn(move || remote.set())
            .join()
            .expect("join");
        assert!(event.wait(1_000));
    }

    #[test]
    fn opening_reports_a_format_or_fails_cleanly() {
        // Exercises worker spawn, COM init, activation, Initialize and the drop/join path
        // without starting. Runners without an audio endpoint must get a clean error.
        for exclude_self in [false, true] {
            match open_system(exclude_self) {
                Ok(mut source) => {
                    let format = source.format();
                    assert_eq!(format.channels, 2);
                    assert!(format.sample_rate >= 8_000);
                    assert!(!source.describe().is_empty());
                    source.stop();
                    source.stop(); // idempotent
                }
                Err(e) => assert!(
                    matches!(
                        e,
                        CaptureError::NotFound(_)
                            | CaptureError::Unsupported(_)
                            | CaptureError::Backend(_)
                            | CaptureError::Format(_)
                    ),
                    "unexpected error {e:?}"
                ),
            }
        }
    }

    #[test]
    fn list_apps_never_reports_self_or_pid_zero() {
        // CI runners may have no audio endpoint (or no audio service) at all.
        match list_apps() {
            Ok(apps) => {
                assert!(apps
                    .iter()
                    .all(|a| a.pid != 0 && a.pid != std::process::id()));
                assert!(apps.iter().all(|a| !a.name.is_empty()));
                let pids: std::collections::HashSet<_> = apps.iter().map(|a| a.pid).collect();
                assert_eq!(pids.len(), apps.len());
            }
            Err(e) => assert!(
                matches!(e, CaptureError::Backend(_) | CaptureError::NotFound(_)),
                "unexpected error {e:?}"
            ),
        }
    }
}
