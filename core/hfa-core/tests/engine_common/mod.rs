//! Helpers shared by the `engine_*` integration tests: temporary devices, hub/sender
//! start-up, event waiting and WAV analysis (Goertzel tone detection, dropout search).

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hfa_audio::AudioFormat;
use hfa_capture::output_file::{NullOutput, WavFileOutput};
use hfa_capture::{AudioOutput, CaptureTarget};
use hfa_core::{
    HubAddress, HubConfig, HubEngine, HubEvent, HubHandle, SenderConfig, SenderEngine, SenderEvent,
    SenderHandle, SenderState, Settings,
};
use tempfile::TempDir;
use tokio::sync::broadcast;

/// Tests in one binary run one at a time (each runs real-time audio threads; running them
/// in parallel on a small CI machine would only add scheduling noise).
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// A device's data directory.
pub struct Device {
    pub dir: TempDir,
    pub name: String,
}

impl Device {
    pub fn new(name: &str) -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
            name: name.to_owned(),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn settings(&self, port: u16) -> Settings {
        Settings {
            device_name: self.name.clone(),
            port,
            data_dir: self.path().to_path_buf(),
            ..Settings::default()
        }
    }
}

/// Output of a test hub.
pub enum Out {
    Wav(PathBuf),
    Null,
}

/// A started hub plus when its output started (the WAV's time origin).
pub struct TestHub {
    pub hub: HubHandle,
    pub events: broadcast::Receiver<HubEvent>,
    pub started: Instant,
}

pub async fn start_hub(dev: &Device, port: u16, out: Out) -> TestHub {
    let output: Box<dyn AudioOutput> = match out {
        Out::Wav(path) => {
            Box::new(WavFileOutput::create(&path, AudioFormat::INTERNAL, 10).expect("wav output"))
        }
        Out::Null => Box::new(NullOutput::new(AudioFormat::INTERNAL, 10)),
    };
    let hub = HubEngine::start(HubConfig {
        settings: dev.settings(port),
        output,
        advertise: false,
    })
    .await
    .expect("hub start");
    let started = Instant::now();
    let events = hub.events();
    TestHub {
        hub,
        events,
        started,
    }
}

/// A started sender and its event receiver.
pub struct TestSender {
    pub sender: SenderHandle,
    pub events: broadcast::Receiver<SenderEvent>,
}

pub async fn start_sender(
    dev: &Device,
    hub_port: u16,
    freq_hz: f32,
    secret: Option<String>,
) -> TestSender {
    let (capture, warning) =
        hfa_core::sender::open_capture(&CaptureTarget::Tone { freq_hz }).expect("tone");
    assert!(warning.is_none());
    let sender = SenderEngine::start(SenderConfig {
        hub: HubAddress::Direct {
            host: "127.0.0.1".into(),
            port: hub_port,
        },
        settings: dev.settings(0),
        capture,
        label: format!("Tone {freq_hz}"),
        expected_hub_key: None,
        pairing_secret: secret,
    })
    .await
    .expect("sender start");
    let events = sender.events();
    TestSender { sender, events }
}

/// Waits for the first event matching `f` (ignores lagged events).
pub async fn wait_event<T: Clone, R>(
    rx: &mut broadcast::Receiver<T>,
    timeout: Duration,
    mut f: impl FnMut(&T) -> Option<R>,
) -> Option<R> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => return None,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => return None,
            Ok(Ok(ev)) => {
                if let Some(r) = f(&ev) {
                    return Some(r);
                }
            }
        }
    }
}

/// Waits until the hub lists a source; returns its stream id.
pub async fn wait_source_added(hub: &mut TestHub, timeout: Duration) -> u32 {
    wait_event(&mut hub.events, timeout, |ev| match ev {
        HubEvent::SourceAdded(info) => Some(info.stream_id),
        _ => None,
    })
    .await
    .expect("no SourceAdded")
}

/// Waits until the sender reports `state`.
pub async fn wait_state(sender: &mut TestSender, timeout: Duration, state: SenderState) {
    if sender.sender.status().state == state {
        return;
    }
    wait_event(&mut sender.events, timeout, |ev| match ev {
        SenderEvent::StateChanged(s) if *s == state => Some(()),
        _ => None,
    })
    .await
    .unwrap_or_else(|| {
        panic!(
            "sender never reached {state:?}: {:?}",
            sender.sender.status()
        )
    });
}

/// Left channel of a stereo WAV written by the hub.
pub fn read_left(path: &Path) -> Vec<f32> {
    let (format, samples) = hfa_audio::wav::read_wav(path).expect("read wav");
    assert_eq!(format, AudioFormat::INTERNAL);
    samples.chunks_exact(2).map(|f| f[0]).collect()
}

pub const RATE: f64 = 48_000.0;

/// Sample index of `t` seconds.
pub fn at(t: f64) -> usize {
    (t.max(0.0) * RATE) as usize
}

/// Median over 50 ms blocks of the amplitude of the `freq` component of `x` (a full-scale
/// sine gives 1.0). Blocks keep the measure robust to phase jumps (concealed or stretched
/// frames), which would cancel a single long Goertzel sum.
pub fn tone_amplitude(x: &[f32], freq: f64) -> f64 {
    let block = at(0.05);
    let mut amps: Vec<f64> = x.chunks_exact(block).map(|b| goertzel(b, freq)).collect();
    if amps.is_empty() {
        return 0.0;
    }
    amps.sort_by(f64::total_cmp);
    amps[amps.len() / 2]
}

/// Per-block amplitudes (debugging aid).
pub fn block_amplitudes(x: &[f32], freq: f64) -> Vec<f64> {
    x.chunks_exact(at(0.05))
        .map(|b| goertzel(b, freq))
        .collect()
}

/// Amplitude of the `freq` component of `x` (Goertzel; a full-scale sine gives 1.0).
pub fn goertzel(x: &[f32], freq: f64) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    let w = 2.0 * std::f64::consts::PI * freq / RATE;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = f64::from(v) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * power.max(0.0).sqrt() / x.len() as f64
}

/// Index of the first sample louder than `threshold`.
pub fn first_sound(x: &[f32], threshold: f32) -> Option<usize> {
    x.iter().position(|v| v.abs() > threshold)
}

/// Longest run of consecutive 5 ms blocks with RMS below `threshold`, in ms.
pub fn longest_dropout_ms(x: &[f32], threshold: f32) -> f64 {
    let block = at(0.005);
    let (mut run, mut longest) = (0usize, 0usize);
    for chunk in x.chunks(block) {
        let rms = (chunk.iter().map(|v| v * v).sum::<f32>() / chunk.len() as f32).sqrt();
        if rms < threshold {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest as f64 * 5.0
}

/// `x[at(from)..at(to)]`, clamped.
pub fn window(x: &[f32], from: f64, to: f64) -> &[f32] {
    let a = at(from).min(x.len());
    let b = at(to).min(x.len()).max(a);
    &x[a..b]
}
