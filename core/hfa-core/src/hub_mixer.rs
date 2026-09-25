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
//! - `Underrun` → no audio this tick: the stream is left out of the mixer's inputs (the
//!   mixer fades it back in later).
//!
//! The decoded frame goes through the stream's [`StreamResampler`] (48 kHz → 48 kHz with the
//! [`DriftController`]'s relative ratio, updated every tick while the buffer is primed) into
//! the FIFO. A stream reset (`FLAG_RESET`, or the first audio packet after DTX keep-alives)
//! resets the decoder, the resampler and the FIFO before the next frame is decoded; the drift
//! estimate is kept (it describes the two clocks, not the stream).
//!
//! # Pacing and output
//!
//! The thread is paced by the output ring: while the ring holds less than the target
//! (`2 × MIX_FRAME_MS` + the output's latency) it produces one tick, otherwise it sleeps a
//! quarter tick. The mix (48 kHz stereo) is converted to the output format (channel map,
//! then resampling to the device rate if needed) and pushed. [`AudioOutput::has_error`] is
//! polled every loop: a failed output is stopped and restarted every [`OUTPUT_RETRY`]
//! (reported as [`HubEvent::Error`]); meanwhile the mixer keeps consuming the jitter buffers
//! at wall-clock pace and discards the mix.
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
    MixerConfig, OpusDecoder, Pop, SourceId, StreamResampler,
};
use hfa_capture::{AudioOutput, PcmSink};
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
/// Pause between two attempts to restart a failed output.
pub(crate) const OUTPUT_RETRY: Duration = Duration::from_secs(2);
/// Most ticks produced in one go before the thread checks commands and sleeps again.
const MAX_TICKS_PER_WAKE: usize = 50;

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
        // case the reset packet itself was lost): restart playout at this packet instead of
        // counting the keep-alives' sequence numbers as lost.
        if header.has_flag(FLAG_RESET) || (self.in_dtx && self.jb.buffered_ms() <= 0.0) {
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
    resampler: StreamResampler,
    drift: DriftController,
    pcm: Vec<f32>,
    fifo: Vec<f32>,
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
            resampler: StreamResampler::new(
                format.channels,
                format.sample_rate,
                format.sample_rate,
                frame_frames,
            )?,
            drift: DriftController::new(DriftConfig::default()),
            pcm: vec![0.0; MAX_PACKET_FRAMES * 2],
            fifo: Vec::with_capacity(MIX_SAMPLES * 4 + MAX_PACKET_FRAMES * 3),
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
                (st.jb.pop(), reset, st.in_dtx)
            };
            if reset {
                self.decoder.reset();
                self.resampler.reset();
                self.fifo.clear();
            }
            let frames = match pop {
                Pop::Packet(p) => self.decode(&p),
                Pop::Missing { next } => self.recover(next.as_deref()),
                Pop::Stretch => self.conceal(),
                Pop::Underrun => {
                    if self.playing && !in_dtx {
                        self.counters.underruns += 1;
                    }
                    break;
                }
            };
            let n = (frames * 2).min(self.pcm.len());
            // Cannot fail for valid 48 kHz stereo input; a failure only loses this frame.
            let _ = self.resampler.process(&self.pcm[..n], &mut self.fifo);
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
    format: AudioFormat,
    mapped: Vec<f32>,
    resampler: Option<StreamResampler>,
    converted: Vec<f32>,
    target_frames: usize,
    retry_at: Option<Instant>,
    reported_retry_failure: bool,
    latency_ms: Arc<AtomicU32>,
}

/// Creates the output ring for `format` ([`OUTPUT_RING_MS`]).
pub(crate) fn output_ring(format: AudioFormat) -> (PcmSink, hfa_capture::PcmSource) {
    let samples =
        format.sample_rate as usize * usize::from(format.channels.max(1)) * OUTPUT_RING_MS / 1000;
    hfa_capture::pcm_ring_with_channels(samples, format.channels.max(1))
}

