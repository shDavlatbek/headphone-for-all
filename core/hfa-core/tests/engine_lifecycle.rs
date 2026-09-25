//! Engine life cycle over localhost: stream removal, refused untrusted senders, wrong PINs
//! and reconnection after a hub restart.

mod engine_common;

use std::time::Duration;

use engine_common::*;
use hfa_core::{HubEvent, SenderEvent, SenderState, TrustStore};

const START_TIMEOUT: Duration = Duration::from_secs(15);

/// A sender that stops sends `StreamStop` + `Bye`: the hub removes its source at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_stop_removes_the_source() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let mut hub = start_hub(&hub_dev, 0, Out::Null).await;
    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Phone");
    let mut sender = start_sender(&dev, hub.hub.local_port(), 440.0, Some(pin)).await;
    let stream_id = wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut sender, START_TIMEOUT, SenderState::Streaming).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (packets, _) = sender.sender.packets_sent();
    assert!(packets > 50, "only {packets} packets sent");

    sender.sender.stop().await;
    let removed = wait_event(&mut hub.events, Duration::from_secs(5), |ev| match ev {
        HubEvent::SourceRemoved { stream_id } => Some(*stream_id),
        _ => None,
    })
    .await;
    assert_eq!(removed, Some(stream_id));
    assert!(hub.hub.sources().is_empty());
    assert!(hub.hub.stream_counters(stream_id).is_none());
    hub.hub.stop().await;
}

/// Without a secret an unknown sender is refused before anything streams; with a wrong PIN
/// the hub reports a failed pairing. Neither ends up trusted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrusted_senders_are_refused() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let mut hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let port = hub.hub.local_port();

    // 1. No secret at all.
    let dev = Device::new("Stranger");
    let mut sender = start_sender(&dev, port, 440.0, None).await;
    let failed = wait_event(&mut sender.events, START_TIMEOUT, |ev| match ev {
        SenderEvent::StateChanged(SenderState::Failed(reason)) => Some(reason.clone()),
        _ => None,
    })
    .await
    .expect("the sender must give up");
    assert!(failed.contains("pairing required"), "{failed}");
    assert_eq!(sender.sender.packets_sent(), (0, 0), "nothing streamed");
    sender.sender.stop().await;

    // 2. A wrong PIN while a pairing window is open.
    let pin = hub.hub.start_pairing().pin;
    let wrong = if pin == "000000" { "111111" } else { "000000" };
    let dev2 = Device::new("Guesser");
    let mut guesser = start_sender(&dev2, port, 440.0, Some(wrong.into())).await;
    let hub_failed = wait_event(&mut hub.events, START_TIMEOUT, |ev| match ev {
        HubEvent::PairingFailed { reason } => Some(reason.clone()),
        HubEvent::SourceAdded(_) => panic!("an unpaired sender was added"),
        _ => None,
    })
    .await;
    assert!(hub_failed.is_some(), "the hub reports the failed pairing");
    let failed = wait_event(&mut guesser.events, START_TIMEOUT, |ev| match ev {
        SenderEvent::StateChanged(SenderState::Failed(reason)) => Some(reason.clone()),
        _ => None,
    })
    .await
    .expect("the guesser must give up");
    assert!(failed.contains("pairing failed"), "{failed}");
    assert_eq!(guesser.sender.packets_sent(), (0, 0));
    guesser.sender.stop().await;

    assert!(hub.hub.sources().is_empty());
    hub.hub.stop().await;
    let trust = TrustStore::load(hub_dev.path()).expect("trust");
    assert!(trust.peers().is_empty(), "nobody was trusted");
    let left = read_left(&wav);
    assert!(
        first_sound(&left, 1e-4).is_none(),
        "the output stayed silent"
    );
}

