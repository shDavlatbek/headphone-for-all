//! `hfa selftest`: an in-process hub (WAV output in a temporary directory, port 0, no mDNS)
//! plus K tone senders (distinct frequencies) whose media goes through an in-process UDP
//! impairment proxy (random loss, random delay → reordering). Every sender pairs with its own
//! PIN. Afterwards the hub's WAV is analysed:
//!
//! - every tone must be present (Hann-windowed Goertzel, median over 20 ms windows);
//! - glitches (a tone leaving 0.5..1.5 × its median, or energy that is not a tone above
//!   −17 dB) are counted after a warm-up; their number and the time spent glitching must stay
//!   under a budget that grows with the configured loss;
//! - the end-to-end latency (capture → output) is measured on sender 1, which is silent until
//!   the warm-up starts and then starts its tone with a sharp onset;
//! - per sender: packets sent / received / lost / recovered, plus the proxy's counters.
//!
//! Timeline after the last sender streams (`T0`): onset at `T0 + LEAD_IN`, analysis from
//! `T0 + WARM_UP` for `--seconds`, then everything is stopped.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use hfa_audio::AudioFormat;
use hfa_capture::output_file::WavFileOutput;
use hfa_capture::{AudioOutput, CaptureError, CaptureSource, PcmSource};
use hfa_core::netsim::{ImpairConfig, ProxyStats, UdpImpairProxy};
use hfa_core::{
    HubAddress, HubConfig, HubEngine, HubEvent, HubHandle, SenderConfig, SenderEngine,
    SenderHandle, SenderState, Settings, StreamCounters,
};
use tokio::sync::broadcast;

use crate::analysis::{self, GlitchReport};
use crate::cli::SelftestArgs;
use crate::display::Table;
use crate::onset::{OnsetToneSource, OnsetTrigger};

/// Tone frequencies of senders 1, 2, ... (at least 500 Hz apart: the 20 ms Hann windows
/// separate them cleanly).
pub const FREQUENCIES: [f64; 8] = [
    440.0, 1000.0, 2500.0, 4000.0, 6000.0, 1500.0, 3200.0, 5000.0,
];
/// Silence of sender 1 after the last sender streams, before its tone starts.
const LEAD_IN: Duration = Duration::from_millis(500);
/// Audio before this point (after `T0`) is not analysed: onset, jitter-buffer adaptation and
/// the first loss report (which switches redundancy on) happen here.
const WARM_UP: Duration = Duration::from_millis(1500);
/// Extra run time after the analysed audio, so the output's last blocks are complete.
const TAIL: Duration = Duration::from_millis(150);
/// Block period of the WAV output.
const OUTPUT_BLOCK_MS: u32 = 10;
/// How long one sender may take to pair and stream.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Glitch budget at 0 % loss.
const BASE_GLITCHES: f64 = 2.0;
/// Budget per expected lost packet (redundancy recovers isolated losses; bursts and losses
/// before redundancy switches on are concealed and may be audible).
const GLITCHES_PER_LOST_PACKET: f64 = 0.2;
/// Abnormal analysis windows allowed per budgeted glitch (one glitch spans 2-4 overlapping
/// windows): bounds the time spent glitching when glitches merge into long runs.
const WINDOWS_PER_GLITCH: usize = 3;
/// A tone is present when its median amplitude reaches this fraction (−3 dB) of the
/// expected one.
const PRESENCE: f64 = 0.7;
/// Amplitude of each tone with few senders (−12 dBFS).
const NOMINAL_AMPLITUDE: f64 = 0.25;
/// Largest possible peak of the mix (all tones in phase), below the hub's limiter threshold
/// (0.966).
const MAX_MIX_PEAK: f64 = 0.9;

/// Runs the selftest; `Err` (non-zero exit) if a check fails.
pub async fn run(args: SelftestArgs) -> anyhow::Result<()> {
    let senders = usize::from(args.senders);
    let seed = args.seed.unwrap_or_else(random_seed);
    println!(
        "hfa selftest: {senders} sender(s), {} s analysed, loss {} %, jitter 0..{} ms, seed {seed}",
        args.seconds, args.loss, args.jitter
    );
    let tmp = tempfile::Builder::new()
        .prefix("hfa-selftest-")
        .tempdir()
        .context("cannot create a temporary directory")?;
    let wav = match &args.wav {
        Some(path) => path.clone(),
        None => tmp.path().join("mix.wav"),
    };

    let run = run_session(&args, senders, seed, tmp.path(), &wav).await?;
    let report = analyse(&args, senders, &wav, &run)?;
    print_report(&args, &wav, &run, &report);
    if report.failures.is_empty() {
        println!("\nResult: PASS");
        Ok(())
    } else {
        println!("\nResult: FAIL");
        for f in &report.failures {
            println!("  - {f}");
        }
        if args.wav.is_none() {
            println!("  (re-run with --wav <path> to keep the hub's output)");
        }
        Err(anyhow!("selftest failed: {}", report.failures.join("; ")))
    }
}

