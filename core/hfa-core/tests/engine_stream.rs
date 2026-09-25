//! End-to-end streaming over localhost without audio hardware: tone senders → hub → WAV
//! file, checked with a Goertzel detector.

mod engine_common;

use std::time::Duration;

use engine_common::*;
use hfa_core::{HubEvent, SenderEvent, SenderState};

const START_TIMEOUT: Duration = Duration::from_secs(15);

/// One 440 Hz sender paired with a PIN: the tone arrives, without dropouts after warm-up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paired_tone_sender_reaches_the_wav() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let mut hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let pin = hub.hub.start_pairing().pin;

    let sender_dev = Device::new("Laptop");
    let mut sender = start_sender(&sender_dev, hub.hub.local_port(), 440.0, Some(pin)).await;
    let paired = wait_event(&mut hub.events, START_TIMEOUT, |ev| match ev {
        HubEvent::PairingCompleted { name, .. } => Some(name.clone()),
        _ => None,
    })
    .await;
    assert_eq!(paired.as_deref(), Some("Laptop"));
    let stream_id = wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut sender, START_TIMEOUT, SenderState::Streaming).await;

    tokio::time::sleep(Duration::from_secs(3)).await;

    let sources = hub.hub.sources();
    assert_eq!(sources.len(), 1);
    let info = &sources[0];
    assert_eq!(info.stream_id, stream_id);
    assert_eq!(info.device_name, "Laptop");
    assert_eq!(info.label, "Tone 440");
    assert!(info.active);
    assert!(info.stats.level_db > -20.0, "{:?}", info.stats);
    assert!(
        info.stats.latency_ms > 0.0 && info.stats.latency_ms < 250.0,
        "{:?}",
        info.stats
    );
    let status = sender.sender.status();
    assert_eq!(status.state, SenderState::Streaming);
    assert!(status.level_db > -20.0, "{status:?}");
    let counters = hub.hub.stream_counters(stream_id).expect("counters");
    assert!(counters.played > 200, "{counters:?}");
    assert_eq!(counters.lost, 0, "no loss on localhost: {counters:?}");

    sender.sender.stop().await;
    hub.hub.stop().await;

    let left = read_left(&wav);
    let start = first_sound(&left, 0.02).expect("the tone never reached the WAV");
    // Analyse from 300 ms after the first sound until 2.5 s later.
    let from = start as f64 / RATE + 0.3;
    let seg = window(&left, from, from + 2.5);
    assert!(seg.len() > at(2.0), "WAV too short: {} samples", left.len());
    let a440 = tone_amplitude(seg, 440.0);
    let a1000 = tone_amplitude(seg, 1000.0);
    assert!(a440 > 0.15, "440 Hz amplitude {a440}");
    assert!(a1000 < 0.01, "1000 Hz amplitude {a1000}");
    let dropout = longest_dropout_ms(seg, 0.02);
    assert!(dropout < 60.0, "dropout of {dropout} ms");
}

/// Two senders at once: both tones are in the mix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_senders_are_mixed() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let port = hub.hub.local_port();

    let dev_a = Device::new("A");
    let pin = hub.hub.start_pairing().pin;
    let mut a = start_sender(&dev_a, port, 440.0, Some(pin)).await;
    wait_state(&mut a, START_TIMEOUT, SenderState::Streaming).await;
    // A pairing window is single-use: open a new one for the second device.
    let dev_b = Device::new("B");
    let pin = hub.hub.start_pairing().pin;
    let mut b = start_sender(&dev_b, port, 1000.0, Some(pin)).await;
    wait_state(&mut b, START_TIMEOUT, SenderState::Streaming).await;
    let t_both = hub.started.elapsed().as_secs_f64();

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(hub.hub.sources().len(), 2);
    a.sender.stop().await;
    b.sender.stop().await;
    hub.hub.stop().await;

    let left = read_left(&wav);
    let seg = window(&left, t_both + 0.5, t_both + 2.5);
    let a440 = tone_amplitude(seg, 440.0);
    let a1000 = tone_amplitude(seg, 1000.0);
    assert!(a440 > 0.15, "440 Hz amplitude {a440}");
    assert!(a1000 > 0.15, "1000 Hz amplitude {a1000}");
    let dropout = longest_dropout_ms(seg, 0.02);
    assert!(dropout < 60.0, "dropout of {dropout} ms");
}

/// Hub controls: muting removes a sender's tone, a gain change scales it, and the sender is
/// told about both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mute_and_gain_change_the_mix() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let mut hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let port = hub.hub.local_port();

    let dev_a = Device::new("A");
    let pin = hub.hub.start_pairing().pin;
    let mut a = start_sender(&dev_a, port, 440.0, Some(pin)).await;
    let id_a = wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut a, START_TIMEOUT, SenderState::Streaming).await;
    let dev_b = Device::new("B");
    let pin = hub.hub.start_pairing().pin;
    let mut b = start_sender(&dev_b, port, 1000.0, Some(pin)).await;
    let id_b = wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut b, START_TIMEOUT, SenderState::Streaming).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let t_mute = hub.started.elapsed().as_secs_f64();
    hub.hub.set_muted(id_a, true).expect("mute");
    let control = wait_event(&mut a.events, Duration::from_secs(5), |ev| match ev {
        SenderEvent::HubControl { muted, .. } => Some(*muted),
        _ => None,
    })
    .await;
    assert_eq!(control, Some(true), "the sender learns it was muted");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let t_gain = hub.started.elapsed().as_secs_f64();
    hub.hub.set_gain(id_b, 0.25).expect("gain");
    let gain = wait_event(&mut b.events, Duration::from_secs(5), |ev| match ev {
        SenderEvent::HubControl { gain, .. } => Some(*gain),
        _ => None,
    })
    .await;
    assert_eq!(gain, Some(0.25));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let t_end = hub.started.elapsed().as_secs_f64();

    let sources = hub.hub.sources();
    let src_a = sources.iter().find(|s| s.stream_id == id_a).expect("a");
    let src_b = sources.iter().find(|s| s.stream_id == id_b).expect("b");
    assert!(src_a.muted && src_a.gain == 1.0);
    assert!(!src_b.muted && src_b.gain == 0.25);
    assert!(hub.hub.set_gain(12345, 1.0).is_err(), "unknown stream");

    a.sender.stop().await;
    b.sender.stop().await;
    hub.hub.stop().await;

    let left = read_left(&wav);
    let before = window(&left, t_mute - 1.0, t_mute - 0.1);
    let muted = window(&left, t_mute + 0.4, t_gain - 0.1);
    let quieter = window(&left, t_gain + 0.4, t_end - 0.1);
    let (a_before, b_before) = (
        tone_amplitude(before, 440.0),
        tone_amplitude(before, 1000.0),
    );
    let (a_muted, b_muted) = (tone_amplitude(muted, 440.0), tone_amplitude(muted, 1000.0));
    let b_quiet = tone_amplitude(quieter, 1000.0);
    assert!(a_before > 0.15 && b_before > 0.15, "{a_before} {b_before}");
    assert!(a_muted < 0.01, "440 Hz still audible after mute: {a_muted}");
    assert!(b_muted > 0.15, "1000 Hz lost: {b_muted}");
    let ratio = b_quiet / b_muted;
    assert!(
        (0.15..0.4).contains(&ratio),
        "gain 0.25 changed the level by {ratio}"
    );
}
