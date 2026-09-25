//! The hub mixer: per-source gain and mute, a master gain, priority ducking and a soft
//! limiter. All gain changes are ramped over one frame so there are no clicks.
//!
//! Ducking: while any priority source's level is above `duck_threshold_db`, every
//! non-priority source is attenuated by `duck_db`, with `attack_ms`/`release_ms` smoothing.
//!
//! # Details
//!
//! - **Gain ramps.** A source's effective gain (`gain`, or 0 when muted) and the master gain
//!   move linearly from their previous value to the new one across the samples of one
//!   [`Mixer::mix`] call, so a step change never produces a click.
//! - **Ducking detector.** A priority source is "active" in a block when its post-gain RMS
//!   level (before ducking; muted = silent) exceeds `duck_threshold_db`. The shared duck gain
//!   follows `db_to_amplitude(duck_db)` (while any priority source is active) or 1.0 with a
//!   one-pole smoother per sample: time constant `attack_ms` when going down, `release_ms`
//!   when coming back up. Priority sources are never ducked.
//! - **Limiter.** With `limiter: true` the master bus goes through a stereo-linked peak
//!   limiter: instant attack (the envelope is the max of the current frame's peak and the
//!   decaying previous envelope, so gain · |x| ≤ threshold is guaranteed), exponential
//!   release of [`LIMITER_RELEASE_MS`], threshold [`LIMITER_THRESHOLD`]. The output is then
//!   clamped to [-1, 1] as a final safety net (non-finite samples become 0). With
//!   `limiter: false` the output is only hard-clamped.
//! - **Levels.** [`Mixer::levels`] reports each source's level after gain, mute and ducking
//!   (what it contributes to the mix); a source without input in the last call is silent.
//! - **Real time.** [`Mixer::mix`] never allocates; per-source state is allocated by
//!   [`Mixer::add_source`]. Inputs for unknown source ids are ignored.

use serde::{Deserialize, Serialize};

use crate::meter::{db_to_amplitude, level_from_stats, Level};

/// Limiter threshold (linear, ≈ −0.3 dBFS).
pub const LIMITER_THRESHOLD: f32 = 0.966;
/// Limiter release time constant in ms.
pub const LIMITER_RELEASE_MS: f32 = 80.0;
/// Upper bound of source and master gains.
pub const MAX_GAIN: f32 = 4.0;

/// Clamps a gain to `0.0..=MAX_GAIN` (NaN becomes 0).
fn clamp_gain(g: f32) -> f32 {
    if g.is_nan() {
        0.0
    } else {
        g.clamp(0.0, MAX_GAIN)
    }
}

/// One-pole smoothing coefficient for a time constant in ms at `rate` Hz.
fn smoothing_coef(ms: f32, rate: u32) -> f32 {
    let samples = ms.max(0.0) * 1e-3 * rate as f32;
    if samples < 1e-3 || !samples.is_finite() {
        0.0
    } else {
        (-1.0 / samples).exp()
    }
}

/// Per-source state (preallocated in `add_source`).
#[derive(Debug, Clone)]
struct Source {
    id: SourceId,
    gain: f32,
    muted: bool,
    priority: bool,
    /// Effective (gain × !muted) value reached at the end of the last mix.
    current: f32,
    level: Level,
}

impl Source {
    fn target(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.gain
        }
    }
}

/// Identifier of a mixer input (the hub uses the stream id).
pub type SourceId = u32;

/// Mixer configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MixerConfig {
    /// Sample rate in Hz (needed for attack/release time constants).
    pub sample_rate: u32,
    /// Interleaved channel count of inputs and output.
    pub channels: u16,
    /// Attenuation applied to non-priority sources while ducking, in dB (negative).
    pub duck_db: f32,
    /// Level above which a priority source triggers ducking, in dBFS.
    pub duck_threshold_db: f32,
    /// Ducking attack time in ms.
    pub attack_ms: f32,
    /// Ducking release time in ms.
    pub release_ms: f32,
    /// Enable the soft limiter on the master bus.
    pub limiter: bool,
}