/// Everything measured while the engines ran.
struct Session {
    /// When the output started (sample 0 of the WAV).
    origin: Instant,
    /// When the last sender started streaming.
    t0: Instant,
    /// Capture time of sender 1's first tone sample.
    onset: Option<Instant>,
    senders: Vec<SenderReport>,
    proxy: ProxyStats,
    port: u16,
    proxy_port: u16,
}

/// Per-sender numbers.
struct SenderReport {
    freq: f64,
    /// How long pairing + stream set-up took.
    setup: Duration,
    /// Audio packets and keep-alives sent.
    sent: (u64, u64),
    counters: StreamCounters,
    /// The hub's latency estimate for this stream (ms).
    hub_latency_ms: f32,
    /// Loss the sender was told about (network loss, %).
    reported_loss_pct: f32,
}

async fn run_session(
    args: &SelftestArgs,
    senders: usize,
    seed: u64,
    dir: &Path,
    wav: &Path,
) -> anyhow::Result<Session> {
    let hub_dir = dir.join("hub");
    let origin = Arc::new(OnceLock::new());
    let output = WavFileOutput::create(wav, AudioFormat::INTERNAL, OUTPUT_BLOCK_MS)
        .with_context(|| format!("cannot create {}", wav.display()))?;
    let hub = HubEngine::start(HubConfig {
        settings: settings("selftest-hub", &hub_dir, 0)?,
        output: Box::new(TimedOutput {
            inner: Box::new(output),
            started: Arc::clone(&origin),
        }),
        advertise: false,
    })
    .await
    .context("cannot start the hub")?;
    let port = hub.local_port();
    let proxy = UdpImpairProxy::start(
        ([127, 0, 0, 1], port).into(),
        ImpairConfig {
            loss_pct: args.loss,
            jitter_ms: args.jitter,
            seed,
        },
    )
    .await
    .context("cannot start the impairment proxy")?;
    let proxy_port = proxy.local_addr().port();
    hub.set_media_port_override(Some(proxy_port));
    println!("hub on 127.0.0.1:{port}, media through the impairment proxy on port {proxy_port}");

    let result = drive(args, senders, dir, &hub).await;
    let proxy_stats = proxy.stats();
    let (handles, outcome) = match result {
        Ok((handles, outcome)) => (handles, Ok(outcome)),
        Err((handles, e)) => (handles, Err(e)),
    };
    for h in handles {
        h.stop().await;
    }
    hub.stop().await;
    proxy.stop().await;
    let (t0, onset, reports) = outcome?;
    Ok(Session {
        origin: *origin
            .get()
            .ok_or_else(|| anyhow!("the hub never started its output"))?,
        t0,
        onset,
        senders: reports,
        proxy: proxy_stats,
        port,
        proxy_port,
    })
}

type Outcome = (Instant, Option<Instant>, Vec<SenderReport>);

/// Starts the senders one by one (pairing each with a fresh PIN), runs the timeline and
/// collects the numbers. Returns the sender handles in every case so they can be stopped.
async fn drive(
    args: &SelftestArgs,
    senders: usize,
    dir: &Path,
    hub: &HubHandle,
) -> Result<(Vec<SenderHandle>, Outcome), (Vec<SenderHandle>, anyhow::Error)> {
    let mut events = hub.events();
    let trigger = OnsetTrigger::default();
    let mut handles = Vec::new();
    let mut started = Vec::new();
    for (i, freq) in FREQUENCIES.iter().take(senders).enumerate() {
        let t = Instant::now();
        match start_sender(i, senders, *freq, dir, hub, &mut events, &trigger).await {
            Ok((handle, stream_id)) => {
                println!(
                    "sender {} ({freq} Hz) paired and streaming after {} ms",
                    i + 1,
                    t.elapsed().as_millis()
                );
                handles.push(handle);
                started.push((stream_id, t.elapsed()));
            }
            Err(e) => {
                return Err((
                    handles,
                    e.context(format!("sender {} did not start", i + 1)),
                ))
            }
        }
    }
    let t0 = Instant::now();
    tokio::time::sleep(LEAD_IN).await;
    trigger.arm();
    let total = WARM_UP + Duration::from_secs(u64::from(args.seconds)) + TAIL;
    println!("running for {:.1} s...", (total - LEAD_IN).as_secs_f64());
    tokio::time::sleep_until(tokio::time::Instant::from_std(t0 + total)).await;

    let sources = hub.sources();
    let mut reports = Vec::new();
    for (i, (handle, (stream_id, setup))) in handles.iter().zip(&started).enumerate() {
        let counters = hub.stream_counters(*stream_id).unwrap_or_default();
        let info = sources.iter().find(|s| s.stream_id == *stream_id);
        let status = handle.status();
        reports.push(SenderReport {
            freq: FREQUENCIES[i],
            setup: *setup,
            sent: handle.packets_sent(),
            counters,
            hub_latency_ms: info.map_or(0.0, |s| s.stats.latency_ms),
            reported_loss_pct: status.loss_pct,
        });
        if status.state != SenderState::Streaming {
            let e = anyhow!("sender {} ended in state {:?}", i + 1, status.state);
            return Err((handles, e));
        }
    }
    Ok((handles, (t0, trigger.onset(), reports)))
}

