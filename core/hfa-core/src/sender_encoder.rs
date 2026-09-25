//! The sender's encoder thread (soft real-time): capture ring → 48 kHz stereo → exact Opus
//! frames → media payload container → [`MediaSender`].
//!
//! # Behaviour
//!
//! - The thread runs for the whole life of the sender, independent of the connection: the
//!   capture keeps running while the control task reconnects, and captured audio is metered
//!   and discarded while there is no stream. A new stream (a new [`MediaSender`] = new
//!   `stream_id` and key) gets a fresh Opus encoder and timestamps starting at 0.
//! - It never busy-spins: when the ring holds no complete frame of input it sleeps a quarter
//!   of a frame.
//! - Input in another format is folded to stereo ([`hfa_audio::convert::to_stereo`]) and
//!   resampled to 48 kHz ([`StreamResampler`]).
//! - **Silence / DTX:** a frame whose peak is below [`SILENCE_DB`] is silent. After more than
//!   [`DTX_AFTER`] of silence the thread stops sending audio and sends a
//!   [`FLAG_DTX`] keep-alive (empty payload) every [`KEEPALIVE_INTERVAL`] instead; the first
//!   non-silent frame resumes immediately with a fresh encoder and [`FLAG_RESET`] (the hub
//!   resets that stream's jitter buffer and decoder). A capture that delivers **nothing**
//!   for [`DTX_AFTER`] (e.g. WASAPI loopback while nothing plays) is treated exactly like
//!   silence, never as a timing break. During DTX the media timestamp follows the wall clock.
//! - **Redundancy:** when enabled, every audio datagram also carries the previous frame's
//!   Opus packet (see [`crate::payload`], `FLAG_FEC`), as long as that frame had `seq − 1`
//!   and both fit one datagram.
//! - Fatal send errors (anything but a transient drop) end the stream and are reported to
//!   the control task, which reconnects. The thread itself never logs.
//!
//! After warm-up the loop allocates only when a new stream starts or DTX ends (a new Opus
//! encoder).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hfa_audio::opus::MAX_OPUS_PACKET;
use hfa_audio::{AudioFormat, LevelMeter, OpusConfig, OpusEncoder, StreamResampler};
use hfa_capture::{CaptureSource, PcmSource};
use hfa_proto::{FLAG_DTX, FLAG_FEC, FLAG_RESET};
use tokio::sync::mpsc::UnboundedSender;

use crate::media::{MediaSender, MAX_MEDIA_PAYLOAD};
use crate::payload;
use crate::sender_adapt::Adaptation;
use crate::CoreError;

/// Peak level (dBFS) below which a frame counts as silence.
pub(crate) const SILENCE_DB: f32 = -70.0;
/// Silence (or a capture delivering nothing) longer than this switches to DTX.
pub(crate) const DTX_AFTER: Duration = Duration::from_millis(200);
/// Interval between two DTX keep-alives.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(100);
/// Input read per step, in ms of capture audio.
const READ_BLOCK_MS: u32 = 20;
/// The 48 kHz FIFO never holds more than this many frames (older audio is dropped; only
/// reachable if the thread was descheduled for a long time).
const MAX_FIFO_FRAMES: usize = 25;

/// Commands from the control task.
pub(crate) enum EncoderCommand {
    /// Start sending on a new stream.
    Stream {
        /// The stream's media sender (fresh key, `seq` 0, destination set).
        media: MediaSender,
        /// Bitrate, redundancy and expected loss to start with.
        adaptation: Adaptation,
    },
    /// Stop sending (connection lost or stopping); keep capturing.
    Clear,
    /// New adaptation decision.
    Adapt(Adaptation),
}

/// Reports to the control task.
#[derive(Debug)]
pub(crate) enum EncoderEvent {
    /// Sending on `stream_id` failed fatally; the stream was dropped.
    StreamFailed {
        /// The stream that failed.
        stream_id: u32,
        /// Why.
        error: CoreError,
    },
    /// A non-fatal problem worth reporting (encoder errors...).
    Warning(String),
}

/// State shared with the engine (lock-free).
#[derive(Debug)]
pub(crate) struct EncoderShared {
    /// Set to stop the thread.
    pub stop: AtomicBool,
    /// RMS level of the last captured frame, dBFS (`f32` bits).
    level_db: AtomicU32,
    /// Audio datagrams sent (all streams).
    pub packets_sent: AtomicU64,
    /// DTX keep-alives sent (all streams).
    pub keepalives_sent: AtomicU64,
}

