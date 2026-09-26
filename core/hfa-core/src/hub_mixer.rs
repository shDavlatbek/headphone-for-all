//! The hub's mixer thread (soft real-time) and the per-stream state it shares with the UDP
//! receive task.
//!
//! # Per stream (the `docs/CONTRACTS.md` §4.2 recipe)
//!
//! The receive task pushes authenticated packets into the stream's [`JitterBuffer`] (inside
//! [`StreamShared`], a short `parking_lot` lock). Every mixer tick ([`MIX_FRAME_MS`] of
//! 48 kHz stereo), while the stream's FIFO holds less than one tick, the mixer pops one slot:
//!
//! - `Packet(p)` → decode the payload container's primary entry ([`crate::payload`]);
//! - `Missing{next: Some(n)}` → decode the **redundant copy** in `n` (full recovery), else
//!   Opus in-band FEC from `n` if it has any, else PLC;
//! - `Missing{next: None}` → PLC; `Stretch` → PLC without consuming a packet;
//! - `Skipped(p)` (the buffer cut latency) → decode `p` all the same (PLC for a slot that
//!   never arrived), so the Opus decoder state stays continuous, hand the audio to the
//!   stream's [`Splicer`] instead of playing it, and pop again;
//! - `Underrun` → no audio this tick: the stream is left out of the mixer's inputs (the
//!   mixer fades it back in later).
//!
//! The decoded frame goes through the [`Splicer`] (after a cut it crossfades, pitch-aligned,
//! from what would have been heard next into the audio after the cut, so the cut is
//! inaudible; otherwise the frame passes unchanged) and the stream's [`StreamResampler`]
//! (48 kHz → 48 kHz with the [`DriftController`]'s relative ratio, updated every tick while
//! the buffer is primed) into the FIFO. Before each pop the jitter buffer is told whether the
//! last decoded frame was quiet ([`QUIET_FLOOR_DBFS`], [`QUIET_BELOW_AVERAGE_DB`]), so a due
//! single-frame cut lands in a pause when there is one. A stream reset (`FLAG_RESET`, or the
//! first audio packet after DTX keep-alives) resets the decoder, the splicer, the resampler
//! and the FIFO before the next frame is decoded; the drift estimate is kept (it describes the
//! two clocks, not the stream).
//!
//! # Pacing and output
//!
//! The thread is paced by the output ring: while the ring holds less than the target
//! (`2 × MIX_FRAME_MS` + the output's latency, at most [`MAX_PACING_LATENCY_MS`] of it, +
//! an extra fill that grows by [`UNDERRUN_STEP_MS`] whenever the output ran dry, up to
//! [`MAX_EXTRA_FILL_MS`]; it shrinks again by [`EXTRA_FILL_DECAY_MS`] after every
//! [`EXTRA_FILL_DECAY_AFTER`] seconds without an underrun and is dropped when the output is
//! reopened) it produces one tick, otherwise it sleeps a quarter tick. The
//! output's latency and the ring's underrun counter are re-read every [`OUTPUT_REFRESH`]:
//! a real device only measures its latency once it runs, and a device period larger than
//! the target shows up as underruns. The published latency after the mixer is the ring
//! target plus the output's whole latency. The mix (48 kHz stereo) is converted to the
//! output format (channel map, then resampling to the device rate if needed) and pushed.
//! [`AudioOutput::has_error`] is polled every loop: a failed output is stopped (reported as
//! [`HubEvent::Error`]) and restarted every [`OUTPUT_RETRY`] until that works (a failing
//! restart is reported once per outage) — unless it is not
//! [restartable](AudioOutput::restartable) (a WAV file would be truncated), then it stays
//! stopped; meanwhile the mixer keeps consuming the jitter buffers at wall-clock pace and
//! discards the mix. It does the same while an output that is up has not pulled any audio for
//! [`OUTPUT_STALL`] (a device that stops without reporting an error), so the jitter buffers do
//! not overflow.
//!
//! After warm-up a tick does not allocate (buffers are preallocated; the input list is a
//! fixed array); packet payloads are freed here after decoding and a copy of the next packet
//! is made only when a packet is missing. Locks are only held for a pop or a counter update.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hfa_audio::meter::SILENCE_DB;
use hfa_audio::{
    packet_has_fec, AudioFormat, DriftConfig, DriftController, JitterBuffer, LevelMeter, Mixer,
    MixerConfig, OpusDecoder, Pop, SourceId, Splicer, StreamResampler,
};
use hfa_capture::{AudioOutput, PcmSink, RingStats};
use hfa_proto::{MediaHeader, FLAG_DTX, FLAG_RESET};
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::hub::{HubEvent, MAX_STREAMS};
use crate::{payload, CoreError};

/// Duration of one mixer tick.
pub(crate) const MIX_FRAME_MS: u32 = 10;
/// Frames per tick at 48 kHz.
const MIX_FRAMES: usize = 480;
/// Interleaved stereo samples per tick.
const MIX_SAMPLES: usize = MIX_FRAMES * 2;
/// Longest Opus packet duration (120 ms) in frames at 48 kHz.
const MAX_PACKET_FRAMES: usize = 5760;
/// Capacity of the output ring in ms.
pub(crate) const OUTPUT_RING_MS: usize = 500;
/// Output latency assumed when the output does not know its own.
const DEFAULT_OUTPUT_LATENCY_MS: f32 = 20.0;
/// Largest part of the output's own latency added to the ring's fill target. The output
/// reports its whole playback delay (a Bluetooth link adds 150-250 ms after the device has
/// pulled the audio); buffering all of it again in the ring would only double the latency.
/// Device periods larger than this are covered by the underrun-driven extra fill.
const MAX_PACING_LATENCY_MS: f32 = 40.0;
/// Extra ring fill added after the output ran dry.
const UNDERRUN_STEP_MS: f32 = 10.0;
/// Most extra ring fill.
const MAX_EXTRA_FILL_MS: f32 = 200.0;
/// Extra ring fill given back after [`EXTRA_FILL_DECAY_AFTER`] without an underrun.
const EXTRA_FILL_DECAY_MS: f32 = 5.0;
/// Refreshes ([`OUTPUT_REFRESH`] apart) without an output underrun before the extra fill
/// shrinks by [`EXTRA_FILL_DECAY_MS`] (30 s).
const EXTRA_FILL_DECAY_AFTER: u32 = 30;
/// An output that is up but has not pulled audio for this long (without reporting an error:
/// e.g. a suspended device) is treated like a stopped one: the mixer consumes the streams at
/// wall-clock pace and discards the mix, so the jitter buffers do not overflow. Longer than
/// any fill target (at most 260 ms), so a device with a large period is never mistaken for it.
pub(crate) const OUTPUT_STALL: Duration = Duration::from_millis(300);
/// How often the output's latency and the ring's underruns are re-read.
pub(crate) const OUTPUT_REFRESH: Duration = Duration::from_secs(1);
/// Pause between two attempts to restart a failed output.
pub(crate) const OUTPUT_RETRY: Duration = Duration::from_secs(2);
/// Most ticks produced in one go before the thread checks commands and sleeps again.
const MAX_TICKS_PER_WAKE: usize = 50;
/// A decoded frame below this RMS is quiet (a good place for a latency cut, see
/// [`JitterBuffer::set_quiet`]).
pub(crate) const QUIET_FLOOR_DBFS: f32 = -50.0;
/// A decoded frame this much below the stream's recent level ([`QUIET_AVERAGE_FRAMES`]) is
/// quiet too (a pause in speech or music that is not silent).
pub(crate) const QUIET_BELOW_AVERAGE_DB: f32 = 15.0;
/// Time constant, in decoded frames, of the recent level [`QUIET_BELOW_AVERAGE_DB`] compares
/// with.
const QUIET_AVERAGE_FRAMES: f32 = 50.0;

/// Per-source controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Controls {
    pub gain: f32,
    pub muted: bool,
    pub priority: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
            priority: false,
        }
    }
}