/// Starts sender `i` with a fresh pairing PIN and waits until the hub plays its stream.
async fn start_sender(
    i: usize,
    senders: usize,
    freq: f64,
    dir: &Path,
    hub: &HubHandle,
    events: &mut broadcast::Receiver<HubEvent>,
    trigger: &OnsetTrigger,
) -> anyhow::Result<(SenderHandle, u32)> {
    let name = format!("selftest-sender-{}", i + 1);
    // Sender 1 waits for the onset trigger; the others play from the start.
    let trigger = if i == 0 {
        trigger.clone()
    } else {
        let armed = OnsetTrigger::default();
        armed.arm();
        armed
    };
    let capture: Box<dyn CaptureSource> = Box::new(OnsetToneSource::new(
        freq as f32,
        tone_amplitude(senders) as f32,
        trigger,
    ));
    let pin = hub.start_pairing().pin;
    let sender = SenderEngine::start(SenderConfig {
        hub: HubAddress::Direct {
            host: "127.0.0.1".into(),
            port: hub.local_port(),
        },
        settings: settings(&name, &dir.join(&name), 0)?,
        capture,
        label: format!("Tone {freq} Hz"),
        expected_hub_key: None,
        pairing_secret: Some(pin),
    })
    .await?;
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    let mut stream_id = None;
    loop {
        match sender.status().state {
            SenderState::Failed(reason) => bail!("{reason}"),
            SenderState::Streaming if stream_id.is_some() => break,
            _ => {}
        }
        let wait = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                ev = events.recv() => Some(ev),
                _ = tokio::time::sleep(Duration::from_millis(50)) => None,
            }
        })
        .await;
        match wait {
            Err(_) => bail!(
                "not streaming after {} s (state {:?})",
                START_TIMEOUT.as_secs(),
                sender.status().state
            ),
            Ok(Some(Ok(HubEvent::SourceAdded(info)))) if info.device_name == name => {
                stream_id = Some(info.stream_id);
            }
            Ok(Some(Ok(HubEvent::PairingFailed { reason }))) => bail!("pairing failed: {reason}"),
            Ok(Some(Err(broadcast::error::RecvError::Closed))) => bail!("the hub stopped"),
            Ok(_) => {}
        }
    }
    let id = stream_id.ok_or_else(|| anyhow!("no stream"))?;
    Ok((sender, id))
}

/// Settings of a selftest device (its own data dir, so identities and trust are fresh).
fn settings(name: &str, dir: &Path, port: u16) -> anyhow::Result<Settings> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(Settings {
        device_name: name.to_owned(),
        port,
        data_dir: dir.to_path_buf(),
        ..Settings::default()
    })
}

fn random_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5eed)
}

/// An output that records when it was started (the time of the WAV's first sample).
struct TimedOutput {
    inner: Box<dyn AudioOutput>,
    started: Arc<OnceLock<Instant>>,
}

impl AudioOutput for TimedOutput {
    fn format(&self) -> AudioFormat {
        self.inner.format()
    }

    fn start(&mut self, source: PcmSource) -> Result<(), CaptureError> {
        let _ = self.started.set(Instant::now());
        self.inner.start(source)
    }

    fn stop(&mut self) {
        self.inner.stop();
    }

    fn latency_ms(&self) -> Option<f32> {
        self.inner.latency_ms()
    }

    fn has_error(&self) -> bool {
        self.inner.has_error()
    }

