//! mDNS advertise + browse in one process (multicast on the container's interfaces,
//! looped back to ourselves).

use std::time::Duration;

use hfa_core::discovery::{browse, Advertiser, DiscoveryEvent};

/// A random, valid device id so parallel test runs never see each other's hubs.
fn random_id() -> String {
    hfa_proto::fingerprint(&rand::random::<[u8; 32]>())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertise_and_browse() {
    let id = random_id();
    let mut browser = browse().expect("browse");
    let advertiser =
        Advertiser::start("Test Hub.local\\x", &id, 47_999, "linux").expect("advertise");
    assert!(
        advertiser.fullname().contains(&id[..4]),
        "{}",
        advertiser.fullname()
    );
    assert!(advertiser.fullname().ends_with("._hfa._tcp.local."));

    let found = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match browser.recv().await {
                Some(DiscoveryEvent::Found(info)) if info.device_id == id => return info,
                Some(_) => {}
                None => panic!("browser stopped"),
            }
        }
    })
    .await
    .expect("hub found within 15 s");
    assert_eq!(found.name, "Test Hub.local\\x");
    assert_eq!(found.port, 47_999);
    assert_eq!(found.platform, "linux");
    assert!(!found.addrs.is_empty());
    // The hub listens on IPv4 only, so only IPv4 addresses are advertised.
    assert!(found.addrs.iter().all(|a| a.is_ipv4()), "{:?}", found.addrs);

    advertiser.stop().expect("stop");
    let lost = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match browser.recv().await {
                Some(DiscoveryEvent::Lost(lost)) if lost == id => return,
                Some(_) => {}
                None => panic!("browser stopped"),
            }
        }
    })
    .await;
    assert!(lost.is_ok(), "hub lost after stop()");

    // Dropping the browser ends the stream of events.
    drop(browser);
}

#[test]
fn invalid_advertisements_are_refused() {
    assert!(Advertiser::start("Hub", "ab12-cd34-ef56-7890", 0, "linux").is_err());
    assert!(Advertiser::start("Hub", "", 47810, "linux").is_err());
}