/// Counters maintained by the receive task.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetCounters {
    pub datagrams: u64,
    pub keepalives: u64,
    pub resets: u64,
}

/// Counters maintained by the mixer thread.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MixCounters {
    pub played: u64,
    pub recovered_redundancy: u64,
    pub recovered_fec: u64,
    pub concealed: u64,
    pub underruns: u64,
}

/// State of one stream shared by the receive task, the mixer thread and the stats task.
#[derive(Debug)]
pub(crate) struct StreamShared {
    pub state: Mutex<StreamState>,
}

/// See [`StreamShared`].
#[derive(Debug)]
pub(crate) struct StreamState {
    pub jb: JitterBuffer,
    /// Set by the receive task when the jitter buffer was reset; the mixer then resets the
    /// decoder state before decoding the next frame.
    pub reset_pending: bool,
    /// The last datagram was a DTX keep-alive.
    pub in_dtx: bool,
    /// Arrival of the last authenticated datagram (audio or keep-alive).
    pub last_packet: Instant,
    pub net: NetCounters,
    pub mix: MixCounters,
    /// Post-gain RMS level of the last tick, dBFS.
    pub level_db: f32,
}

impl StreamShared {
    pub(crate) fn new(jb: JitterBuffer) -> Self {
        Self {
            state: Mutex::new(StreamState {
                jb,
                reset_pending: false,
                in_dtx: false,
                last_packet: Instant::now(),
                net: NetCounters::default(),
                mix: MixCounters::default(),
                level_db: SILENCE_DB,
            }),
        }
    }
}

impl StreamState {
    /// Handles one authenticated datagram (receive task).
    pub(crate) fn on_datagram(
        &mut self,
        header: &MediaHeader,
        payload: Vec<u8>,
        arrival_us: u64,
        now: Instant,
    ) {
        self.last_packet = now;
        self.net.datagrams += 1;
        if header.has_flag(FLAG_DTX) {
            self.in_dtx = true;
            self.net.keepalives += 1;
            return;
        }
        // A reset packet, or the first audio after keep-alives once the buffer drained (in
        // case the reset packet itself was lost or is late): restart playout at this packet
        // instead of counting the keep-alives' sequence numbers as lost. `seq` only grows, so
        // a reset packet at or below a seq already pushed was reordered behind later packets
        // of the same restart: it must not throw those away (they were accepted by the
        // replay window and would never come back); it only joins them if playout has not
        // started yet.
        let reset = header.has_flag(FLAG_RESET);
        let reordered_reset = reset
            && self
                .jb
                .highest_seq()
                .is_some_and(|h| (header.seq.wrapping_sub(h) as i32) <= 0);
        if reordered_reset {
            self.jb.lower_floor(header.seq);
        } else if reset || (self.in_dtx && self.jb.buffered_ms() <= 0.0) {
            self.jb.reset_at(header.seq);
            self.reset_pending = true;
            self.net.resets += 1;
        }
        self.in_dtx = false;
        self.jb
            .push(header.seq, header.timestamp, arrival_us, payload);
    }
}

/// Commands to the mixer thread.
pub(crate) enum MixerCommand {
    /// Start mixing a stream.
    Add(Box<MixStream>),
    /// Stop mixing a stream.
    Remove(u32),
    /// Change a stream's controls.
    Controls(u32, Controls),
    /// Master gain.
    Master(f32),
}

/// Mixer-side state of one stream (built on the connection task, moved into the thread).
pub(crate) struct MixStream {
    id: u32,
    shared: Arc<StreamShared>,
    frame_frames: usize,
    decoder: OpusDecoder,
    /// Joins the audio around latency cuts (`Pop::Skipped`).
    splicer: Splicer,
    resampler: StreamResampler,
    drift: DriftController,
    pcm: Vec<f32>,
    fifo: Vec<f32>,
    /// Mean square of recent decoded frames (exponential average), for the quiet hint.
    recent_ms: f32,
    /// The last decoded frame was quiet (hint for the jitter buffer's latency cuts).
    quiet: bool,
    meter: LevelMeter,
    controls: Controls,
    counters: MixCounters,
    has_input: bool,
    playing: bool,
}

impl MixStream {
    /// Builds the decoder, resampler and buffers of a 48 kHz stereo stream with `frame_ms`
    /// frames.
    pub(crate) fn new(
        id: u32,
        shared: Arc<StreamShared>,
        frame_ms: u32,
        controls: Controls,
    ) -> Result<Self, CoreError> {
        let format = AudioFormat::INTERNAL;
        let frame_frames = format.frames_for_ms(frame_ms);
        Ok(Self {
            id,
            shared,
            frame_frames,
            decoder: OpusDecoder::new(format.sample_rate, format.channels)?,
            splicer: Splicer::new(format.sample_rate, format.channels),
            resampler: StreamResampler::new(
                format.channels,
                format.sample_rate,
                format.sample_rate,
                frame_frames,
            )?,
            drift: DriftController::new(DriftConfig::default()),
            pcm: vec![0.0; MAX_PACKET_FRAMES * 2],
            fifo: Vec::with_capacity(MIX_SAMPLES * 4 + MAX_PACKET_FRAMES * 3),
            recent_ms: 0.0,
            quiet: false,
            meter: LevelMeter::new(),
            controls,
            counters: MixCounters::default(),
            has_input: false,
            playing: false,
        })
    }

    /// PLC for one frame into `pcm`; returns frames.
    fn conceal(&mut self) -> usize {
        let n = self.frame_frames * 2;
        match self.decoder.conceal(&mut self.pcm[..n]) {
            Ok(frames) if frames > 0 => frames,
            _ => {
                self.pcm[..n].fill(0.0);
                self.frame_frames
            }
        }
    }

    fn decode(&mut self, packet: &[u8]) -> usize {
        if let Some(p) = payload::parse(packet) {
            if let Ok(frames) = self.decoder.decode(p.primary, &mut self.pcm) {
                if frames > 0 {
                    self.counters.played += 1;
                    return frames;
                }
            }
        }
        self.counters.concealed += 1;
        self.conceal()
    }

    /// A frame discarded to cut latency: decoded all the same (so the decoder state stays
    /// continuous), PLC if it cannot be; not counted as played or concealed.
    fn decode_discarded(&mut self, packet: Option<&[u8]>) -> usize {
        if let Some(p) = packet.and_then(payload::parse) {
            if let Ok(frames) = self.decoder.decode(p.primary, &mut self.pcm) {
                if frames > 0 {
                    return frames;
                }
            }
        }
        self.conceal()
    }

    /// Updates the quiet hint from a decoded frame (see [`QUIET_FLOOR_DBFS`]).
    fn note_level(&mut self, pcm_len: usize) {
        let pcm = &self.pcm[..pcm_len];
        let ms = pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len().max(1) as f32;
        let floor = 10f32.powf(QUIET_FLOOR_DBFS / 10.0);
        let below = 10f32.powf(-QUIET_BELOW_AVERAGE_DB / 10.0);
        self.quiet = ms < floor || ms < self.recent_ms * below;
        if ms.is_finite() {
            self.recent_ms += (ms - self.recent_ms) / QUIET_AVERAGE_FRAMES;
        }
    }

    /// A lost frame: redundancy in the next packet, else in-band FEC, else PLC.
    fn recover(&mut self, next: Option<&[u8]>) -> usize {
        if let Some(p) = next.and_then(payload::parse) {
            if let Some(copy) = p.redundant {
                if let Ok(frames) = self.decoder.decode(copy, &mut self.pcm) {
                    if frames > 0 {
                        self.counters.recovered_redundancy += 1;
                        return frames;
                    }
                }
            }
            if packet_has_fec(p.primary) {
                let n = self.frame_frames * 2;
                if let Ok(frames) = self.decoder.decode_fec(p.primary, &mut self.pcm[..n]) {
                    self.counters.recovered_fec += 1;
                    return frames;
                }
            }
        }
        self.counters.concealed += 1;
        self.conceal()
    }