    fn xruns(&self) -> u64 {
        self.inner.xruns()
    }

    fn restartable(&self) -> bool {
        self.inner.restartable()
    }
}

/// Analysis results and failed checks.
struct Report {
    glitches: GlitchReport,
    expected_amplitude: f64,
    glitch_budget: usize,
    dropout_ms: f64,
    /// Capture → output latency of sender 1's onset (ms).
    latency_ms: Option<f64>,
    analysed_s: f64,
    failures: Vec<String>,
}

/// Amplitude of each tone: −12 dBFS, lowered with many senders so that the mix never
/// reaches the hub's limiter (whose gain changes would distort the tones).
fn tone_amplitude(senders: usize) -> f64 {
    NOMINAL_AMPLITUDE.min(MAX_MIX_PEAK / senders.max(1) as f64)
}

/// Glitch budget: [`BASE_GLITCHES`] plus [`GLITCHES_PER_LOST_PACKET`] per packet the
/// configured loss is expected to drop during the analysed audio.
pub fn glitch_budget(loss_pct: f32, seconds: f64, senders: usize) -> usize {
    let packets = seconds * 100.0 * senders as f64; // 10 ms frames
    let lost = packets * f64::from(loss_pct) / 100.0;
    (BASE_GLITCHES + GLITCHES_PER_LOST_PACKET * lost).floor() as usize
}

fn analyse(
    args: &SelftestArgs,
    senders: usize,
    wav: &Path,
    run: &Session,
) -> anyhow::Result<Report> {
    let (format, samples) = hfa_audio::wav::read_wav(wav)
        .with_context(|| format!("cannot read the hub's output {}", wav.display()))?;
    if format != AudioFormat::INTERNAL {
        bail!("unexpected WAV format {format:?}");
    }
    let left: Vec<f32> = samples.chunks_exact(2).map(|f| f[0]).collect();
    let t0 = run.t0.saturating_duration_since(run.origin).as_secs_f64();
    let from = t0 + WARM_UP.as_secs_f64();
    let to = from + f64::from(args.seconds);
    let segment = analysis::slice(&left, from, to);
    let analysed_s = segment.len() as f64 / analysis::RATE;
    let freqs: Vec<f64> = FREQUENCIES.iter().take(senders).copied().collect();
    let glitches = analysis::glitches(segment, &freqs);
    let dropout_ms = analysis::longest_dropout_ms(segment);
    let expected_amplitude = tone_amplitude(senders);
    let glitch_budget = glitch_budget(args.loss, f64::from(args.seconds), senders);

    let mut failures = Vec::new();
    if analysed_s + 0.05 < f64::from(args.seconds) {
        failures.push(format!(
            "the output holds only {analysed_s:.2} s of the {} s to analyse",
            args.seconds
        ));
    }
    for (freq, amp) in freqs.iter().zip(&glitches.amplitudes) {
        if *amp < PRESENCE * expected_amplitude {
            failures.push(format!(
                "the {freq} Hz tone is missing (amplitude {amp:.3}, needs {:.3})",
                PRESENCE * expected_amplitude
            ));
        }
    }
    if glitches.events > glitch_budget {
        failures.push(format!(
            "{} glitches (budget {glitch_budget} for {} % loss)",
            glitches.events, args.loss
        ));
    }
    if glitches.bad_windows > WINDOWS_PER_GLITCH * glitch_budget {
        failures.push(format!(
            "{} of {} analysis windows are abnormal (at most {} allowed for {} % loss)",
            glitches.bad_windows,
            glitches.windows,
            WINDOWS_PER_GLITCH * glitch_budget,
            args.loss
        ));
    }

    // Latency: where sender 1's onset appears in the output vs. when it was captured.
    // A block pulled from the ring at time t plays during [t, t + block): sample n of the
    // WAV leaves the output at n / rate + one block.
    let latency_ms = run.onset.and_then(|onset| {
        let captured = onset.saturating_duration_since(run.origin).as_secs_f64();
        let search = analysis::slice(&left, captured, captured + 1.0);
        let reference = glitches.amplitudes.first().copied()?;
        analysis::onset(search, FREQUENCIES[0], reference)
            .map(|t| (t + f64::from(OUTPUT_BLOCK_MS) / 1000.0) * 1000.0)
    });
    if latency_ms.is_none() {
        failures.push("the onset of sender 1 was not found in the output".to_owned());
    }
    Ok(Report {
        glitches,
        expected_amplitude,
        glitch_budget,
        dropout_ms,
        latency_ms,
        analysed_s,
        failures,
    })
}