impl Default for MixerConfig {
    /// 48 kHz stereo, duck −12 dB above −40 dBFS, attack 10 ms, release 300 ms, limiter on.
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            duck_db: -12.0,
            duck_threshold_db: -40.0,
            attack_ms: 10.0,
            release_ms: 300.0,
            limiter: true,
        }
    }
}

/// The mixer. Runs on the hub mixer thread.
#[derive(Debug)]
pub struct Mixer {
    config: MixerConfig,
    frame_frames: usize,
    sources: Vec<Source>,
    master_gain: f32,
    master_current: f32,
    /// Current shared duck gain (linear).
    duck: f32,
    duck_floor: f32,
    duck_threshold: f32,
    attack_coef: f32,
    release_coef: f32,
    limiter_env: f32,
    limiter_release_coef: f32,
    master_level: Level,
}

impl Mixer {
    /// Creates a mixer processing `frame_frames` frames per [`Mixer::mix`] call.
    pub fn new(config: MixerConfig, frame_frames: usize) -> Self {
        let rate = config.sample_rate.max(1);
        Self {
            config,
            frame_frames,
            sources: Vec::with_capacity(8),
            master_gain: 1.0,
            master_current: 1.0,
            duck: 1.0,
            duck_floor: db_to_amplitude(config.duck_db.min(0.0)),
            duck_threshold: db_to_amplitude(config.duck_threshold_db),
            attack_coef: smoothing_coef(config.attack_ms, rate),
            release_coef: smoothing_coef(config.release_ms, rate),
            limiter_env: 0.0,
            limiter_release_coef: smoothing_coef(LIMITER_RELEASE_MS, rate),
            master_level: Level::SILENT,
        }
    }

    fn source_mut(&mut self, id: SourceId) -> Option<&mut Source> {
        self.sources.iter_mut().find(|s| s.id == id)
    }

    /// The current shared duck gain applied to non-priority sources (1.0 = not ducked).
    pub fn duck_gain(&self) -> f32 {
        self.duck
    }