    /// Fills the FIFO with at least one tick (unless the buffer underruns) and updates the
    /// drift correction and the level.
    fn fill(&mut self) {
        while self.fifo.len() < MIX_SAMPLES {
            let (pop, reset, in_dtx) = {
                let mut st = self.shared.state.lock();
                let reset = std::mem::take(&mut st.reset_pending);
                st.jb.set_quiet(self.quiet);
                (st.jb.pop(), reset, st.in_dtx)
            };
            if reset {
                self.decoder.reset();
                self.splicer.reset();
                self.resampler.reset();
                self.fifo.clear();
                self.quiet = false;
            }
            let frames = match pop {
                Pop::Packet(p) => self.decode(&p),
                Pop::Missing { next } => self.recover(next.as_deref()),
                Pop::Stretch => self.conceal(),
                Pop::Skipped(p) => {
                    // Cut out to reduce latency: decoded, not played; the next frame is
                    // spliced onto what was played before.
                    let frames = self.decode_discarded(p.as_deref());
                    let n = (frames * 2).min(self.pcm.len());
                    self.splicer.discard(&self.pcm[..n]);
                    continue;
                }
                Pop::Underrun => {
                    if self.playing && !in_dtx {
                        self.counters.underruns += 1;
                    }
                    // Nothing to splice onto: the mixer fades the stream out and back in.
                    self.splicer.reset();
                    break;
                }
            };
            let n = (frames * 2).min(self.pcm.len());
            self.note_level(n);
            let (head, rest) = self.splicer.splice(&self.pcm[..n]);
            // Cannot fail for valid 48 kHz stereo input; a failure only loses this frame.
            let _ = self.resampler.process(head, &mut self.fifo);
            let _ = self.resampler.process(rest, &mut self.fifo);
        }
        self.has_input = self.fifo.len() >= MIX_SAMPLES;
        self.playing = self.has_input;
        let level_db = if self.has_input && !self.controls.muted && self.controls.gain > 0.0 {
            let level = self.meter.process(&self.fifo[..MIX_SAMPLES]);
            if level.rms_db <= SILENCE_DB {
                SILENCE_DB
            } else {
                (level.rms_db + 20.0 * self.controls.gain.log10()).max(SILENCE_DB)
            }
        } else {
            SILENCE_DB
        };
        let mut st = self.shared.state.lock();
        if st.jb.is_primed() {
            let ratio = self.drift.update(
                st.jb.buffered_ms(),
                st.jb.target_ms(),
                f64::from(MIX_FRAME_MS) / 1000.0,
            );
            // The controller clamps to ±2000 ppm, far inside the resampler's range.
            let _ = self.resampler.set_ratio_relative(ratio);
        }
        st.level_db = level_db;
        st.mix = self.counters;
    }
}

/// The output device plus conversion from the 48 kHz stereo mix to its format.
struct OutputStage {
    output: Box<dyn AudioOutput>,
    /// `None` while the output is down.
    sink: Option<PcmSink>,
    /// Underruns (samples) of the ring the output currently plays from.
    underruns: RingStats,
    /// Underrun count at the last refresh (`None` right after a (re)start: start-up
    /// underruns before the first tick do not count).
    seen_underruns: Option<u64>,
    /// Extra fill target after output underruns (ms; grows fast, decays slowly).
    extra_ms: f32,
    /// Refreshes in a row without an underrun (see [`EXTRA_FILL_DECAY_AFTER`]).
    quiet_refreshes: u32,
    format: AudioFormat,
    mapped: Vec<f32>,
    resampler: Option<StreamResampler>,
    converted: Vec<f32>,
    target_frames: usize,
    retry_at: Option<Instant>,
    reported_retry_failure: bool,
    /// Latency after the mixer (ring fill target + the output's latency), ms as `f32` bits.
    latency_ms: Arc<AtomicU32>,
}

/// Creates the output ring for `format` ([`OUTPUT_RING_MS`]).
pub(crate) fn output_ring(format: AudioFormat) -> (PcmSink, hfa_capture::PcmSource) {
    let samples =
        format.sample_rate as usize * usize::from(format.channels.max(1)) * OUTPUT_RING_MS / 1000;
    hfa_capture::pcm_ring_with_channels(samples, format.channels.max(1))
}

impl OutputStage {
    fn new(
        output: Box<dyn AudioOutput>,
        sink: PcmSink,
        underruns: RingStats,
        latency_ms: Arc<AtomicU32>,
    ) -> Self {
        let mut stage = Self {
            format: output.format(),
            output,
            sink: Some(sink),
            underruns,
            seen_underruns: None,
            extra_ms: 0.0,
            quiet_refreshes: 0,
            mapped: Vec::new(),
            resampler: None,
            converted: Vec::new(),
            target_frames: 0,
            retry_at: None,
            reported_retry_failure: false,
            latency_ms,
        };
        stage.configure();
        stage
    }

    /// (Re)builds the converter and the fill target for the output's current format. The
    /// extra fill learnt from the previous device's underruns is dropped (a reopened output
    /// may be another device with another period).
    fn configure(&mut self) {
        self.seen_underruns = None;
        self.extra_ms = 0.0;
        self.quiet_refreshes = 0;
        self.format = self.output.format();
        let rate = self.format.sample_rate.max(1);
        let ch = usize::from(self.format.channels.max(1));
        self.mapped = Vec::with_capacity(MIX_FRAMES * ch);
        self.resampler = if rate == AudioFormat::INTERNAL.sample_rate {
            None
        } else {
            StreamResampler::new(
                self.format.channels.max(1),
                AudioFormat::INTERNAL.sample_rate,
                rate,
                MIX_FRAMES,
            )
            .ok()
        };
        let out_frames_per_tick = MIX_FRAMES * rate as usize / 48_000 + 64;
        self.converted = Vec::with_capacity(out_frames_per_tick * ch * 2);
        self.update_target();
    }

    /// Re-reads the output's latency (atomic loads only; the backend measures it once it
    /// runs) and recomputes the fill target and the published latency. No allocation.
    fn update_target(&mut self) {
        let rate = self.format.sample_rate.max(1);
        let ch = usize::from(self.format.channels.max(1));
        let latency = self
            .output
            .latency_ms()
            .filter(|l| l.is_finite() && *l >= 0.0)
            .unwrap_or(DEFAULT_OUTPUT_LATENCY_MS);
        let target_ms =
            2.0 * MIX_FRAME_MS as f32 + latency.min(MAX_PACING_LATENCY_MS) + self.extra_ms;
        let out_frames_per_tick = MIX_FRAMES * rate as usize / 48_000 + 64;
        let capacity_frames = self.sink.as_ref().map_or(0, |s| s.capacity() / ch);
        self.target_frames = ((rate as f32 * target_ms / 1000.0) as usize)
            .min(capacity_frames.saturating_sub(out_frames_per_tick));
        let ring_ms = self.target_frames as f32 * 1000.0 / rate as f32;
        self.latency_ms
            .store((ring_ms + latency).to_bits(), Ordering::Relaxed);
    }

    /// Periodic refresh (every [`OUTPUT_REFRESH`]): an output that ran dry since the last
    /// refresh (its period is larger than the fill target, or the thread was descheduled)
    /// gets [`UNDERRUN_STEP_MS`] more fill, up to [`MAX_EXTRA_FILL_MS`]; after
    /// [`EXTRA_FILL_DECAY_AFTER`] refreshes in a row without one it gets
    /// [`EXTRA_FILL_DECAY_MS`] less (a one-off hiccup does not add latency for good); then
    /// the target follows the output's current latency.
    fn refresh(&mut self) {
        if self.sink.is_none() {
            return;
        }
        let count = self.underruns.count();
        if self.seen_underruns.is_some_and(|seen| count > seen) {
            self.extra_ms = (self.extra_ms + UNDERRUN_STEP_MS).min(MAX_EXTRA_FILL_MS);
            self.quiet_refreshes = 0;
        } else if self.seen_underruns.is_some() {
            self.quiet_refreshes += 1;
            if self.quiet_refreshes >= EXTRA_FILL_DECAY_AFTER {
                self.quiet_refreshes = 0;
                self.extra_ms = (self.extra_ms - EXTRA_FILL_DECAY_MS).max(0.0);
            }
        }
        self.seen_underruns = Some(count);
        self.update_target();
    }

