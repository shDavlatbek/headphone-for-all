//! The hub's control protocol over localhost: `StreamStart` validation (every rejection
//! reason), controls announced with each new stream and remembered per device across
//! reconnects, and the limits on connections that are still in the handshake.

mod engine_common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use engine_common::*;
use hfa_core::control::ControlChannel;
use hfa_core::hub::{FIRST_MESSAGE_TIMEOUT, MAX_PENDING_PER_IP, MAX_STREAMS};
use hfa_core::{HubEvent, Identity, SenderEvent, SenderState, TrustStore};
use hfa_proto::control::{Body, StreamStart, StreamStop};
use hfa_proto::ControlMessage;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

const START_TIMEOUT: Duration = Duration::from_secs(15);

/// What the hub answered to one `StreamStart`.
#[derive(Debug)]
enum Answer {
    /// Accepted, with the controls announced before `StreamAccepted`.
    Accepted {
        gain: Option<f32>,
        muted: Option<bool>,
        priority: Option<bool>,
    },
    Rejected(String),
}

fn stream_start(stream_id: u32) -> StreamStart {
    StreamStart {
        stream_id,
        sample_rate: 48_000,
        channels: 2,
        frame_ms: 10,
        bitrate: 128_000,
        label: "Probe".into(),
        media_key: vec![7; 32],
    }
}

/// Sends `ss` and collects the hub's answer for its stream id.
async fn request(ch: &mut ControlChannel, ss: StreamStart) -> Answer {
    let id = ss.stream_id;
    ch.send(&ControlMessage::new(Body::StreamStart(ss)))
        .await
        .expect("send");
    let (mut gain, mut muted, mut priority) = (None, None, None);
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ch.recv())
            .await
            .expect("an answer in time")
            .expect("recv");
        match msg.body {
            Some(Body::SetVolume(v)) if v.stream_id == id => gain = Some(v.gain),
            Some(Body::SetMute(m)) if m.stream_id == id => muted = Some(m.muted),
            Some(Body::SetPriority(p)) if p.stream_id == id => priority = Some(p.priority),
            Some(Body::StreamAccepted(a)) if a.stream_id == id => {
                return Answer::Accepted {
                    gain,
                    muted,
                    priority,
                }
            }
            Some(Body::StreamRejected(r)) if r.stream_id == id => {
                return Answer::Rejected(r.reason)
            }
            _ => {}
        }
    }
}

fn assert_rejected(answer: Answer, needle: &str) {
    match answer {
        Answer::Rejected(reason) => assert!(reason.contains(needle), "{reason}"),
        other => panic!("expected a rejection containing {needle:?}, got {other:?}"),
    }
}

