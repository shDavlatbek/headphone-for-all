//! Streaming through a lossy, reordering UDP relay: the tone survives with bounded glitches
//! and the redundant copies recover most single losses.

mod engine_common;

use std::time::Duration;

use engine_common::*;
use hfa_core::netsim::{ImpairConfig, UdpImpairProxy};
use hfa_core::SenderState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redundancy_recovers_losses_through_a_lossy_link() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let wav = hub_dev.path().join("mix.wav");
    let mut hub = start_hub(&hub_dev, 0, Out::Wav(wav.clone())).await;
    let port = hub.hub.local_port();
    let proxy = UdpImpairProxy::start(
        ([127, 0, 0, 1], port).into(),
        ImpairConfig {
            loss_pct: 5.0,
            jitter_ms: 8,
            seed: 42,
        },
    )
    .await
    .expect("proxy");
    hub.hub
        .set_media_port_override(Some(proxy.local_addr().port()));

    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Wi-Fi laptop");
    let mut sender = start_sender(&dev, port, 440.0, Some(pin)).await;
    let stream_id = wait_source_added(&mut hub, Duration::from_secs(15)).await;
    wait_state(&mut sender, Duration::from_secs(15), SenderState::Streaming).await;

    // The first Stats (after 1 s) report ~5 % loss, which switches redundancy on.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let t_mid = hub.started.elapsed().as_secs_f64();
    let mid = hub.hub.stream_counters(stream_id).expect("counters");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let t_end = hub.started.elapsed().as_secs_f64();
    let end = hub.hub.stream_counters(stream_id).expect("counters");
    let status = sender.sender.status();
    let proxy_stats = proxy.stats();

    sender.sender.stop().await;
    hub.hub.stop().await;
    proxy.stop().await;

    eprintln!("proxy {proxy_stats:?}\nmid {mid:?}\nend {end:?}\nsender {status:?}");
    let drop_pct = proxy_stats.dropped as f64 * 100.0 / proxy_stats.received.max(1) as f64;
    assert!((2.5..8.0).contains(&drop_pct), "proxy dropped {drop_pct} %");
    assert!(status.loss_pct > 0.5, "the sender saw the loss: {status:?}");

    let lost = end.lost - mid.lost;
    let recovered = end.recovered_redundancy - mid.recovered_redundancy;
    assert!(lost >= 5, "too few losses to judge: {mid:?} → {end:?}");
    assert!(
        recovered * 2 >= lost,
        "redundancy recovered only {recovered} of {lost} lost frames ({mid:?} → {end:?})"
    );

    let left = read_left(&wav);
    let seg = window(&left, t_mid, t_end - 0.1);
    let a440 = tone_amplitude(seg, 440.0);
    assert!(a440 > 0.15, "440 Hz amplitude {a440}");
    let dropout = longest_dropout_ms(seg, 0.02);
    assert!(dropout < 100.0, "dropout of {dropout} ms");
}

/// UDP is blocked (the TCP control connection works): the hub reports that nothing arrives
/// (`NO_MEDIA_LOSS_PCT`) instead of "0 % loss", and the sender says what is wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_udp_is_reported_to_the_sender() {
    let _serial = serial().await;
    let hub_dev = Device::new("Hub");
    let mut hub = start_hub(&hub_dev, 0, Out::Null).await;
    let port = hub.hub.local_port();
    // A socket that swallows the media: the hub never sees a datagram.
    let black_hole = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    hub.hub
        .set_media_port_override(Some(black_hole.local_addr().expect("addr").port()));

    let pin = hub.hub.start_pairing().pin;
    let dev = Device::new("Firewalled laptop");
    let mut sender = start_sender(&dev, port, 440.0, Some(pin)).await;
    wait_source_added(&mut hub, Duration::from_secs(15)).await;
    wait_state(&mut sender, Duration::from_secs(15), SenderState::Streaming).await;

    let error = wait_event(&mut sender.events, Duration::from_secs(10), |ev| match ev {
        hfa_core::SenderEvent::Error(m) if m.contains("receives no audio") => Some(m.clone()),
        _ => None,
    })
    .await;
    let status = sender.sender.status();
    sender.sender.stop().await;
    hub.hub.stop().await;
    let error = error.expect("the sender reports that the hub receives nothing");
    assert!(error.contains("UDP port"), "{error}");
    assert_eq!(
        status.loss_pct,
        hfa_core::hub::NO_MEDIA_LOSS_PCT,
        "{status:?}"
    );
}