    fn is_up(&self) -> bool {
        self.sink.is_some()
    }

    fn needs_audio(&self) -> bool {
        let ch = usize::from(self.format.channels.max(1));
        self.sink
            .as_ref()
            .is_some_and(|s| (s.capacity() - s.free()) / ch < self.target_frames)
    }

    /// Converts the 48 kHz stereo `mix` to the output format and queues it.
    fn write(&mut self, mix: &[f32]) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        let ch = usize::from(self.format.channels.max(1));
        if ch == 2 && self.resampler.is_none() {
            sink.push(mix);
            return;
        }
        self.mapped.clear();
        for frame in mix.chunks_exact(2) {
            let (l, r) = (frame[0], frame[1]);
            if ch == 1 {
                self.mapped.push(0.5 * (l + r));
            } else {
                self.mapped.push(l);
                self.mapped.push(r);
                for _ in 2..ch {
                    self.mapped.push(0.0);
                }
            }
        }
        match self.resampler.as_mut() {
            None => {
                sink.push(&self.mapped);
            }
            Some(rs) => {
                self.converted.clear();
                if rs.process(&self.mapped, &mut self.converted).is_ok() {
                    sink.push(&self.converted);
                }
            }
        }
    }

    /// Detects a failed output and restarts it (see the module docs).
    fn check_health(&mut self, now: Instant, events: &broadcast::Sender<HubEvent>) {
        if self.sink.is_some() && self.output.has_error() {
            self.output.stop();
            self.sink = None;
            self.reported_retry_failure = false;
            let message = if self.output.restartable() {
                self.retry_at = Some(now);
                "the audio output failed; reopening it"
            } else {
                // Restarting would destroy what it already wrote (a WAV file is truncated).
                self.retry_at = None;
                "the audio output failed and is not reopened (that would overwrite what it \
                 wrote)"
            };
            let _ = events.send(HubEvent::Error(message.into()));
        }
        if self.sink.is_none() && self.retry_at.is_some_and(|t| now >= t) {
            let (sink, source) = output_ring(self.output.format());
            let underruns = source.stats();
            match self.output.start(source) {
                Ok(()) => {
                    self.sink = Some(sink);
                    self.underruns = underruns;
                    self.retry_at = None;
                    self.configure();
                    tracing::info!(format = ?self.format, "audio output reopened");
                }
                Err(e) => {
                    self.retry_at = Some(now + OUTPUT_RETRY);
                    if !self.reported_retry_failure {
                        self.reported_retry_failure = true;
                        let _ = events.send(HubEvent::Error(format!(
                            "cannot reopen the audio output: {e}"
                        )));
                    }
                }
            }
        }
    }
}

/// The mixer thread's state.
struct MixerThread {
    streams: Vec<MixStream>,
    mixer: Mixer,
    mix: Vec<f32>,
    output: OutputStage,
    commands: Receiver<MixerCommand>,
    events: broadcast::Sender<HubEvent>,
    stop: Arc<AtomicBool>,
}

/// Starts the mixer thread. `output` must already be started on the ring whose producer end
/// is `sink`; the thread stops it when it exits.
pub(crate) fn spawn(
    output: Box<dyn AudioOutput>,
    sink: PcmSink,
    underruns: RingStats,
    commands: Receiver<MixerCommand>,
    events: broadcast::Sender<HubEvent>,
    stop: Arc<AtomicBool>,
    latency_ms: Arc<AtomicU32>,
) -> Result<JoinHandle<()>, CoreError> {
    let mut worker = MixerThread {
        streams: Vec::with_capacity(MAX_STREAMS),
        mixer: Mixer::new(MixerConfig::default(), MIX_FRAMES),
        mix: vec![0.0; MIX_SAMPLES],
        output: OutputStage::new(output, sink, underruns, latency_ms),
        commands,
        events,
        stop,
    };
    thread::Builder::new()
        .name("hfa-mixer".into())
        .spawn(move || {
            let _rt = hfa_capture::rt::promote_current_thread(Duration::from_millis(u64::from(
                MIX_FRAME_MS,
            )));
            worker.run();
        })
        .map_err(|e| CoreError::Io(format!("cannot start the mixer thread: {e}")))
}

impl MixerThread {
    fn run(&mut self) {
        let tick = Duration::from_millis(u64::from(MIX_FRAME_MS));
        let nap = tick / 4;
        let mut wall_next = Instant::now();
        let mut next_refresh = wall_next + OUTPUT_REFRESH;
        // Last time the output took audio (a tick was produced for it).
        let mut last_pull = wall_next;
        let mut stalled = false;
        while !self.stop.load(Ordering::Acquire) {
            self.handle_commands();
            let now = Instant::now();
            self.output.check_health(now, &self.events);
            if now >= next_refresh {
                self.output.refresh();
                next_refresh = now + OUTPUT_REFRESH;
            }
            let mut discard = !self.output.is_up();
            if self.output.is_up() {
                let mut ticks = 0;
                while self.output.needs_audio() && ticks < MAX_TICKS_PER_WAKE {
                    self.tick();
                    let (mix, output) = (&self.mix, &mut self.output);
                    output.write(mix);
                    ticks += 1;
                }
                if ticks > 0 {
                    last_pull = now;
                    if stalled {
                        stalled = false;
                        tracing::info!("the audio output takes audio again");
                    }
                }
                if now.saturating_duration_since(last_pull) < OUTPUT_STALL {
                    wall_next = now + tick;
                } else {
                    if !stalled {
                        stalled = true;
                        tracing::warn!(
                            "the audio output has not taken audio for {} ms; consuming the \
                             streams without it",
                            OUTPUT_STALL.as_millis()
                        );
                    }
                    discard = true;
                }
            }
            if discard && now >= wall_next {
                // No (or a stalled) output: keep the streams flowing at wall-clock pace,
                // discard the mix.
                self.tick();
                wall_next += tick;
                if wall_next + tick * 10 < now {
                    wall_next = now + tick;
                }
                continue;
            }
            thread::sleep(nap);
        }
        self.output.output.stop();
    }

