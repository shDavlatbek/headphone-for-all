//! A platform discovery backend (as iOS registers for Bonjour) replaces `mdns-sd` for
//! browsing and advertising. Own test binary: the backend is process-wide.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use hfa_core::discovery::{
    browse, set_platform_backend, Advertiser, DiscoveryEvent, DiscoveryFeed, DiscoveryGuard,
    PlatformDiscovery, ServiceAdvert,
};
use parking_lot::Mutex;

/// Sets a flag when dropped.
struct Guard(Arc<AtomicBool>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Fake {
    feed: Mutex<Option<DiscoveryFeed>>,
    adverts: Mutex<Vec<ServiceAdvert>>,
    browse_stopped: Arc<AtomicBool>,
    advert_stopped: Arc<AtomicBool>,
}

impl PlatformDiscovery for Fake {
    fn browse(&self, feed: DiscoveryFeed) -> hfa_core::Result<DiscoveryGuard> {
        *self.feed.lock() = Some(feed);
        Ok(Box::new(Guard(Arc::clone(&self.browse_stopped))))
    }

    fn advertise(&self, service: &ServiceAdvert) -> hfa_core::Result<DiscoveryGuard> {
        self.adverts.lock().push(service.clone());
        Ok(Box::new(Guard(Arc::clone(&self.advert_stopped))))
    }
}

fn txt(id: &str, name: &str) -> Vec<(String, String)> {
    [("v", "0"), ("id", id), ("name", name), ("platform", "ios")]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[tokio::test]
async fn platform_backend_replaces_mdns() {
    let fake = Arc::new(Fake::default());
    set_platform_backend(Some(fake.clone()));

    // Advertising goes through the backend with the same data mdns-sd would get.
    let id = hfa_proto::fingerprint(&[3; 32]);
    let advertiser = Advertiser::start("Phone", &id, 47_810, "ios").expect("advertise");
    let advert = fake.adverts.lock()[0].clone();
    assert_eq!(advert.instance, format!("Phone ({})", &id[..4]));
    assert_eq!(advert.service_type, hfa_proto::SERVICE_TYPE);
    assert_eq!(advert.port, 47_810);
    assert_eq!(advert.txt, txt(&id, "Phone"));
    assert_eq!(
        advertiser.fullname(),
        format!("Phone ({})._hfa._tcp.local.", &id[..4])
    );
    advertiser.stop().expect("stop");
    assert!(fake.advert_stopped.load(Ordering::SeqCst));

    // Browsing: results fed by the backend arrive as DiscoveryEvents, validated.
    let mut browser = browse().expect("browse");
    let feed = fake.feed.lock().clone().expect("browse started");
    let v4: IpAddr = "192.168.1.20".parse().expect("ip");
    let v6: IpAddr = "fd00::20".parse().expect("ip");
    let link_local: IpAddr = "fe80::1".parse().expect("ip");
    assert!(feed.resolved("bad", &txt("not-an-id", "X"), &[v4], 1));
    assert!(feed.resolved("a", &txt(&id, "Desk\u{7}"), &[v6, link_local, v4], 47_810));
    assert!(feed.resolved("b", &txt(&id, "Desk"), &[v4], 47_810));
    match browser.recv().await {
        Some(DiscoveryEvent::Found(info)) => {
            assert_eq!(info.device_id, id);
            assert_eq!(info.addrs, vec![v4, v6], "IPv4 first, link-local dropped");
            assert_eq!(info.port, 47_810);
            assert_eq!(info.platform, "ios");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        browser.recv().await,
        Some(DiscoveryEvent::Found(_))
    ));
    // Lost only once the last instance of the id is gone.
    assert!(feed.removed("a"));
    assert!(feed.removed("unknown"));
    assert!(browser.try_recv().is_none());
    assert!(feed.removed("b"));
    assert_eq!(browser.recv().await, Some(DiscoveryEvent::Lost(id.clone())));

    drop(browser);
    assert!(feed.is_closed());
    assert!(fake.browse_stopped.load(Ordering::SeqCst));
    assert!(!feed.resolved("c", &txt(&id, "Desk"), &[v4], 47_810));

    set_platform_backend(None);
}
