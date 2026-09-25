//! Silence handling end to end: a 44.1 kHz mono external feed (converted and resampled by the
//! sender) plays a tone, then silence, then nothing at all (a stalled capture), then the tone
//! again. The sender switches to DTX keep-alives, the hub keeps the source active without
//! counting loss, and the tone resumes at once.

mod engine_common;

use std::time::{Duration, Instant};

use engine_common::*;
use hfa_audio::{AudioFormat, SineGenerator};
use hfa_capture::CaptureTarget;
use hfa_core::{HubAddress, SenderConfig, SenderEngine, SenderState};

const FEED_ID: u32 = 0x00df_0001;

/// Pushes `seconds` of `tone` (or zeros) into the feed at real-time pace.
fn push_for(feed: &hfa_capture::ExternalFeed, tone: Option<&mut SineGenerator>, seconds: f64) {
    let format = feed.format();
    let block = format.samples_for_ms(10);
    let mut buf = vec![0.0f32; block];
    let blocks = (seconds * 100.0) as u32;
    let start = Instant::now();
    let mut tone = tone;
    for i in 0..blocks {
        match tone.as_deref_mut() {
            Some(t) => t.fill(&mut buf),
            None => buf.fill(0.0),
        }
        feed.push(&buf);
        let due = start + Duration::from_millis(10 * u64::from(i + 1));
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silence_and_stalls_become_dtx_and_audio_resumes() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let mut hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let pin = hub.hub.start_pairing().pin;

    let format = AudioFormat::new(44_100, 1);
    let feed = hfa_capture::register_external(FEED_ID, format);
    let (capture, _) =
        hfa_core::sender::open_capture(&CaptureTarget::External { id: FEED_ID }).expect("feed");
    let dev = Device::new("Phone");
    let sender = SenderEngine::start(SenderConfig {
        hub: HubAddress::Direct {
            host: "localhost".into(),
            port: hub.hub.local_port(),
        },
        settings: dev.settings(0),
        capture,
        label: "Phone audio".into(),
        expected_hub_key: None,
        pairing_secret: Some(pin),
    })
    .await
    .expect("sender");
    let mut sender = TestSender {
        events: sender.events(),
        sender,
    };
    let stream_id = wait_source_added(&mut hub, Duration::from_secs(15)).await;
    wait_state(&mut sender, Duration::from_secs(15), SenderState::Streaming).await;

    let t0 = hub.started.elapsed().as_secs_f64();
    let pusher_feed = feed.clone();
    let pusher = std::thread::spawn(move || {
        let mut tone = SineGenerator::new(440.0, 0.25, format);
        push_for(&pusher_feed, Some(&mut tone), 1.0);
        push_for(&pusher_feed, None, 1.0);
        std::thread::sleep(Duration::from_secs(1)); // stalled capture
        push_for(&pusher_feed, Some(&mut tone), 1.5);
    });
    // During the stall: still listed and active (keep-alives count as traffic).
    tokio::time::sleep(Duration::from_millis(2700)).await;
    let during = hub.hub.stream_counters(stream_id).expect("counters");
    let source = hub.hub.sources().into_iter().next().expect("source");
    assert!(source.active, "keep-alives keep the source active");
    tokio::task::spawn_blocking(move || pusher.join().expect("pusher"))
        .await
        .expect("join");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let end = hub.hub.stream_counters(stream_id).expect("counters");
    let (audio, keepalives) = sender.sender.packets_sent();

    sender.sender.stop().await;
    hub.hub.stop().await;
    hfa_capture::unregister_external(FEED_ID);

    // ~1.8 s of silence/stall minus the 200 ms DTX delay, one keep-alive per 100 ms.
    assert!(
        keepalives >= 10,
        "only {keepalives} keep-alives ({audio} audio packets)"
    );
    assert!(during.keepalives >= 5, "{during:?}");
    assert!(end.resets >= 1, "audio resumed with a reset: {end:?}");
    assert_eq!(end.lost, 0, "DTX is not loss: {end:?}");
    assert_eq!(during.underruns, 0, "DTX is not an underrun: {during:?}");
    // The final stop of the feed is a real gap until DTX starts 200 ms later.
    assert!(end.underruns <= 1, "{end:?}");
    // 1 s + 1.5 s of tone plus ~200 ms of silence before DTX starts, at 100 packets/s.
    assert!((200..=320).contains(&audio), "{audio} audio packets");

    let left = read_left(&wav);
    let first = window(&left, t0 + 0.3, t0 + 0.9);
    let silent = window(&left, t0 + 1.5, t0 + 2.8);
    let resumed = window(&left, t0 + 3.4, t0 + 4.3);
    let (a1, a2, a3) = (
        tone_amplitude(first, 440.0),
        tone_amplitude(silent, 440.0),
        tone_amplitude(resumed, 440.0),
    );
    assert!(a1 > 0.15, "first tone {a1}");
    assert!(a2 < 0.005, "silence {a2}");
    assert!(a3 > 0.15, "resumed tone {a3}");
}