impl EncoderShared {
    pub(crate) fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            level_db: AtomicU32::new(hfa_audio::meter::SILENCE_DB.to_bits()),
            packets_sent: AtomicU64::new(0),
            keepalives_sent: AtomicU64::new(0),
        }
    }

    /// RMS level of the last captured frame in dBFS.
    pub(crate) fn level_db(&self) -> f32 {
        f32::from_bits(self.level_db.load(Ordering::Relaxed))
    }
}

/// Fixed parameters of the encoder thread.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EncoderParams {
    /// Opus frame duration (10 or 20 ms).
    pub frame_ms: u32,
    /// Opus in-band FEC.
    pub fec: bool,
}

/// DTX bookkeeping of a stream.
#[derive(Debug, Clone, Copy)]
struct Dtx {
    since: Instant,
    /// Media timestamp when DTX started.
    ts0: u32,
    next_keepalive: Instant,
}

/// One outgoing stream.
struct Stream {
    media: MediaSender,
    opus: OpusEncoder,
    adaptation: Adaptation,
    packet: Vec<u8>,
    prev: Vec<u8>,
    /// `seq` the frame in `prev` was sent with.
    prev_seq: Option<u32>,
    payload: Vec<u8>,
    timestamp: u32,
    dtx: Option<Dtx>,
}

/// Media timestamp units (48 kHz samples) elapsed since `since`.
fn elapsed_samples(since: Instant, now: Instant) -> u32 {
    let us = now.saturating_duration_since(since).as_micros();
    (us * 48 / 1000) as u32
}

struct EncoderThread {
    params: EncoderParams,
    shared: Arc<EncoderShared>,
    commands: Receiver<EncoderCommand>,
    events: UnboundedSender<EncoderEvent>,
    source: PcmSource,
    input: AudioFormat,
    raw: Vec<f32>,
    stereo: Vec<f32>,
    resampler: Option<StreamResampler>,
    fifo: Vec<f32>,
    frame: Vec<f32>,
    meter: LevelMeter,
    stream: Option<Stream>,
    silent_for: Duration,
    last_input: Instant,
}

/// Starts the encoder thread. The capture must already be started into the ring whose
/// consumer end is `source`; the thread stops it when it exits.
pub(crate) fn spawn(
    capture: Box<dyn CaptureSource>,
    source: PcmSource,
    input: AudioFormat,
    params: EncoderParams,
    shared: Arc<EncoderShared>,
    commands: Receiver<EncoderCommand>,
    events: UnboundedSender<EncoderEvent>,
) -> Result<JoinHandle<()>, CoreError> {
    let ch = usize::from(input.channels.max(1));
    let block_frames = input.frames_for_ms(READ_BLOCK_MS).max(1);
    let resampler = if input.sample_rate == AudioFormat::INTERNAL.sample_rate {
        None
    } else {
        Some(StreamResampler::new(
            2,
            input.sample_rate,
            AudioFormat::INTERNAL.sample_rate,
            input.frames_for_ms(10).max(1),
        )?)
    };
    let frame_len = AudioFormat::INTERNAL.samples_for_ms(params.frame_ms);
    let mut worker = EncoderThread {
        params,
        shared,
        commands,
        events,
        source,
        input,
        raw: vec![0.0; block_frames * ch],
        stereo: Vec::with_capacity(block_frames * 2),
        resampler,
        fifo: Vec::with_capacity(frame_len * (MAX_FIFO_FRAMES + 4)),
        frame: vec![0.0; frame_len],
        meter: LevelMeter::new(),
        stream: None,
        silent_for: Duration::ZERO,
        last_input: Instant::now(),
    };
    let mut capture = capture;
    thread::Builder::new()
        .name("hfa-encoder".into())
        .spawn(move || {
            worker.run();
            capture.stop();
        })
        .map_err(|e| CoreError::Io(format!("cannot start the encoder thread: {e}")))
}

impl EncoderThread {
    fn run(&mut self) {
        let idle = Duration::from_millis(u64::from(self.params.frame_ms)) / 4;
        while !self.shared.stop.load(Ordering::Acquire) {
            self.handle_commands();
            let got_input = self.read_input();
            while self.fifo.len() >= self.frame.len() {
                let n = self.frame.len();
                self.frame.copy_from_slice(&self.fifo[..n]);
                self.fifo.drain(..n);
                self.process_frame(Instant::now());
            }
            self.dtx_housekeeping(Instant::now());
            if !got_input {
                thread::sleep(idle);
            }
        }
        self.stream = None;
    }