fn print_report(args: &SelftestArgs, wav: &Path, run: &Session, report: &Report) {
    println!("\n=== hfa selftest report ===");
    println!(
        "setup: hub port {}, proxy port {}, {} sender(s), loss {} %, jitter 0..{} ms",
        run.port,
        run.proxy_port,
        run.senders.len(),
        args.loss,
        args.jitter
    );
    if args.wav.is_some() {
        println!("output: {}", wav.display());
    }
    let p = &run.proxy;
    println!(
        "network: proxy received {} datagrams, dropped {} ({:.1} %), forwarded {}",
        p.received,
        p.dropped,
        p.dropped as f64 * 100.0 / p.received.max(1) as f64,
        p.forwarded
    );

    println!("\nper sender:");
    let mut table = Table::new([
        "#",
        "TONE",
        "SETUP",
        "SENT",
        "KEEPALIVE",
        "RECEIVED",
        "LOST",
        "RECOVERED",
        "CONCEALED",
        "LATE",
        "STRETCHED",
        "UNDERRUNS",
        "LOSS SEEN",
        "HUB LATENCY",
    ]);
    for (i, s) in run.senders.iter().enumerate() {
        let c = &s.counters;
        table.row([
            (i + 1).to_string(),
            format!("{} Hz", s.freq),
            format!("{} ms", s.setup.as_millis()),
            s.sent.0.to_string(),
            s.sent.1.to_string(),
            c.datagrams.to_string(),
            c.lost.to_string(),
            (c.recovered_redundancy + c.recovered_fec).to_string(),
            c.concealed.to_string(),
            c.late.to_string(),
            c.stretched.to_string(),
            c.underruns.to_string(),
            format!("{:.1} %", s.reported_loss_pct),
            format!("{:.0} ms", s.hub_latency_ms),
        ]);
    }
    for line in table.render().lines() {
        println!("  {line}");
    }

    let g = &report.glitches;
    println!(
        "\naudio ({:.2} s analysed after a {:.1} s warm-up):",
        report.analysed_s,
        WARM_UP.as_secs_f64()
    );
    for (freq, amp) in FREQUENCIES.iter().zip(&g.amplitudes) {
        let ok = *amp >= PRESENCE * report.expected_amplitude;
        println!(
            "  tone {freq:>6} Hz: amplitude {amp:.3} ({:.1} dBFS, sent at {:.3}, \
             present from {:.3}) {}",
            20.0 * amp.max(1e-9).log10(),
            report.expected_amplitude,
            PRESENCE * report.expected_amplitude,
            if ok { "present" } else { "MISSING" }
        );
    }
    let at: Vec<String> = g.first_events.iter().map(|t| format!("{t:.2}s")).collect();
    println!(
        "  glitches: {} (budget {}), {} of {} 20 ms windows abnormal (at most {}){}",
        g.events,
        report.glitch_budget,
        g.bad_windows,
        g.windows,
        WINDOWS_PER_GLITCH * report.glitch_budget,
        if at.is_empty() {
            String::new()
        } else {
            format!(", first at {}", at.join(" "))
        }
    );
    println!("  longest dropout: {:.0} ms", report.dropout_ms);
    match report.latency_ms {
        Some(ms) => println!(
            "  end-to-end latency (sender 1 capture -> hub output): {ms:.0} ms \
             (hub estimate {:.0} ms)",
            run.senders.first().map_or(0.0, |s| s.hub_latency_ms)
        ),
        None => println!("  end-to-end latency: onset not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_scales_with_loss() {
        assert_eq!(glitch_budget(0.0, 3.0, 2), 2);
        assert_eq!(glitch_budget(5.0, 3.0, 2), 8);
        assert_eq!(glitch_budget(5.0, 10.0, 2), 22);
        assert!(glitch_budget(20.0, 10.0, 2) > glitch_budget(5.0, 10.0, 2));
    }

    #[test]
    fn the_mix_stays_below_the_limiter() {
        assert_eq!(tone_amplitude(2), 0.25);
        assert_eq!(tone_amplitude(3), 0.25);
        for k in 1..=8 {
            assert!(tone_amplitude(k) * k as f64 <= MAX_MIX_PEAK + 1e-9);
        }
        assert!((tone_amplitude(8) * 8.0 - MAX_MIX_PEAK).abs() < 1e-9);
    }

    #[test]
    fn frequencies_are_well_separated() {
        for (i, a) in FREQUENCIES.iter().enumerate() {
            for b in &FREQUENCIES[i + 1..] {
                assert!((a - b).abs() >= 500.0, "{a} vs {b}");
            }
        }
    }
}