/// Every invalid `StreamStart` is answered with `StreamRejected` and registers nothing; a
/// valid one is accepted with its controls announced first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_stream_starts_are_rejected() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let mut hub = start_hub(&hub_dev, 0, Out::Null).await;
    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Probe");
    let identity = Identity::load_or_create(dev.path(), "Probe").expect("identity");
    let trust = TrustStore::load(dev.path()).expect("trust");
    let addr: SocketAddr = ([127, 0, 0, 1], hub.hub.local_port()).into();
    let (mut ch, _) = ControlChannel::connect(addr, &identity, &trust, None, Some(pin))
        .await
        .expect("connect");

    let mut ss = stream_start(1);
    ss.sample_rate = 44_100;
    assert_rejected(request(&mut ch, ss).await, "48000 Hz stereo");
    let mut ss = stream_start(1);
    ss.channels = 1;
    assert_rejected(request(&mut ch, ss).await, "48000 Hz stereo");
    for frame_ms in [0, 5, 40] {
        let mut ss = stream_start(1);
        ss.frame_ms = frame_ms;
        assert_rejected(request(&mut ch, ss).await, "frame size");
    }
    let mut ss = stream_start(1);
    ss.media_key = vec![7; 16];
    assert_rejected(request(&mut ch, ss).await, "media key");
    assert!(hub.hub.sources().is_empty(), "nothing was registered");

    // A valid stream: accepted, with the default controls announced before.
    match request(&mut ch, stream_start(1)).await {
        Answer::Accepted {
            gain,
            muted,
            priority,
        } => assert_eq!(
            (gain, muted, priority),
            (Some(1.0), Some(false), Some(false))
        ),
        other => panic!("{other:?}"),
    }
    wait_source_added(&mut hub, START_TIMEOUT).await;
    assert_rejected(request(&mut ch, stream_start(1)).await, "already in use");

    // Up to MAX_STREAMS streams, then no more.
    for id in 2..=MAX_STREAMS as u32 {
        assert!(
            matches!(
                request(&mut ch, stream_start(id)).await,
                Answer::Accepted { .. }
            ),
            "stream {id}"
        );
    }
    assert_rejected(
        request(&mut ch, stream_start(MAX_STREAMS as u32 + 1)).await,
        "already mixes",
    );
    assert_eq!(hub.hub.sources().len(), MAX_STREAMS);

    // A stop frees a slot.
    ch.send(&ControlMessage::new(Body::StreamStop(StreamStop {
        stream_id: 1,
    })))
    .await
    .expect("stop");
    assert!(matches!(
        request(&mut ch, stream_start(100)).await,
        Answer::Accepted { .. }
    ));
    let ids: Vec<u32> = hub.hub.sources().iter().map(|s| s.stream_id).collect();
    assert_eq!(ids.len(), MAX_STREAMS);
    assert!(!ids.contains(&1) && ids.contains(&100));

    let _ = ch.close("done").await;
    hub.hub.stop().await;
}

/// Controls set on the hub are remembered for the device: when it comes back with a new
/// stream, the source, the mix and the sender's view all carry them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controls_are_remembered_per_device() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let mut hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let port = hub.hub.local_port();
    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Laptop");
    let mut first = start_sender(&dev, port, 440.0, Some(pin)).await;
    let first_id = wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut first, START_TIMEOUT, SenderState::Streaming).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let t_full = hub.started.elapsed().as_secs_f64();

    hub.hub.set_gain(first_id, 0.5).expect("gain");
    hub.hub.set_priority(first_id, true).expect("priority");
    let seen = wait_event(&mut first.events, Duration::from_secs(5), |ev| match ev {
        SenderEvent::HubControl {
            priority: true,
            gain,
            ..
        } => Some(*gain),
        _ => None,
    })
    .await;
    assert_eq!(seen, Some(0.5));
    first.sender.stop().await;
    wait_event(&mut hub.events, Duration::from_secs(5), |ev| match ev {
        HubEvent::SourceRemoved { stream_id } if *stream_id == first_id => Some(()),
        _ => None,
    })
    .await
    .expect("removed");

    // The same device (same identity, already paired) comes back.
    let mut second = start_sender(&dev, port, 440.0, None).await;
    let info = wait_event(&mut hub.events, START_TIMEOUT, |ev| match ev {
        HubEvent::SourceAdded(info) => Some(info.clone()),
        _ => None,
    })
    .await
    .expect("SourceAdded");
    let t_back = hub.started.elapsed().as_secs_f64();
    assert_ne!(info.stream_id, first_id);
    assert_eq!((info.gain, info.muted, info.priority), (0.5, false, true));
    let control = wait_event(&mut second.events, Duration::from_secs(5), |ev| match ev {
        SenderEvent::HubControl {
            gain,
            muted,
            priority,
        } => Some((*gain, *muted, *priority)),
        _ => None,
    })
    .await;
    assert_eq!(
        control,
        Some((0.5, false, true)),
        "the sender is told at once"
    );
    wait_state(&mut second, START_TIMEOUT, SenderState::Streaming).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let t_end = hub.started.elapsed().as_secs_f64();
    second.sender.stop().await;
    hub.hub.stop().await;

    let left = read_left(&wav);
    let full = tone_amplitude(window(&left, t_full - 1.0, t_full - 0.1), 440.0);
    let back = tone_amplitude(window(&left, t_back + 0.5, t_end - 0.1), 440.0);
    assert!(full > 0.15, "{full}");
    let ratio = back / full;
    assert!(
        (0.35..0.7).contains(&ratio),
        "the remembered gain 0.5 scaled the tone by {ratio}"
    );
}