    fn warn(&self, message: String) {
        let _ = self.events.send(EncoderEvent::Warning(message));
    }

    fn handle_commands(&mut self) {
        loop {
            match self.commands.try_recv() {
                Ok(EncoderCommand::Stream { media, adaptation }) => {
                    self.start_stream(media, adaptation)
                }
                Ok(EncoderCommand::Clear) => self.stream = None,
                Ok(EncoderCommand::Adapt(a)) => self.adapt(a),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    // The engine is gone: stop.
                    self.shared.stop.store(true, Ordering::Release);
                    return;
                }
            }
        }
    }

    fn start_stream(&mut self, media: MediaSender, adaptation: Adaptation) {
        match new_encoder(self.params, adaptation) {
            Ok(opus) => {
                self.stream = Some(Stream {
                    media,
                    opus,
                    adaptation,
                    packet: vec![0; MAX_OPUS_PACKET],
                    prev: Vec::with_capacity(MAX_OPUS_PACKET),
                    prev_seq: None,
                    payload: Vec::with_capacity(MAX_MEDIA_PAYLOAD),
                    timestamp: 0,
                    dtx: None,
                });
                // A capture that has been stalled for a while goes straight to DTX.
            }
            Err(error) => {
                let _ = self.events.send(EncoderEvent::StreamFailed {
                    stream_id: media.stream_id(),
                    error,
                });
            }
        }
    }

    fn adapt(&mut self, a: Adaptation) {
        let Some(st) = self.stream.as_mut() else {
            return;
        };
        let mut failed = None;
        if a.bitrate != st.adaptation.bitrate {
            if let Err(e) = st.opus.set_bitrate(a.bitrate) {
                failed = Some(format!("cannot set bitrate {}: {e}", a.bitrate));
            }
        }
        if a.expected_loss != st.adaptation.expected_loss {
            if let Err(e) = st.opus.set_expected_loss(a.expected_loss) {
                failed = Some(format!("cannot set expected loss: {e}"));
            }
        }
        st.adaptation = a;
        if let Some(message) = failed {
            self.warn(message);
        }
    }

    /// Moves available capture input into the 48 kHz stereo FIFO. Returns `false` if the ring
    /// held no complete input frame.
    fn read_input(&mut self) -> bool {
        let ch = usize::from(self.input.channels.max(1));
        let frames = (self.source.available() / ch).min(self.raw.len() / ch);
        if frames == 0 {
            return false;
        }
        let n = frames * ch;
        self.source.pull(&mut self.raw[..n]);
        self.last_input = Instant::now();
        let stereo: &[f32] = if self.input.channels == 2 {
            &self.raw[..n]
        } else {
            hfa_audio::convert::to_stereo(&self.raw[..n], self.input.channels, &mut self.stereo);
            &self.stereo
        };
        match self.resampler.as_mut() {
            None => self.fifo.extend_from_slice(stereo),
            Some(rs) => {
                if let Err(e) = rs.process(stereo, &mut self.fifo) {
                    let _ = self.events.send(EncoderEvent::Warning(format!(
                        "capture resampling failed: {e}"
                    )));
                }
            }
        }
        let max = self.frame.len() * MAX_FIFO_FRAMES;
        if self.fifo.len() > max {
            let excess = (self.fifo.len() - max) / 2 * 2;
            self.fifo.drain(..excess);
        }
        true
    }

    fn frame_duration(&self) -> Duration {
        Duration::from_millis(u64::from(self.params.frame_ms))
    }

    fn frame_samples(&self) -> u32 {
        (self.frame.len() / 2) as u32
    }

    fn process_frame(&mut self, now: Instant) {
        let level = self.meter.process(&self.frame);
        self.shared
            .level_db
            .store(level.rms_db.to_bits(), Ordering::Relaxed);
        let loud = level.peak_db >= SILENCE_DB;
        self.silent_for = if loud {
            Duration::ZERO
        } else {
            self.silent_for.saturating_add(self.frame_duration())
        };
        let frame_samples = self.frame_samples();
        let silent_for = self.silent_for;
        let Some(st) = self.stream.as_mut() else {
            return;
        };
        let mut flags = 0u8;
        if let Some(dtx) = st.dtx {
            if !loud {
                return; // keep-alives are sent by `dtx_housekeeping`
            }
            // Resume at once, with a fresh codec state on both sides.
            st.timestamp = dtx.ts0.wrapping_add(elapsed_samples(dtx.since, now));
            st.dtx = None;
            st.prev_seq = None;
            match new_encoder(self.params, st.adaptation) {
                Ok(opus) => st.opus = opus,
                Err(e) => {
                    let _ = self.events.send(EncoderEvent::Warning(format!(
                        "cannot reset the encoder: {e}"
                    )));
                }
            }
            flags |= FLAG_RESET;
        } else if silent_for > DTX_AFTER {
            st.dtx = Some(Dtx {
                since: now,
                ts0: st.timestamp,
                next_keepalive: now,
            });
            self.dtx_housekeeping(now);
            return;
        }
        let result = encode_and_send(st, &self.frame, flags);
        st.timestamp = st.timestamp.wrapping_add(frame_samples);
        match result {
            Ok(()) => {
                self.shared.packets_sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(Failure::Warning(message)) => self.warn(message),
            Err(Failure::Fatal(error)) => self.fail_stream(error),
        }
    }

    fn fail_stream(&mut self, error: CoreError) {
        if let Some(st) = self.stream.take() {
            let _ = self.events.send(EncoderEvent::StreamFailed {
                stream_id: st.media.stream_id(),
                error,
            });
        }
    }

    /// Enters DTX when the capture stalled, and sends due keep-alives.
    fn dtx_housekeeping(&mut self, now: Instant) {
        let stalled = now.saturating_duration_since(self.last_input) > DTX_AFTER;
        let Some(st) = self.stream.as_mut() else {
            return;
        };
        if st.dtx.is_none() && stalled {
            st.dtx = Some(Dtx {
                since: now,
                ts0: st.timestamp,
                next_keepalive: now,
            });
        }
        let Some(dtx) = st.dtx.as_mut() else {
            return;
        };
        if now < dtx.next_keepalive {
            return;
        }
        dtx.next_keepalive += KEEPALIVE_INTERVAL;
        if dtx.next_keepalive <= now {
            dtx.next_keepalive = now + KEEPALIVE_INTERVAL;
        }
        let ts = dtx.ts0.wrapping_add(elapsed_samples(dtx.since, now));
        st.prev_seq = None;
        match st.media.send(FLAG_DTX, ts, &[]) {
            Ok(_) => {
                self.shared.keepalives_sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => self.fail_stream(error),
        }
    }
}

/// A fresh 48 kHz stereo Opus encoder for the current adaptation.
fn new_encoder(params: EncoderParams, a: Adaptation) -> Result<OpusEncoder, CoreError> {
    Ok(OpusEncoder::new(OpusConfig {
        sample_rate: AudioFormat::INTERNAL.sample_rate,
        channels: AudioFormat::INTERNAL.channels,
        bitrate: a.bitrate,
        frame_ms: params.frame_ms,
        fec: params.fec,
        expected_loss_pct: a.expected_loss,
        low_delay: false,
    })?)
}

enum Failure {
    Warning(String),
    Fatal(CoreError),
}

/// Encodes one frame and sends it (with the previous frame as redundancy when enabled).
fn encode_and_send(st: &mut Stream, frame: &[f32], mut flags: u8) -> Result<(), Failure> {
    let n = st
        .opus
        .encode(frame, &mut st.packet)
        .map_err(|e| Failure::Warning(format!("opus encode failed: {e}")))?;
    let seq = st
        .media
        .next_seq()
        .ok_or(Failure::Fatal(CoreError::SequenceExhausted(
            st.media.stream_id(),
        )))?;
    let primary = &st.packet[..n];
    let redundant = (st.adaptation.redundancy
        && seq > 0
        && st.prev_seq == Some(seq - 1)
        && payload::fits(primary, Some(&st.prev)))
    .then_some(&st.prev[..]);
    if redundant.is_some() {
        flags |= FLAG_FEC;
    }
    if !payload::write(primary, redundant, &mut st.payload) {
        return Err(Failure::Warning(format!(
            "opus packet of {n} bytes does not fit a datagram"
        )));
    }
    let sent = st.media.send(flags, st.timestamp, &st.payload);
    // The frame now belongs to `seq` whether or not the datagram left (a transient drop still
    // consumes the seq), so it is the redundancy for `seq + 1`.
    st.prev.clear();
    st.prev.extend_from_slice(primary);
    st.prev_seq = Some(seq);
    sent.map(drop).map_err(Failure::Fatal)
}