    fn handle_commands(&mut self) {
        loop {
            match self.commands.try_recv() {
                Ok(MixerCommand::Add(stream)) => {
                    let id: SourceId = stream.id;
                    self.streams.retain(|s| s.id != id);
                    self.mixer.add_source(id);
                    self.mixer.set_gain(id, stream.controls.gain);
                    self.mixer.set_muted(id, stream.controls.muted);
                    self.mixer.set_priority(id, stream.controls.priority);
                    self.streams.push(*stream);
                }
                Ok(MixerCommand::Remove(id)) => {
                    self.streams.retain(|s| s.id != id);
                    self.mixer.remove_source(id);
                }
                Ok(MixerCommand::Controls(id, c)) => {
                    if let Some(s) = self.streams.iter_mut().find(|s| s.id == id) {
                        s.controls = c;
                        self.mixer.set_gain(id, c.gain);
                        self.mixer.set_muted(id, c.muted);
                        self.mixer.set_priority(id, c.priority);
                    }
                }
                Ok(MixerCommand::Master(gain)) => self.mixer.set_master_gain(gain),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    // The hub is gone.
                    self.stop.store(true, Ordering::Release);
                    return;
                }
            }
        }
    }

    /// Produces one tick of mix into `self.mix`.
    fn tick(&mut self) {
        for s in self.streams.iter_mut() {
            s.fill();
        }
        let mut inputs: [(SourceId, &[f32]); MAX_STREAMS] = [(0, &[]); MAX_STREAMS];
        let mut n = 0;
        for s in self.streams.iter().filter(|s| s.has_input) {
            if n < MAX_STREAMS {
                inputs[n] = (s.id, &s.fifo[..MIX_SAMPLES]);
                n += 1;
            }
        }
        self.mixer.mix(&inputs[..n], &mut self.mix);
        for s in self.streams.iter_mut().filter(|s| s.has_input) {
            s.fifo.drain(..MIX_SAMPLES);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hfa_audio::{JitterConfig, OpusConfig, OpusEncoder, SineGenerator};

    fn stream(frame_ms: u32) -> (Arc<StreamShared>, Box<MixStream>) {
        let jb = JitterBuffer::new(JitterConfig {
            frame_ms,
            min_target_ms: 20,
            max_target_ms: 100,
            initial_target_ms: 20,
            capacity: 64,
        });
        let shared = Arc::new(StreamShared::new(jb));
        let mix = MixStream::new(7, Arc::clone(&shared), frame_ms, Controls::default())
            .expect("mix stream");
        (shared, Box::new(mix))
    }

    fn header(flags: u8, seq: u32) -> MediaHeader {
        MediaHeader {
            flags,
            stream_id: 7,
            seq,
            timestamp: seq * 480,
        }
    }

    /// Encoded 440 Hz frames wrapped in payload containers (`with_redundancy`: entry 1 =
    /// previous frame).
    fn packets(count: usize, with_redundancy: bool) -> Vec<Vec<u8>> {
        let mut enc = OpusEncoder::new(OpusConfig::default()).expect("encoder");
        let mut tone = SineGenerator::new(440.0, 0.25, AudioFormat::INTERNAL);
        let mut pcm = vec![0.0; 960];
        let mut prev: Option<Vec<u8>> = None;
        let mut out = Vec::new();
        for _ in 0..count {
            tone.fill(&mut pcm);
            let mut buf = vec![0u8; 1275];
            let n = enc.encode(&pcm, &mut buf).expect("encode");
            buf.truncate(n);
            let mut payload = Vec::new();
            let red = if with_redundancy {
                prev.as_deref()
            } else {
                None
            };
            assert!(payload::write(&buf, red, &mut payload));
            out.push(payload);
            prev = Some(buf);
        }
        out
    }

    #[test]
    fn redundancy_recovers_a_single_loss_and_plc_covers_the_rest() {
        let (shared, mut mix) = stream(10);
        let pkts = packets(30, true);
        let t0 = Instant::now();
        // Packets arrive in real time (one per tick, two ticks ahead of playout).
        for (seq, p) in pkts.iter().enumerate() {
            if seq != 10 {
                // (10 is lost.)
                let seq = seq as u32;
                shared.state.lock().on_datagram(
                    &header(0, seq),
                    p.clone(),
                    u64::from(seq) * 10_000,
                    t0,
                );
            }
            if seq >= 2 {
                mix.fill();
                if mix.has_input {
                    mix.fifo.drain(..MIX_SAMPLES);
                }
            }
        }
        let c = mix.counters;
        assert_eq!(c.recovered_redundancy, 1, "{c:?}");
        assert_eq!(c.concealed, 0, "{c:?}");
        assert!(c.played >= 20, "{c:?}");
        let st = shared.state.lock();
        assert_eq!(st.jb.stats().lost, 1);
        assert!(st.level_db > -20.0, "level {}", st.level_db);
    }

    #[test]
    fn without_redundancy_a_loss_is_concealed() {
        let (shared, mut mix) = stream(10);
        let pkts = packets(20, false);
        let t0 = Instant::now();
        for (seq, p) in pkts.iter().enumerate() {
            if seq == 5 {
                continue;
            }
            shared.state.lock().on_datagram(
                &header(0, seq as u32),
                p.clone(),
                seq as u64 * 10_000,
                t0,
            );
        }
        for _ in 0..15 {
            mix.fill();
            if mix.has_input {
                mix.fifo.drain(..MIX_SAMPLES);
            }
        }
        // Frame 5 lies in the encoder's start-up, where libopus still emits LBRR, so it may
        // come back through in-band FEC; otherwise it is concealed.
        let c = mix.counters;
        assert_eq!(c.recovered_redundancy, 0, "{c:?}");
        assert_eq!(c.recovered_fec + c.concealed, 1, "{c:?}");
    }

    #[test]
    fn keepalives_then_audio_reset_the_stream_without_counting_loss() {
        let (shared, mut mix) = stream(10);
        let pkts = packets(12, false);
        let t0 = Instant::now();
        let mut seq = 0u32;
        for p in &pkts[..4] {
            shared.state.lock().on_datagram(
                &header(0, seq),
                p.clone(),
                u64::from(seq) * 10_000,
                t0,
            );
            seq += 1;
        }
        for _ in 0..8 {
            mix.fill();
            if mix.has_input {
                mix.fifo.drain(..MIX_SAMPLES);
            }
        }
        // 20 keep-alives (2 s of DTX), then audio resumes without FLAG_RESET (as if the
        // reset packet had been lost).
        for _ in 0..20 {
            shared
                .state
                .lock()
                .on_datagram(&header(FLAG_DTX, seq), Vec::new(), 0, t0);
            seq += 1;
        }
        assert!(shared.state.lock().in_dtx);
        for p in &pkts[4..] {
            shared.state.lock().on_datagram(
                &header(0, seq),
                p.clone(),
                2_000_000 + u64::from(seq) * 10_000,
                t0,
            );
            seq += 1;
        }
        let st = shared.state.lock();
        assert_eq!(st.net.keepalives, 20);
        assert_eq!(st.net.resets, 1);
        assert!(st.reset_pending);
        assert_eq!(st.jb.stats().lost, 0, "{:?}", st.jb.stats());
        drop(st);
        mix.fill();
        assert!(!shared.state.lock().reset_pending);
    }

    /// Reviewer scenario: the `FLAG_RESET` packet R arrives after R+1 and R+2 when audio
    /// resumes after DTX. R+1 already restarted playout (first audio after keep-alives); the
    /// late R must not reset again and discard R+1/R+2 (the replay window never lets them
    /// back in, so they would play as PLC and count as network loss).
    #[test]
    fn a_reset_packet_reordered_behind_later_packets_keeps_them() {
        let (shared, mut mix) = stream(10);
        let pkts = packets(12, false);
        let t0 = Instant::now();
        for seq in 0..4u32 {
            shared.state.lock().on_datagram(
                &header(0, seq),
                pkts[seq as usize].clone(),
                u64::from(seq) * 10_000,
                t0,
            );
        }
        for _ in 0..8 {
            mix.fill();
            if mix.has_input {
                mix.fifo.drain(..MIX_SAMPLES);
            }
        }
        // Keep-alives 4..=9, then the resumed audio 10.. with R = 10 arriving third.
        for seq in 4..10u32 {
            shared
                .state
                .lock()
                .on_datagram(&header(FLAG_DTX, seq), Vec::new(), 0, t0);
        }
        let arrival = |seq: u32| 1_000_000 + u64::from(seq) * 10_000;
        for (seq, flags) in [(11, 0), (12, 0), (10, FLAG_RESET), (13, 0), (14, 0)] {
            let p = pkts[(seq - 6) as usize].clone();
            shared
                .state
                .lock()
                .on_datagram(&header(flags, seq), p, arrival(seq), t0);
        }
        {
            let st = shared.state.lock();
            assert_eq!(st.net.resets, 1, "one restart, not two");
            assert_eq!(st.jb.buffered_ms(), 50.0, "10..=14 all buffered");
        }
        for _ in 0..5 {
            mix.fill();
            if mix.has_input {
                mix.fifo.drain(..MIX_SAMPLES);
            }
        }
        let st = shared.state.lock();
        assert_eq!(st.jb.stats().lost, 0, "{:?}", st.jb.stats());
        assert_eq!(st.jb.stats().late, 0, "{:?}", st.jb.stats());
        assert_eq!(st.mix.concealed, 0, "{:?}", st.mix);
    }

    /// Start times (s) of the abnormal windows of a steady `freq` tone in `x` (48 kHz mono):
    /// the detector of `hfa selftest` (`hfa-cli/src/analysis.rs::glitches`) for one tone:
    /// Hann-windowed 20 ms windows every 10 ms where the tone's amplitude leaves 0.5..1.5 ×
    /// its median, or the energy that is not the tone exceeds 2 % (−17 dB) of the tone's.
    fn abnormal_windows(x: &[f32], freq: f64) -> Vec<f64> {
        let (len, hop) = (960, 480);
        let w: Vec<f64> = (0..len)
            .map(|n| 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / (len - 1) as f64).cos())
            .collect();
        let (sum, sum_sq): (f64, f64) = (w.iter().sum(), w.iter().map(|v| v * v).sum());
        let coeff = 2.0 * (std::f64::consts::TAU * freq / 48_000.0).cos();
        let windows: Vec<(f64, f64)> = (0..)
            .map(|k| k * hop)
            .take_while(|s| s + len <= x.len())
            .map(|s| {
                let block = &x[s..s + len];
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for (v, w) in block.iter().zip(&w) {
                    let s0 = f64::from(*v) * w + coeff * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let amp = 2.0 * (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / sum;
                let ms = block
                    .iter()
                    .zip(&w)
                    .map(|(v, w)| (f64::from(*v) * w).powi(2))
                    .sum::<f64>()
                    / sum_sq;
                (amp, ms)
            })
            .collect();
        let mut amps: Vec<f64> = windows.iter().map(|w| w.0).collect();
        amps.sort_by(f64::total_cmp);
        let median = amps[amps.len() / 2];
        windows
            .iter()
            .enumerate()
            .filter(|(_, (amp, ms))| {
                *amp < 0.5 * median
                    || *amp > 1.5 * median
                    || ms - amp * amp / 2.0 > 0.02 * median * median / 2.0
            })
            .map(|(k, _)| k as f64 * 0.010)
            .collect()
    }

    /// The failure of the macOS CI run: a burst after a stall left far more audio buffered
    /// than the target, and every latency cut (one frame every 100 ms) was a glitch of the
    /// tone. Through the real per-stream path: a burst of 15 packets (single-frame cuts) and
    /// one of 45 (a multi-frame cut at once). The selftest's detector finds nothing, and the
    /// decoder saw every packet, discarded ones included.
    #[test]
    fn latency_cuts_are_inaudible_and_keep_the_decoder_continuous() {
        let (shared, mut mix) = stream(10);
        {
            // The hub's default jitter settings (a 20 ms minimum target).
            let mut st = shared.state.lock();
            st.jb = JitterBuffer::new(JitterConfig {
                frame_ms: 10,
                min_target_ms: 20,
                max_target_ms: 150,
                initial_target_ms: 40,
                capacity: 64,
            });
        }
        let pkts = packets(500, false);
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut next = 0usize;
        for tick in 0u64.. {
            let arriving = match tick {
                100 => 15,
                250 => 45,
                _ => 1,
            };
            if next + arriving + 1 > pkts.len() {
                break;
            }
            for _ in 0..arriving {
                shared.state.lock().on_datagram(
                    &header(0, next as u32),
                    pkts[next].clone(),
                    tick * 10_000,
                    t0,
                );
                next += 1;
            }
            mix.fill();
            if mix.has_input {
                out.extend(mix.fifo[..MIX_SAMPLES].iter().step_by(2));
                mix.fifo.drain(..MIX_SAMPLES);
            } else {
                out.extend(std::iter::repeat_n(0.0, MIX_FRAMES));
            }
        }
        let (stats, counters) = {
            let st = shared.state.lock();
            (st.jb.stats(), st.mix)
        };
        assert!(stats.skipped > 40, "{stats:?}");
        assert_eq!(
            stats.lost + counters.concealed + stats.stretched,
            0,
            "{stats:?} {counters:?}"
        );
        // Single-frame cuts after the first burst, one long cut after the second.
        assert!(mix.splicer.splices() >= 5, "{}", mix.splicer.splices());
        let bad = abnormal_windows(&out[24_000..], 440.0);
        assert!(bad.is_empty(), "abnormal windows at {bad:?} s after 0.5 s");

        // Opus state continuity: the stream's decoder decoded packets 0..n in order (played
        // or discarded), so it decodes packet n exactly like a decoder that saw them all.
        let n = (counters.played + stats.skipped) as usize;
        let mut reference = OpusDecoder::new(48_000, 2).expect("decoder");
        let mut expected = vec![0.0; MAX_PACKET_FRAMES * 2];
        for p in &pkts[..=n] {
            let primary = payload::parse(p).expect("container").primary;
            reference.decode(primary, &mut expected).expect("decode");
        }
        let primary = payload::parse(&pkts[n]).expect("container").primary;
        let frames = mix.decoder.decode(primary, &mut mix.pcm).expect("decode");
        assert_eq!(mix.pcm[..frames * 2], expected[..frames * 2]);
    }

    /// An output that records what it is given, for [`OutputStage`] tests.
    struct FakeOutput {
        format: AudioFormat,
        source: Arc<Mutex<Option<hfa_capture::PcmSource>>>,
        starts: Arc<AtomicU32>,
        error: Arc<AtomicBool>,
        fail_start: Arc<AtomicBool>,
        latency_ms: Arc<AtomicU32>,
        restartable: bool,
    }

    impl AudioOutput for FakeOutput {
        fn format(&self) -> AudioFormat {
            self.format
        }
        fn start(&mut self, source: hfa_capture::PcmSource) -> hfa_capture::Result<()> {
            if self.fail_start.load(Ordering::Relaxed) {
                return Err(hfa_capture::CaptureError::Backend("unplugged".into()));
            }
            *self.source.lock() = Some(source);
            self.starts.fetch_add(1, Ordering::Relaxed);
            self.error.store(false, Ordering::Relaxed);
            Ok(())
        }
        fn stop(&mut self) {
            *self.source.lock() = None;
        }
        fn latency_ms(&self) -> Option<f32> {
            Some(f32::from_bits(self.latency_ms.load(Ordering::Relaxed)))
        }
        fn has_error(&self) -> bool {
            self.error.load(Ordering::Relaxed)
        }
        fn restartable(&self) -> bool {
            self.restartable
        }
    }

    struct Fake {
        source: Arc<Mutex<Option<hfa_capture::PcmSource>>>,
        starts: Arc<AtomicU32>,
        error: Arc<AtomicBool>,
        fail_start: Arc<AtomicBool>,
        /// What the output reports as its latency (ms, `f32` bits).
        latency_ms: Arc<AtomicU32>,
        /// What the stage publishes as the latency after the mixer.
        published: Arc<AtomicU32>,
    }

    fn fake_stage(format: AudioFormat) -> (OutputStage, Fake) {
        fake_stage_with(format, true)
    }

    fn fake_stage_with(format: AudioFormat, restartable: bool) -> (OutputStage, Fake) {
        let fake = Fake {
            source: Arc::new(Mutex::new(None)),
            starts: Arc::new(AtomicU32::new(0)),
            error: Arc::new(AtomicBool::new(false)),
            fail_start: Arc::new(AtomicBool::new(false)),
            latency_ms: Arc::new(AtomicU32::new(5.0f32.to_bits())),
            published: Arc::new(AtomicU32::new(0)),
        };
        let mut output = FakeOutput {
            format,
            source: Arc::clone(&fake.source),
            starts: Arc::clone(&fake.starts),
            error: Arc::clone(&fake.error),
            fail_start: Arc::clone(&fake.fail_start),
            latency_ms: Arc::clone(&fake.latency_ms),
            restartable,
        };
        let (sink, source) = output_ring(format);
        let underruns = source.stats();
        output.start(source).expect("start");
        let stage = OutputStage::new(
            Box::new(output),
            sink,
            underruns,
            Arc::clone(&fake.published),
        );
        (stage, fake)
    }

    fn published_ms(fake: &Fake) -> f32 {
        f32::from_bits(fake.published.load(Ordering::Relaxed))
    }

    fn goertzel(x: &[f32], freq: f64, rate: f64) -> f64 {
        let coeff = 2.0 * (2.0 * std::f64::consts::PI * freq / rate).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s0 = f64::from(v) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        2.0 * (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / x.len() as f64
    }

    #[test]
    fn output_stage_converts_to_the_device_format() {
        let (mut stage, fake) = fake_stage(AudioFormat::new(44_100, 1));
        // Fill target: 2 ticks + 5 ms of device latency = 25 ms at 44.1 kHz.
        assert_eq!(stage.target_frames, 1102);
        assert!(stage.needs_audio());
        let mut tone = SineGenerator::new(441.0, 0.25, AudioFormat::INTERNAL);
        let mut mix = vec![0.0; MIX_SAMPLES];
        let mut out = Vec::new();
        let mut buf = vec![0.0; 44_100];
        for _ in 0..100 {
            tone.fill(&mut mix);
            stage.write(&mix);
            let mut guard = fake.source.lock();
            let src = guard.as_mut().expect("started");
            let n = src.available();
            src.pull(&mut buf[..n]);
            out.extend_from_slice(&buf[..n]);
        }
        // 1 s of 48 kHz mix → about 1 s at 44.1 kHz (minus the resampler's delay).
        assert!((43_500..=44_100).contains(&out.len()), "{}", out.len());
        let tail = &out[out.len() - 4410..];
        let amp = goertzel(tail, 441.0, 44_100.0);
        assert!((amp - 0.25).abs() < 0.02, "amplitude {amp}");
    }

    #[test]
    fn output_stage_maps_stereo_to_more_channels() {
        let (mut stage, fake) = fake_stage(AudioFormat::new(48_000, 4));
        let mix: Vec<f32> = (0..MIX_SAMPLES)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.25 })
            .collect();
        stage.write(&mix);
        let mut guard = fake.source.lock();
        let src = guard.as_mut().expect("started");
        assert_eq!(src.available(), MIX_FRAMES * 4);
        let mut buf = vec![0.0; MIX_FRAMES * 4];
        src.pull(&mut buf);
        assert!(buf.chunks_exact(4).all(|f| f == [0.5, -0.25, 0.0, 0.0]));
    }

    #[test]
    fn a_failed_output_is_restarted() {
        let (mut stage, fake) = fake_stage(AudioFormat::INTERNAL);
        let (events, mut rx) = broadcast::channel(8);
        let t0 = Instant::now();
        stage.check_health(t0, &events);
        assert!(stage.is_up());
        assert_eq!(fake.starts.load(Ordering::Relaxed), 1);

        // The device goes away and cannot be reopened yet.
        fake.error.store(true, Ordering::Relaxed);
        fake.fail_start.store(true, Ordering::Relaxed);
        stage.check_health(t0, &events);
        assert!(!stage.is_up());
        assert!(
            fake.source.lock().is_none(),
            "the failed output was stopped"
        );
        assert!(matches!(rx.try_recv(), Ok(HubEvent::Error(m)) if m.contains("failed")));
        assert!(matches!(rx.try_recv(), Ok(HubEvent::Error(m)) if m.contains("cannot reopen")));
        // Retries are spaced and reported once.
        stage.check_health(t0 + Duration::from_millis(100), &events);
        stage.check_health(t0 + OUTPUT_RETRY + Duration::from_millis(1), &events);
        assert!(rx.try_recv().is_err());
        assert!(!stage.is_up());

        // It comes back.
        fake.fail_start.store(false, Ordering::Relaxed);
        stage.check_health(t0 + OUTPUT_RETRY * 3, &events);
        assert!(stage.is_up());
        assert_eq!(fake.starts.load(Ordering::Relaxed), 2);
        assert!(rx.try_recv().is_err(), "recovery is not an error");
    }

    /// A WAV output is truncated by `start`: after a write error it is stopped (which
    /// finalizes what it wrote) and reported, never restarted.
    #[test]
    fn a_failed_non_restartable_output_is_not_reopened() {
        let (mut stage, fake) = fake_stage_with(AudioFormat::INTERNAL, false);
        let (events, mut rx) = broadcast::channel(8);
        let t0 = Instant::now();
        fake.error.store(true, Ordering::Relaxed);
        stage.check_health(t0, &events);
        assert!(!stage.is_up());
        assert!(fake.source.lock().is_none(), "stopped");
        assert!(matches!(rx.try_recv(), Ok(HubEvent::Error(m)) if m.contains("not reopened")));
        for i in 1..5 {
            stage.check_health(t0 + OUTPUT_RETRY * i, &events);
        }
        assert!(!stage.is_up());
        assert_eq!(
            fake.starts.load(Ordering::Relaxed),
            1,
            "never started again"
        );
        assert!(rx.try_recv().is_err(), "reported once");
    }

    /// The fill target follows the latency the output measures once it runs (only its first
    /// 40 ms are buffered in the ring), grows after the output ran dry, and the published
    /// latency covers the ring and the whole output latency.
    #[test]
    fn the_fill_target_follows_the_output() {
        let (mut stage, fake) = fake_stage(AudioFormat::INTERNAL);
        // 2 ticks + 5 ms.
        assert_eq!(stage.target_frames, 1200);
        assert!((published_ms(&fake) - 30.0).abs() < 0.01);

        // The backend now reports a long playback delay (Bluetooth).
        fake.latency_ms.store(180.0f32.to_bits(), Ordering::Relaxed);
        stage.refresh();
        assert_eq!(stage.target_frames, 48 * 60, "2 ticks + the 40 ms cap");
        assert!((published_ms(&fake) - 240.0).abs() < 0.01);

        // The device pulls more than the ring holds: an underrun.
        {
            let mut guard = fake.source.lock();
            let src = guard.as_mut().expect("started");
            let mut buf = vec![0.0; 960];
            src.pull(&mut buf);
            assert!(src.underruns() > 0);
        }
        stage.refresh();
        assert_eq!(stage.target_frames, 48 * 70, "one step of extra fill");
        stage.refresh();
        assert_eq!(stage.target_frames, 48 * 70, "no new underrun, no change");
        assert!((published_ms(&fake) - 250.0).abs() < 0.01);

        // Extra fill is bounded.
        for _ in 0..40 {
            {
                let mut guard = fake.source.lock();
                let src = guard.as_mut().expect("started");
                let mut buf = vec![0.0; 2];
                src.pull(&mut buf);
            }
            stage.refresh();
        }
        let max_ms = 2.0 * MIX_FRAME_MS as f32 + MAX_PACING_LATENCY_MS + MAX_EXTRA_FILL_MS;
        assert_eq!(stage.target_frames, 48 * max_ms as usize);
    }

    /// The extra fill added after an underrun is given back slowly once the output runs
    /// cleanly, and dropped when the output is reopened.
    #[test]
    fn the_extra_fill_decays_and_is_dropped_on_reopen() {
        let (mut stage, fake) = fake_stage(AudioFormat::INTERNAL);
        let base = stage.target_frames;
        let underrun = |fake: &Fake| {
            let mut guard = fake.source.lock();
            let src = guard.as_mut().expect("started");
            let mut buf = vec![0.0; 48_000];
            src.pull(&mut buf);
        };
        stage.refresh();
        for _ in 0..3 {
            underrun(&fake);
            stage.refresh();
        }
        assert_eq!(stage.target_frames, base + 48 * 30, "3 steps of 10 ms");
        // 29 s without an underrun: nothing yet; at 30 s: 5 ms less.
        for _ in 0..(EXTRA_FILL_DECAY_AFTER - 1) {
            stage.refresh();
        }
        assert_eq!(stage.target_frames, base + 48 * 30);
        stage.refresh();
        assert_eq!(stage.target_frames, base + 48 * 25);
        // An underrun restarts the count (and adds a step).
        underrun(&fake);
        stage.refresh();
        assert_eq!(stage.target_frames, base + 48 * 35);
        // Down to nothing, never below.
        for _ in 0..(EXTRA_FILL_DECAY_AFTER * 10) {
            stage.refresh();
        }
        assert_eq!(stage.target_frames, base);
        // Reopening the output forgets what the previous device needed.
        underrun(&fake);
        stage.refresh();
        assert!(stage.target_frames > base);
        let (events, _rx) = broadcast::channel(8);
        fake.error.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        stage.check_health(t0, &events);
        stage.check_health(t0 + OUTPUT_RETRY, &events);
        assert!(stage.is_up());
        assert_eq!(stage.target_frames, base);
    }

    /// An output that is up but stops taking audio without reporting an error: the streams
    /// are still consumed at wall-clock pace, so their jitter buffers never overflow.
    #[test]
    fn a_stalled_output_does_not_overflow_the_jitter_buffers() {
        let (stage, fake) = fake_stage(AudioFormat::INTERNAL);
        // Hand the stage's parts to a real mixer thread (the fake never pulls).
        let OutputStage {
            output,
            sink,
            underruns,
            latency_ms,
            ..
        } = stage;
        let (tx, rx) = std::sync::mpsc::channel();
        let (events, _) = broadcast::channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn(
            output,
            sink.expect("up"),
            underruns,
            rx,
            events,
            Arc::clone(&stop),
            latency_ms,
        )
        .expect("spawn");
        let (shared, mix) = stream(10);
        tx.send(MixerCommand::Add(mix)).expect("add");
        let pkts = packets(150, false);
        let t0 = Instant::now();
        for (seq, p) in pkts.iter().enumerate() {
            shared.state.lock().on_datagram(
                &header(0, seq as u32),
                p.clone(),
                t0.elapsed().as_micros() as u64,
                Instant::now(),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Release);
        handle.join().expect("join");
        assert!(fake.source.lock().is_none(), "stopped");
        let st = shared.state.lock();
        let jb = st.jb.stats();
        assert_eq!(jb.overflowed, 0, "{jb:?}");
        assert!(st.jb.buffered_ms() < 200.0, "{}", st.jb.buffered_ms());
        assert!(st.mix.played > 80, "{:?}", st.mix);
    }

    #[test]
    fn mixer_ticks_do_not_allocate_after_warm_up() {
        use hfa_capture::output_file::NullOutput;
        let format = AudioFormat::INTERNAL;
        let (sink, source) = output_ring(format);
        let underruns = source.stats();
        let (_tx, rx) = std::sync::mpsc::channel();
        let (events, _) = broadcast::channel(8);
        let mut worker = MixerThread {
            streams: Vec::with_capacity(MAX_STREAMS),
            mixer: Mixer::new(MixerConfig::default(), MIX_FRAMES),
            mix: vec![0.0; MIX_SAMPLES],
            output: OutputStage::new(
                Box::new(NullOutput::new(format, 10)),
                sink,
                underruns,
                Arc::new(AtomicU32::new(0)),
            ),
            commands: rx,
            events,
            stop: Arc::new(AtomicBool::new(false)),
        };
        let mut shared = Vec::new();
        for (id, frame_ms) in [(1u32, 10u32), (2, 20)] {
            let jb = JitterBuffer::new(JitterConfig {
                frame_ms,
                min_target_ms: 20,
                max_target_ms: 100,
                initial_target_ms: 20,
                capacity: 256,
            });
            let s = Arc::new(StreamShared::new(jb));
            let mix =
                MixStream::new(id, Arc::clone(&s), frame_ms, Controls::default()).expect("stream");
            worker.mixer.add_source(id);
            worker.streams.push(mix);
            shared.push((s, frame_ms));
        }
        // 2 s of packets for both streams (a 10 ms and a 20 ms one), encoded up front and
        // delivered in real-time order (40 ms ahead) as the ticks run: pushing moves the
        // payload in and stays within the buffer's preallocated capacity.
        let t0 = Instant::now();
        let mut queues = Vec::new();
        for (_, frame_ms) in &shared {
            let mut enc = OpusEncoder::new(OpusConfig {
                frame_ms: *frame_ms,
                ..OpusConfig::default()
            })
            .expect("encoder");
            let mut tone = SineGenerator::new(440.0, 0.25, AudioFormat::INTERNAL);
            let mut pcm = vec![0.0; AudioFormat::INTERNAL.samples_for_ms(*frame_ms)];
            let mut queue = std::collections::VecDeque::new();
            for seq in 0..(2000 / frame_ms) {
                tone.fill(&mut pcm);
                let mut buf = vec![0u8; 1275];
                let n = enc.encode(&pcm, &mut buf).expect("encode");
                let mut payload = Vec::new();
                assert!(payload::write(&buf[..n], None, &mut payload));
                let h = MediaHeader {
                    flags: 0,
                    stream_id: 0,
                    seq,
                    timestamp: seq * frame_ms * 48,
                };
                queue.push_back((h, payload));
            }
            queues.push(queue);
        }
        let mut deliver = |tick: u32| {
            for ((s, frame_ms), queue) in shared.iter().zip(queues.iter_mut()) {
                while queue
                    .front()
                    .is_some_and(|(h, _)| h.seq * frame_ms <= (tick + 4) * MIX_FRAME_MS)
                {
                    let (h, payload) = queue.pop_front().expect("queued");
                    s.state
                        .lock()
                        .on_datagram(&h, payload, u64::from(h.seq * frame_ms) * 1000, t0);
                }
            }
        };
        for tick in 0..30 {
            deliver(tick);
            worker.tick();
            let (mix, output) = (&worker.mix, &mut worker.output);
            output.write(mix);
        }
        let allocations = crate::test_alloc::count_allocs(|| {
            for tick in 30..130 {
                deliver(tick);
                worker.tick();
                let (mix, output) = (&worker.mix, &mut worker.output);
                output.write(mix);
            }
        });
        assert_eq!(allocations, 0);
        for (s, _) in &shared {
            assert!(s.state.lock().mix.played > 50);
        }
    }

    #[test]
    fn mixer_thread_paces_output_and_mixes_streams() {
        use hfa_capture::output_file::NullOutput;
        let format = AudioFormat::INTERNAL;
        let mut output: Box<dyn AudioOutput> = Box::new(NullOutput::new(format, 10));
        let (sink, source) = output_ring(format);
        let underruns = source.stats();
        output.start(source).expect("start");
        let (tx, rx) = std::sync::mpsc::channel();
        let (events, _) = broadcast::channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let latency = Arc::new(AtomicU32::new(0));
        let spawned = Instant::now();
        let handle = spawn(
            output,
            sink,
            underruns,
            rx,
            events,
            Arc::clone(&stop),
            Arc::clone(&latency),
        )
        .expect("spawn");
        let (shared, mix) = stream(10);
        tx.send(MixerCommand::Add(mix)).expect("add");
        let pkts = packets(60, false);
        let t0 = Instant::now();
        for (seq, p) in pkts.iter().enumerate() {
            shared.state.lock().on_datagram(
                &header(0, seq as u32),
                p.clone(),
                t0.elapsed().as_micros() as u64,
                Instant::now(),
            );
            // One packet per 10 ms against the clock (a late wake-up of this thread delays
            // one packet, not the whole run), like a real sender.
            sleep_until(t0 + Duration::from_millis(10 * (seq as u64 + 1)));
        }
        sleep_until(t0 + Duration::from_millis(700));
        stop.store(true, Ordering::Release);
        handle.join().expect("join");
        let elapsed = spawned.elapsed();
        let st = shared.state.lock();
        assert!(st.mix.played >= 40, "{:?}", st.mix);
        // Ring target (2 ticks + the output's 10 ms) plus the output's 10 ms. (Extra fill
        // could only come from an output underrun between the thread's first two refreshes,
        // 1 s and 2 s after it started; the run is over well before that.)
        let published = f32::from_bits(latency.load(Ordering::Relaxed));
        assert!(elapsed < 2 * OUTPUT_REFRESH, "the run took {elapsed:?}");
        assert!((published - 40.0).abs() < 0.01, "published {published} ms");
    }

    /// Sleeps until `deadline` (no-op if it has passed).
    fn sleep_until(deadline: Instant) {
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }
}