/// The hub restarts on the same port (same identity and trust store): the sender notices,
/// backs off, reconnects without a new pairing and streams a new stream. The restarted hub
/// forgot that it had muted the sender, and the sender's view follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_reconnects_after_hub_restart() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let mut hub = start_hub(&hub_dev, 0, Out::Null).await;
    let port = hub.hub.local_port();
    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Tablet");
    let mut sender = start_sender(&dev, port, 440.0, Some(pin)).await;
    let first_id = wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut sender, START_TIMEOUT, SenderState::Streaming).await;
    hub.hub.set_muted(first_id, true).expect("mute");
    let muted = wait_event(&mut sender.events, Duration::from_secs(5), |ev| match ev {
        SenderEvent::HubControl { muted, .. } => Some(*muted),
        _ => None,
    })
    .await;
    assert_eq!(muted, Some(true));
    tokio::time::sleep(Duration::from_millis(500)).await;

    hub.hub.stop().await;
    wait_state(
        &mut sender,
        Duration::from_secs(10),
        SenderState::Reconnecting,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut hub2 = start_hub(&hub_dev, port, Out::Null).await;
    assert_eq!(hub2.hub.local_port(), port);
    let second_id = wait_source_added(&mut hub2, Duration::from_secs(20)).await;
    assert_ne!(second_id, first_id, "a reconnect announces a new stream");
    let unmuted = wait_event(&mut sender.events, Duration::from_secs(10), |ev| match ev {
        SenderEvent::HubControl { muted, .. } => Some(*muted),
        _ => None,
    })
    .await;
    assert_eq!(unmuted, Some(false), "the new stream is not muted");
    assert!(!hub2.hub.sources()[0].muted);
    wait_state(&mut sender, Duration::from_secs(10), SenderState::Streaming).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let counters = hub2.hub.stream_counters(second_id).expect("counters");
    assert!(counters.played > 20, "{counters:?}");

    sender.sender.stop().await;
    hub2.hub.stop().await;
}

/// A second hub on a port that is already taken fails with a clear error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_taken_port_is_an_error() {
    let _serial = serial().await;
    let dev = Device::new("Hub");
    let hub = start_hub(&dev, 0, Out::Null).await;
    let dev2 = Device::new("Hub 2");
    let result = hfa_core::HubEngine::start(hfa_core::HubConfig {
        settings: dev2.settings(hub.hub.local_port()),
        output: Box::new(hfa_capture::output_file::NullOutput::new(
            hfa_audio::AudioFormat::INTERNAL,
            10,
        )),
        advertise: false,
    })
    .await;
    let err = result.expect_err("port in use");
    assert!(err.to_string().contains("already in use"), "{err}");
    hub.hub.stop().await;
}

/// A sender finds an advertised hub over mDNS by its device id and streams to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_discovers_the_hub_by_device_id() {
    let _serial = serial().await;
    let hub_dev = Device::new("Discoverable hub");
    let hub = hfa_core::HubEngine::start(hfa_core::HubConfig {
        settings: hub_dev.settings(0),
        output: Box::new(hfa_capture::output_file::NullOutput::new(
            hfa_audio::AudioFormat::INTERNAL,
            10,
        )),
        advertise: true,
    })
    .await
    .expect("hub");
    let mut events = hub.events();
    let pin = hub.start_pairing().pin;
    let dev = Device::new("Finder");
    let (capture, _) =
        hfa_core::sender::open_capture(&hfa_capture::CaptureTarget::Tone { freq_hz: 440.0 })
            .expect("tone");
    let sender = hfa_core::SenderEngine::start(hfa_core::SenderConfig {
        hub: hfa_core::HubAddress::Discover {
            name_or_id: hub.device_id().to_owned(),
        },
        settings: dev.settings(0),
        capture,
        label: "Found".into(),
        expected_hub_key: None,
        pairing_secret: Some(pin),
    })
    .await
    .expect("sender");
    let added = wait_event(&mut events, Duration::from_secs(40), |ev| match ev {
        HubEvent::SourceAdded(info) => Some(info.device_name.clone()),
        _ => None,
    })
    .await;
    assert_eq!(added.as_deref(), Some("Finder"), "{:?}", sender.status());
    sender.stop().await;
    hub.stop().await;
}