impl OutputStage {
    fn new(output: Box<dyn AudioOutput>, sink: PcmSink, latency_ms: Arc<AtomicU32>) -> Self {
        let mut stage = Self {
            format: output.format(),
            output,
            sink: Some(sink),
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

    /// (Re)builds the converter and the fill target for the output's current format.
    fn configure(&mut self) {
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
        let latency = self
            .output
            .latency_ms()
            .filter(|l| l.is_finite() && *l >= 0.0)
            .unwrap_or(DEFAULT_OUTPUT_LATENCY_MS);
        self.latency_ms.store(latency.to_bits(), Ordering::Relaxed);
        let target_ms = 2.0 * MIX_FRAME_MS as f32 + latency;
        let capacity_frames = self.sink.as_ref().map_or(0, |s| s.capacity() / ch);
        self.target_frames = ((rate as f32 * target_ms / 1000.0) as usize)
            .min(capacity_frames.saturating_sub(out_frames_per_tick));
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
            self.retry_at = Some(now);
            self.reported_retry_failure = false;
            let _ = events.send(HubEvent::Error(
                "the audio output failed; reopening it".into(),
            ));
        }
        if self.sink.is_none() && self.retry_at.is_some_and(|t| now >= t) {
            let (sink, source) = output_ring(self.output.format());
            match self.output.start(source) {
                Ok(()) => {
                    self.sink = Some(sink);
                    self.retry_at = None;
                    self.configure();
                    let _ = events.send(HubEvent::Error("the audio output was reopened".into()));
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
    commands: Receiver<MixerCommand>,
    events: broadcast::Sender<HubEvent>,
    stop: Arc<AtomicBool>,
    latency_ms: Arc<AtomicU32>,
) -> Result<JoinHandle<()>, CoreError> {
    let mut worker = MixerThread {
        streams: Vec::with_capacity(MAX_STREAMS),
        mixer: Mixer::new(MixerConfig::default(), MIX_FRAMES),
        mix: vec![0.0; MIX_SAMPLES],
        output: OutputStage::new(output, sink, latency_ms),
        commands,
        events,
        stop,
    };
    thread::Builder::new()
        .name("hfa-mixer".into())
        .spawn(move || worker.run())
        .map_err(|e| CoreError::Io(format!("cannot start the mixer thread: {e}")))
}

impl MixerThread {
    fn run(&mut self) {
        let tick = Duration::from_millis(u64::from(MIX_FRAME_MS));
        let nap = tick / 4;
        let mut wall_next = Instant::now();
        while !self.stop.load(Ordering::Acquire) {
            self.handle_commands();
            let now = Instant::now();
            self.output.check_health(now, &self.events);
            if self.output.is_up() {
                let mut ticks = 0;
                while self.output.needs_audio() && ticks < MAX_TICKS_PER_WAKE {
                    self.tick();
                    let (mix, output) = (&self.mix, &mut self.output);
                    output.write(mix);
                    ticks += 1;
                }
                wall_next = now + tick;
            } else if now >= wall_next {
                // No output: keep the streams flowing at wall-clock pace, discard the mix.
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
        for (seq, p) in pkts.iter().enumerate() {
            if seq == 10 {
                continue; // lost
            }
            let seq = seq as u32;
            shared.state.lock().on_datagram(
                &header(0, seq),
                p.clone(),
                u64::from(seq) * 10_000,
                t0,
            );
        }
        for _ in 0..25 {
            mix.fill();
            if mix.has_input {
                mix.fifo.drain(..MIX_SAMPLES);
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

    #[test]
    fn mixer_thread_paces_output_and_mixes_streams() {
        use hfa_capture::output_file::NullOutput;
        let format = AudioFormat::INTERNAL;
        let mut output: Box<dyn AudioOutput> = Box::new(NullOutput::new(format, 10));
        let (sink, source) = output_ring(format);
        output.start(source).expect("start");
        let (tx, rx) = std::sync::mpsc::channel();
        let (events, _) = broadcast::channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let latency = Arc::new(AtomicU32::new(0));
        let handle = spawn(
            output,
            sink,
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
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::Release);
        handle.join().expect("join");
        let st = shared.state.lock();
        assert!(st.mix.played >= 40, "{:?}", st.mix);
        assert!((f32::from_bits(latency.load(Ordering::Relaxed)) - 10.0).abs() < 0.01);
    }
}