    /// Ids of all sources, in insertion order.
    pub fn source_ids(&self) -> impl Iterator<Item = SourceId> + '_ {
        self.sources.iter().map(|s| s.id)
    }

    /// The configuration.
    pub fn config(&self) -> &MixerConfig {
        &self.config
    }

    /// Frames per mix call.
    pub fn frame_frames(&self) -> usize {
        self.frame_frames
    }

    /// Adds a source (gain 1.0, unmuted, not priority). No-op if it exists.
    pub fn add_source(&mut self, id: SourceId) {
        if self.sources.iter().any(|s| s.id == id) {
            return;
        }
        self.sources.push(Source {
            id,
            gain: 1.0,
            muted: false,
            priority: false,
            // Fade in from silence on the first mix.
            current: 0.0,
            level: Level::SILENT,
        });
    }

    /// Removes a source. No-op if unknown.
    pub fn remove_source(&mut self, id: SourceId) {
        self.sources.retain(|s| s.id != id);
    }

    /// Sets a source's linear gain (clamped to 0.0..=4.0).
    pub fn set_gain(&mut self, id: SourceId, gain: f32) {
        if let Some(s) = self.source_mut(id) {
            s.gain = clamp_gain(gain);
        }
    }

    /// Mutes or unmutes a source.
    pub fn set_muted(&mut self, id: SourceId, muted: bool) {
        if let Some(s) = self.source_mut(id) {
            s.muted = muted;
        }
    }

    /// Marks a source as priority (it ducks the others).
    pub fn set_priority(&mut self, id: SourceId, priority: bool) {
        if let Some(s) = self.source_mut(id) {
            s.priority = priority;
        }
    }

    /// Sets the master linear gain (clamped to 0.0..=4.0).
    pub fn set_master_gain(&mut self, gain: f32) {
        self.master_gain = clamp_gain(gain);
    }

    /// Mixes one frame. Each input slice holds `frame_frames * channels` interleaved samples;
    /// sources without an input this tick contribute silence. `out` (same length) is
    /// overwritten. Must not allocate.
    ///
    /// The block length is `out.len() / channels` frames (normally `frame_frames`); a shorter
    /// input slice is padded with silence, a longer one truncated.
    pub fn mix(&mut self, inputs: &[(SourceId, &[f32])], out: &mut [f32]) {
        let ch = usize::from(self.config.channels.max(1));
        let frames = out.len() / ch;
        let n = frames * ch;
        out.fill(0.0);
        if frames == 0 {
            return;
        }
        let inv_frames = 1.0 / frames as f32;

        // 1. Ducking detector: any priority source above the threshold (post-gain RMS).
        let mut duck_active = false;
        for &(id, input) in inputs {
            let Some(src) = self.sources.iter().find(|s| s.id == id) else {
                continue;
            };
            if !src.priority || src.muted {
                continue;
            }
            let m = input.len().min(n);
            let sum_sq: f32 = input[..m]
                .iter()
                .map(|x| if x.is_finite() { x * x } else { 0.0 })
                .sum();
            let rms = (sum_sq / n as f32).sqrt() * src.gain;
            if rms > self.duck_threshold {
                duck_active = true;
            }
        }
        let duck_start = self.duck;
        let (duck_target, duck_coef) = if duck_active {
            (self.duck_floor, self.attack_coef)
        } else {
            (1.0, self.release_coef)
        };

        // 2. Sources.
        for src in &mut self.sources {
            let start = src.current;
            let end = src.target();
            src.current = end;
            src.level = Level::SILENT;
            let ducked = !src.priority;
            let mut peak = 0.0_f32;
            let mut sum_sq = 0.0_f64;
            let mut any = false;
            for &(id, input) in inputs {
                if id != src.id {
                    continue;
                }
                any = true;
                let m = input.len().min(n);
                let mut duck = duck_start;
                for (f, (o_frame, i_frame)) in out[..m]
                    .chunks_exact_mut(ch)
                    .zip(input[..m].chunks_exact(ch))
                    .enumerate()
                {
                    let g_src = start + (end - start) * (f + 1) as f32 * inv_frames;
                    let g = if ducked {
                        duck = duck_target + (duck - duck_target) * duck_coef;
                        g_src * duck
                    } else {
                        g_src
                    };
                    for (o, &x) in o_frame.iter_mut().zip(i_frame) {
                        let v = if x.is_finite() { x * g } else { 0.0 };
                        *o += v;
                        peak = peak.max(v.abs());
                        sum_sq += f64::from(v) * f64::from(v);
                    }
                }
            }
            if any {
                src.level = level_from_stats(peak, sum_sq, n);
            }
        }
        // Advance the shared duck envelope by the whole block.
        let mut duck = duck_start;
        for _ in 0..frames {
            duck = duck_target + (duck - duck_target) * duck_coef;
        }
        self.duck = duck;

        // 3. Master gain ramp, limiter, safety clamp, meter.
        let (m_start, m_end) = (self.master_current, self.master_gain);
        self.master_current = m_end;
        let mut env = self.limiter_env;
        let mut peak = 0.0_f32;
        let mut sum_sq = 0.0_f64;
        for (f, frame) in out[..n].chunks_exact_mut(ch).enumerate() {
            let g = m_start + (m_end - m_start) * (f + 1) as f32 * inv_frames;
            let mut frame_peak = 0.0_f32;
            for x in frame.iter_mut() {
                *x *= g;
                if !x.is_finite() {
                    *x = 0.0;
                }
                frame_peak = frame_peak.max(x.abs());
            }
            if self.config.limiter {
                env = frame_peak.max(env * self.limiter_release_coef);
                if env > LIMITER_THRESHOLD {
                    let lg = LIMITER_THRESHOLD / env;
                    frame.iter_mut().for_each(|x| *x *= lg);
                }
            }
            for x in frame.iter_mut() {
                *x = x.clamp(-1.0, 1.0);
                peak = peak.max(x.abs());
                sum_sq += f64::from(*x) * f64::from(*x);
            }
        }
        self.limiter_env = env;
        self.master_level = level_from_stats(peak, sum_sq, n);
    }

    /// Post-gain level of every source from the last [`Mixer::mix`] call.
    pub fn levels(&self) -> Vec<(SourceId, Level)> {
        self.sources.iter().map(|s| (s.id, s.level)).collect()
    }

    /// Level of the master output from the last [`Mixer::mix`] call.
    pub fn master_level(&self) -> Level {
        self.master_level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::AudioFormat;
    use crate::meter::amplitude_to_db;
    use crate::tone::SineGenerator;

    const FRAMES: usize = 480;

    fn mixer() -> Mixer {
        Mixer::new(MixerConfig::default(), FRAMES)
    }

    /// Mixes `blocks` blocks of constant inputs, returning every output sample.
    fn run(m: &mut Mixer, inputs: &[(SourceId, f32)], blocks: usize) -> Vec<f32> {
        let bufs: Vec<(SourceId, Vec<f32>)> = inputs
            .iter()
            .map(|&(id, v)| (id, vec![v; FRAMES * 2]))
            .collect();
        let refs: Vec<(SourceId, &[f32])> =
            bufs.iter().map(|(id, b)| (*id, b.as_slice())).collect();
        let mut all = Vec::new();
        let mut out = vec![0.0; FRAMES * 2];
        for _ in 0..blocks {
            m.mix(&refs, &mut out);
            all.extend_from_slice(&out);
        }
        all
    }

    #[test]
    fn sums_sources_with_gains_and_fade_in() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(2);
        m.add_source(1); // no-op
        assert_eq!(m.source_ids().collect::<Vec<_>>(), vec![1, 2]);
        let out = run(&mut m, &[(1, 0.2), (2, 0.1)], 2);
        // First block fades in from silence, linearly.
        assert!(out[0] < 0.01);
        assert!((out[FRAMES * 2 - 1] - 0.3).abs() < 1e-6);
        assert!(out[FRAMES * 2..].iter().all(|x| (x - 0.3).abs() < 1e-6));
        m.set_gain(1, 2.0);
        m.set_master_gain(0.5);
        let out = run(&mut m, &[(1, 0.2), (2, 0.1)], 2);
        let settled = &out[FRAMES * 2..];
        assert!(
            settled.iter().all(|x| (x - 0.25).abs() < 1e-6),
            "{}",
            settled[0]
        );
        // Gains are clamped; unknown ids are ignored.
        m.set_gain(1, 100.0);
        m.set_master_gain(-1.0);
        let out = run(&mut m, &[(1, 0.2), (99, 1.0)], 2);
        assert!(out[FRAMES * 2..].iter().all(|x| *x == 0.0));
        m.remove_source(1);
        m.remove_source(42);
        assert_eq!(m.source_ids().collect::<Vec<_>>(), vec![2]);
        assert_eq!(m.config().channels, 2);
        assert_eq!(m.frame_frames(), FRAMES);
    }

    #[test]
    fn mute_ramps_without_discontinuity() {
        let mut m = mixer();
        m.add_source(7);
        let mut out = run(&mut m, &[(7, 0.5)], 3);
        m.set_muted(7, true);
        out.extend(run(&mut m, &[(7, 0.5)], 2));
        m.set_muted(7, false);
        out.extend(run(&mut m, &[(7, 0.5)], 2));
        // Largest sample-to-sample jump is one ramp step (0.5 / 480), never a click.
        let max_step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step <= 0.5 / FRAMES as f32 + 1e-6, "step {max_step}");
        // Fully muted in the block after the ramp, fully back after unmuting.
        let b = FRAMES * 2;
        assert!(out[4 * b..5 * b].iter().all(|x| *x == 0.0));
        assert!(out[6 * b..].iter().all(|x| (x - 0.5).abs() < 1e-6));
        // Muted sources report silence.
        m.set_muted(7, true);
        run(&mut m, &[(7, 0.5)], 2);
        assert_eq!(m.levels(), vec![(7, Level::SILENT)]);
    }

    #[test]
    fn priority_source_ducks_others_and_releases() {
        let cfg = MixerConfig::default();
        let mut m = Mixer::new(cfg, FRAMES);
        m.add_source(1); // music
        m.add_source(2); // voice (priority)
        m.set_priority(2, true);
        let fmt = AudioFormat::INTERNAL;
        let mut voice_gen = SineGenerator::new(300.0, 0.3, fmt);
        let music = vec![0.1_f32; FRAMES * 2];
        let silence = vec![0.0_f32; FRAMES * 2];
        let mut voice = vec![0.0_f32; FRAMES * 2];
        let mut out = vec![0.0_f32; FRAMES * 2];
        let music_level = |m: &Mixer| m.levels()[0].1.rms_db;

        // Voice silent: music untouched.
        for _ in 0..5 {
            m.mix(&[(1, &music), (2, &silence)], &mut out);
        }
        assert!((music_level(&m) - amplitude_to_db(0.1)).abs() < 0.01);
        assert_eq!(m.duck_gain(), 1.0);

        // Voice starts: attack of 10 ms, so after 50 ms the music is ~12 dB down.
        for _ in 0..5 {
            voice_gen.fill(&mut voice);
            m.mix(&[(1, &music), (2, &voice)], &mut out);
        }
        let ducked = music_level(&m) - amplitude_to_db(0.1);
        assert!((ducked - cfg.duck_db).abs() < 0.5, "ducked by {ducked} dB");
        // The priority source itself is not ducked.
        assert!((m.levels()[1].1.peak_db - amplitude_to_db(0.3)).abs() < 0.1);

        // Voice stops: release is slow (300 ms) ...
        for _ in 0..5 {
            m.mix(&[(1, &music), (2, &silence)], &mut out);
        }
        let after_50ms = music_level(&m) - amplitude_to_db(0.1);
        assert!(
            after_50ms < -1.0 && after_50ms > cfg.duck_db + 0.5,
            "{after_50ms}"
        );
        // ... but complete after 2 s.
        for _ in 0..195 {
            m.mix(&[(1, &music), (2, &silence)], &mut out);
        }
        assert!((music_level(&m) - amplitude_to_db(0.1)).abs() < 0.05);

        // A quiet priority source (below -40 dBFS) or a muted one does not duck.
        let whisper = vec![0.001_f32; FRAMES * 2];
        for _ in 0..20 {
            m.mix(&[(1, &music), (2, &whisper)], &mut out);
        }
        assert!(m.duck_gain() > 0.999);
        m.set_muted(2, true);
        for _ in 0..20 {
            voice_gen.fill(&mut voice);
            m.mix(&[(1, &music), (2, &voice)], &mut out);
        }
        assert!(m.duck_gain() > 0.999);
    }

    #[test]
    fn duck_gain_is_continuous() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(2);
        m.set_priority(2, true);
        let music = vec![0.2_f32; FRAMES * 2];
        let loud = vec![0.2_f32; FRAMES * 2];
        let quiet = vec![0.0_f32; FRAMES * 2];
        let mut out = vec![0.0; FRAMES * 2];
        let mut music_only = Vec::new();
        for i in 0..40 {
            let voice: &[f32] = if (10..20).contains(&i) { &loud } else { &quiet };
            m.mix(&[(1, &music), (2, voice)], &mut out);
            // Remove the voice contribution to look at the ducked music alone.
            for (o, v) in out.iter().zip(voice) {
                music_only.push(o - v);
            }
        }
        // 10 ms attack at 48 kHz: the largest per-sample change is 0.2·(1−0.25)/480 ≈ 0.0003
        // (the sum stays below the limiter threshold, so subtracting the voice is exact).
        let max_step = music_only
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.0005, "step {max_step}");
        // It did duck and recover.
        let min = music_only[FRAMES * 20..]
            .iter()
            .fold(1.0_f32, |a, b| a.min(*b));
        assert!(
            (min - 0.2 * db_to_amplitude(-12.0)).abs() < 0.005,
            "min {min}"
        );
    }

    #[test]
    fn limiter_bounds_eight_full_scale_sources() {
        let mut m = mixer();
        let mut gens: Vec<SineGenerator> = (0..8)
            .map(|i| SineGenerator::new(100.0 + 37.0 * i as f32, 1.0, AudioFormat::INTERNAL))
            .collect();
        for id in 0..8 {
            m.add_source(id);
            m.set_gain(id, MAX_GAIN);
        }
        m.set_master_gain(MAX_GAIN);
        let mut bufs = vec![vec![0.0_f32; FRAMES * 2]; 8];
        let mut out = vec![0.0; FRAMES * 2];
        let mut worst = 0.0_f32;
        for block in 0..200 {
            for (g, b) in gens.iter_mut().zip(bufs.iter_mut()) {
                g.fill(b);
            }
            // Throw in a DC square and a NaN once in a while.
            if block % 17 == 0 {
                bufs[0].iter_mut().for_each(|x| *x = 1.0);
                bufs[1][3] = f32::NAN;
            }
            let inputs: Vec<(SourceId, &[f32])> = bufs
                .iter()
                .enumerate()
                .map(|(i, b)| (i as u32, b.as_slice()))
                .collect();
            m.mix(&inputs, &mut out);
            worst = out.iter().fold(worst, |w, x| w.max(x.abs()));
            assert!(out.iter().all(|x| x.is_finite()));
        }
        assert!(worst <= 1.0, "peak {worst}");
        assert!(worst > 0.9, "limiter should not over-attenuate: {worst}");
        assert!(m.master_level().peak_db <= 0.0);
    }

    #[test]
    fn limiter_is_transparent_below_threshold() {
        let mut m = mixer();
        m.add_source(1);
        let mut g = SineGenerator::new(1000.0, 0.5, AudioFormat::INTERNAL);
        let mut buf = vec![0.0; FRAMES * 2];
        let mut out = vec![0.0; FRAMES * 2];
        g.fill(&mut buf);
        m.mix(&[(1, &buf)], &mut out); // fade-in block
        for _ in 0..10 {
            g.fill(&mut buf);
            m.mix(&[(1, &buf)], &mut out);
            assert_eq!(out, buf);
        }
        // Without the limiter the output is only clamped.
        let mut m = Mixer::new(
            MixerConfig {
                limiter: false,
                ..MixerConfig::default()
            },
            FRAMES,
        );
        m.add_source(1);
        m.set_gain(1, 3.0);
        let loud = vec![0.5_f32; FRAMES * 2];
        m.mix(&[(1, &loud)], &mut out);
        m.mix(&[(1, &loud)], &mut out);
        assert!(out.iter().all(|x| *x == 1.0));
    }

    #[test]
    fn levels_report_post_gain() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(2);
        m.set_gain(1, 0.5);
        run(&mut m, &[(1, 0.4)], 3);
        let levels = m.levels();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].0, 1);
        assert!((levels[0].1.peak_db - amplitude_to_db(0.2)).abs() < 1e-3);
        assert!((levels[0].1.rms_db - amplitude_to_db(0.2)).abs() < 1e-3);
        assert_eq!(levels[1], (2, Level::SILENT), "no input = silent");
        assert!((m.master_level().rms_db - amplitude_to_db(0.2)).abs() < 1e-3);
        // Short inputs are padded with silence; an empty output is a no-op.
        let mut out = vec![1.0; FRAMES * 2];
        m.mix(&[(1, &[0.4; 10])], &mut out);
        assert!(out[10..].iter().all(|x| *x == 0.0));
        m.mix(&[(1, &[0.4; 10])], &mut []);
    }
}