/// Connections that do not speak are refused beyond a few per address and closed after
/// `FIRST_MESSAGE_TIMEOUT`; after that a real sender connects normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_connections_cannot_lock_senders_out() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let mut hub = start_hub(&hub_dev, 0, Out::Null).await;
    let addr: SocketAddr = ([127, 0, 0, 1], hub.hub.local_port()).into();

    let mut silent = Vec::new();
    for _ in 0..MAX_PENDING_PER_IP {
        silent.push(TcpStream::connect(addr).await.expect("connect"));
    }
    // Give the accept loop time to register them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut extra = TcpStream::connect(addr).await.expect("connect");
    let mut byte = [0u8; 1];
    let refused = tokio::time::timeout(Duration::from_secs(2), extra.read(&mut byte)).await;
    assert!(
        matches!(refused, Ok(Ok(0)) | Ok(Err(_))),
        "one more silent connection from the same address is closed at once: {refused:?}"
    );

    // The silent ones are closed once their first-message deadline passes.
    let started = Instant::now();
    for mut conn in silent {
        let closed = tokio::time::timeout(
            FIRST_MESSAGE_TIMEOUT + Duration::from_secs(5),
            conn.read(&mut byte),
        )
        .await;
        assert!(
            matches!(closed, Ok(Ok(0)) | Ok(Err(_))),
            "a silent connection is closed: {closed:?}"
        );
    }
    assert!(started.elapsed() < FIRST_MESSAGE_TIMEOUT + Duration::from_secs(5));

    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Phone");
    let mut sender = start_sender(&dev, hub.hub.local_port(), 440.0, Some(pin)).await;
    wait_source_added(&mut hub, START_TIMEOUT).await;
    wait_state(&mut sender, START_TIMEOUT, SenderState::Streaming).await;
    sender.sender.stop().await;
    hub.hub.stop().await;
}

/// A sender removed from the hub's trusted devices while connected (the app's "forget",
/// through its own `TrustStore` handle) is disconnected at once, and the hub does not accept
/// it again without a new pairing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgetting_a_connected_sender_disconnects_it() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let mut hub = start_hub(&hub_dev, 0, Out::Null).await;
    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Probe");
    let identity = Identity::load_or_create(dev.path(), "Probe").expect("identity");
    let trust = TrustStore::load(dev.path()).expect("trust");
    let addr: SocketAddr = ([127, 0, 0, 1], hub.hub.local_port()).into();
    let (mut ch, _) = ControlChannel::connect(addr, &identity, &trust, None, Some(pin))
        .await
        .expect("connect");
    assert!(matches!(
        request(&mut ch, stream_start(1)).await,
        Answer::Accepted { .. }
    ));
    wait_source_added(&mut hub, START_TIMEOUT).await;

    let settings_trust = TrustStore::load(hub_dev.path()).expect("hub trust");
    let id = identity.device_id.clone();
    tokio::task::spawn_blocking(move || settings_trust.remove(&id))
        .await
        .expect("join")
        .expect("forget");

    let bye = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ch.recv().await {
                Ok(msg) => {
                    if let Some(Body::Bye(b)) = msg.body {
                        break Some(b.reason);
                    }
                }
                Err(_) => break None,
            }
        }
    })
    .await
    .expect("the hub disconnects the forgotten sender");
    assert!(
        bye.as_deref().is_some_and(|r| r.contains("removed")),
        "{bye:?}"
    );
    let removed = wait_event(&mut hub.events, Duration::from_secs(5), |e| match e {
        HubEvent::SourceRemoved { .. } => Some(()),
        _ => None,
    })
    .await;
    assert!(
        removed.is_some(),
        "the forgotten sender's stream is removed"
    );

    // Reconnecting without a secret is refused now.
    let again = ControlChannel::connect(addr, &identity, &trust, None, None).await;
    assert!(
        matches!(again, Err(hfa_core::CoreError::PairingRequired)),
        "{again:?}"
    );
    hub.hub.stop().await;
}
