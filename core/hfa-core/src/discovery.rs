//! mDNS / DNS-SD discovery of hubs (`_hfa._tcp.local.`, via `mdns-sd`).
//!
//! - **Advertising** ([`Advertiser`]): one service instance per hub. The instance name is the
//!   display name plus a short device-id suffix (`Desk (ab12)`), so two hubs with the same
//!   name never collide; the mDNS host name is `hfa-<device id>.local.`. The addresses follow
//!   the host's interfaces automatically, **IPv4 only**: the hub listens on IPv4 only, so an
//!   advertised IPv6 address could never be reached. TXT records: `v` (protocol version),
//!   `id` (device id), `name`, `platform`.
//! - **Browsing** ([`browse`]): a background thread turns `mdns-sd` events into
//!   [`DiscoveryEvent`]s on a tokio channel. Only resolved instances with `v=0` and a
//!   well-formed `id` are reported. `Found` repeats when a hub's data changes; `Lost(id)` is
//!   sent when the last instance of that device id disappears.
//!
//! mDNS is unauthenticated: a `HubInfo` is only a hint. The control channel
//! authenticates the hub (`expected_hub_key`, trust store, pairing).
//!
//! Stopping an [`Advertiser`] (or dropping it) unregisters the instance (goodbye packets)
//! and shuts its daemon down; dropping a [`Browser`] stops browsing and shuts its daemon
//! down. Neither blocks.
//!
//! # Platform backends (iOS)
//!
//! `mdns-sd` opens its own multicast sockets on UDP 5353. On iOS 14+ that needs the
//! restricted `com.apple.developer.networking.multicast` entitlement; apps are expected to
//! use Bonjour (`NWBrowser` / `NWListener`) instead. A platform registers such a backend with
//! [`set_platform_backend`]; [`browse`] and [`Advertiser::start`] then use it instead of
//! `mdns-sd` (on every OS), and everything built on them (the sender's discovery by name or
//! id, the app's hub list, the hub's advertising) works unchanged. The backend reports what
//! it resolves through a [`DiscoveryFeed`], which applies the same validation and
//! `Found`/`Lost` rules as the `mdns-sd` path. On iOS without a registered backend, [`browse`]
//! and [`Advertiser::start`] fail with [`crate::CoreError::Discovery`] instead of silently
//! finding nothing.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// TXT key: protocol version.
pub const TXT_VERSION: &str = "v";
/// TXT key: device id (fingerprint).
pub const TXT_ID: &str = "id";
/// TXT key: display name.
pub const TXT_NAME: &str = "name";
/// TXT key: platform name.
pub const TXT_PLATFORM: &str = "platform";

/// Longest DNS label (the instance name must fit in one).
const MAX_INSTANCE_LEN: usize = 63;
/// Longest display name put into the TXT record (a TXT string holds at most 255 bytes).
const MAX_TXT_NAME_LEN: usize = 200;
/// Longest platform name accepted from a TXT record.
const MAX_TXT_PLATFORM_LEN: usize = 32;
/// Capacity of the browse event channel (events are dropped with a warning when full).
const EVENT_QUEUE: usize = 64;

/// A discovered hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    /// Hub device id.
    pub device_id: String,
    /// Hub display name.
    pub name: String,
    /// Resolved addresses (IPv4 first).
    pub addrs: Vec<IpAddr>,
    /// TCP control port.
    pub port: u16,
    /// Hub platform.
    pub platform: String,
}

/// A change in the set of visible hubs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryEvent {
    /// A hub appeared or its data changed.
    Found(HubInfo),
    /// The hub with this device id disappeared.
    Lost(String),
}

fn discovery_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::Discovery(e.to_string())
}

/// A service registration for a [`PlatformDiscovery`] backend (what [`Advertiser::start`]
/// would register with `mdns-sd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAdvert {
    /// Instance name (`"Desk (ab12)"`, one DNS label).
    pub instance: String,
    /// Service type, [`hfa_proto::SERVICE_TYPE`] (`_hfa._tcp.local.`; Bonjour APIs take
    /// `_hfa._tcp` and the domain `local.` separately).
    pub service_type: String,
    /// TCP control port.
    pub port: u16,
    /// TXT record: `v`, `id`, `name`, `platform` (in this order).
    pub txt: Vec<(String, String)>,
}

/// Keeps a platform backend's browse or registration running; dropping it stops it.
pub type DiscoveryGuard = Box<dyn Send + Sync>;

/// A platform discovery backend (e.g. Bonjour through `NWBrowser`/`NWListener` on iOS), see
/// the module docs. Registered with [`set_platform_backend`].
pub trait PlatformDiscovery: Send + Sync {
    /// Starts browsing for `_hfa._tcp` and reports through `feed` until the returned guard is
    /// dropped (or [`DiscoveryFeed::is_closed`]).
    ///
    /// # Errors
    /// [`crate::CoreError::Discovery`] if browsing cannot start.
    fn browse(&self, feed: DiscoveryFeed) -> Result<DiscoveryGuard>;

    /// Starts advertising `service` until the returned guard is dropped.
    ///
    /// # Errors
    /// [`crate::CoreError::Discovery`] if the service cannot be registered.
    fn advertise(&self, service: &ServiceAdvert) -> Result<DiscoveryGuard>;
}

static PLATFORM_BACKEND: parking_lot::RwLock<Option<Arc<dyn PlatformDiscovery>>> =
    parking_lot::RwLock::new(None);

/// Registers (`Some`) or removes (`None`) the process-wide platform discovery backend used
/// by later [`browse`] and [`Advertiser::start`] calls (running ones are not affected).
pub fn set_platform_backend(backend: Option<Arc<dyn PlatformDiscovery>>) {
    *PLATFORM_BACKEND.write() = backend;
}

fn platform_backend() -> Option<Arc<dyn PlatformDiscovery>> {
    PLATFORM_BACKEND.read().clone()
}

/// Error for iOS without a platform backend (`mdns-sd` cannot work there).
#[cfg(target_os = "ios")]
fn no_platform_backend() -> CoreError {
    discovery_err(
        "mDNS on iOS needs the native Bonjour backend (NWBrowser/NWListener), which is not registered",
    )
}

/// Advertises this device as a hub while alive.
pub struct Advertiser {
    backend: AdvertiserBackend,
    fullname: String,
    stopped: bool,
}

enum AdvertiserBackend {
    #[cfg_attr(target_os = "ios", allow(dead_code))]
    Mdns(ServiceDaemon),
    /// The platform backend's registration guard (`None` once stopped).
    Platform(Option<DiscoveryGuard>),
}

impl std::fmt::Debug for Advertiser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Advertiser")
            .field("fullname", &self.fullname)
            .field("stopped", &self.stopped)
            .finish()
    }
}

impl Advertiser {
    /// Registers `_hfa._tcp` with the given data on all IPv4 interfaces (addresses follow
    /// interface changes), or through the platform backend if one is registered.
    ///
    /// # Errors
    /// [`crate::CoreError::Discovery`] if the daemon cannot start or the data is invalid
    /// (port 0, empty device id).
    pub fn start(name: &str, device_id: &str, port: u16, platform: &str) -> Result<Advertiser> {
        if port == 0 {
            return Err(discovery_err("cannot advertise port 0"));
        }
        if device_id.is_empty() {
            return Err(discovery_err("cannot advertise an empty device id"));
        }
        let instance = instance_name(name, device_id);
        let txt_name = truncate_utf8(&hfa_proto::sanitize_name(name), MAX_TXT_NAME_LEN);
        let version = hfa_proto::PROTOCOL_VERSION.to_string();
        let properties = [
            (TXT_VERSION, version.as_str()),
            (TXT_ID, device_id),
            (TXT_NAME, txt_name.as_str()),
            (TXT_PLATFORM, platform),
        ];
        if let Some(backend) = platform_backend() {
            let service = ServiceAdvert {
                instance: instance.clone(),
                service_type: hfa_proto::SERVICE_TYPE.to_owned(),
                port,
                txt: properties
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            };
            let guard = backend.advertise(&service)?;
            let fullname = format!("{instance}.{}", hfa_proto::SERVICE_TYPE);
            tracing::info!(%fullname, port, "advertising hub via the platform backend");
            return Ok(Advertiser {
                backend: AdvertiserBackend::Platform(Some(guard)),
                fullname,
                stopped: false,
            });
        }
        #[cfg(target_os = "ios")]
        return Err(no_platform_backend());
        #[cfg(not(target_os = "ios"))]
        advertise_mdns(&instance, device_id, port, &properties)
    }

    /// The registered DNS-SD instance name (`<instance>._hfa._tcp.local.`).
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Unregisters the service and shuts the daemon down.
    ///
    /// # Errors
    /// [`crate::CoreError::Discovery`] if the daemon already exited.
    pub fn stop(mut self) -> Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.stopped, true) {
            return Ok(());
        }
        let result = match &mut self.backend {
            AdvertiserBackend::Mdns(daemon) => {
                // The daemon executes commands in order: the goodbye packets go out before
                // it exits.
                let unregistered = daemon.unregister(&self.fullname).map(drop);
                let shut_down = daemon.shutdown().map(drop);
                unregistered.and(shut_down).map_err(discovery_err)
            }
            AdvertiserBackend::Platform(guard) => {
                drop(guard.take());
                Ok(())
            }
        };
        tracing::debug!(fullname = %self.fullname, "stopped advertising");
        result
    }
}

/// Registers the service with `mdns-sd` on the IPv4 interfaces.
#[cfg_attr(target_os = "ios", allow(dead_code))]
fn advertise_mdns(
    instance: &str,
    device_id: &str,
    port: u16,
    properties: &[(&str, &str)],
) -> Result<Advertiser> {
    let host = format!("hfa-{}.local.", host_label(device_id));
    let info = ServiceInfo::new(
        hfa_proto::SERVICE_TYPE,
        instance,
        &host,
        "",
        port,
        properties,
    )
    .map_err(discovery_err)?
    .enable_addr_auto();
    let fullname = info.get_fullname().to_owned();
    let daemon = ServiceDaemon::new().map_err(discovery_err)?;
    // The hub binds IPv4 only (see the module docs). The daemon runs its commands in order,
    // so the registration below already uses the IPv4 interfaces only.
    let registered = daemon
        .disable_interface(IfKind::IPv6)
        .and_then(|()| daemon.register(info));
    if let Err(e) = registered {
        let _ = daemon.shutdown();
        return Err(discovery_err(e));
    }
    tracing::info!(%fullname, port, "advertising hub via mDNS");
    Ok(Advertiser {
        backend: AdvertiserBackend::Mdns(daemon),
        fullname,
        stopped: false,
    })
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            tracing::debug!(error = %e, "mDNS advertiser shutdown");
        }
    }
}

/// Starts browsing for hubs (through the platform backend if one is registered).
///
/// # Errors
/// [`crate::CoreError::Discovery`] if the daemon or the browse thread cannot start (or the
/// platform backend fails; on iOS: if no platform backend is registered).
pub fn browse() -> Result<Browser> {
    let (tx, events) = tokio::sync::mpsc::channel(EVENT_QUEUE);
    let feed = DiscoveryFeed {
        tx,
        instances: Arc::default(),
    };
    if let Some(backend) = platform_backend() {
        let guard = backend.browse(feed)?;
        return Ok(Browser {
            events,
            daemon: None,
            guard: Some(guard),
        });
    }
    #[cfg(target_os = "ios")]
    return Err(no_platform_backend());
    #[cfg(not(target_os = "ios"))]
    browse_mdns(feed, events)
}

#[cfg_attr(target_os = "ios", allow(dead_code))]
fn browse_mdns(
    feed: DiscoveryFeed,
    events: tokio::sync::mpsc::Receiver<DiscoveryEvent>,
) -> Result<Browser> {
    let daemon = ServiceDaemon::new().map_err(discovery_err)?;
    let receiver = match daemon.browse(hfa_proto::SERVICE_TYPE) {
        Ok(r) => r,
        Err(e) => {
            let _ = daemon.shutdown();
            return Err(discovery_err(e));
        }
    };
    let spawned = std::thread::Builder::new()
        .name("hfa-mdns-browse".into())
        .spawn(move || browse_loop(&receiver, &feed));
    if let Err(e) = spawned {
        let _ = daemon.shutdown();
        return Err(discovery_err(e));
    }
    Ok(Browser {
        events,
        daemon: Some(daemon),
        guard: None,
    })
}

/// Converts daemon events into [`DiscoveryEvent`]s until the daemon stops or the
/// [`Browser`] is dropped.
fn browse_loop(receiver: &mdns_sd::Receiver<ServiceEvent>, feed: &DiscoveryFeed) {
    while let Ok(event) = receiver.recv() {
        let open = match event {
            ServiceEvent::ServiceResolved(service) => {
                let info = hub_info(
                    |key| service.get_property_val_str(key),
                    service.get_addresses().iter().map(|a| a.to_ip_addr()),
                    service.port,
                );
                feed.found(&service.fullname, info)
            }
            ServiceEvent::ServiceRemoved(_, fullname) => feed.removed(&fullname),
            ServiceEvent::SearchStopped(_) => break,
            _ => continue,
        };
        if !open {
            break;
        }
    }
    tracing::debug!("mDNS browse thread finished");
}

/// Delivers the instances a browse backend resolves to its [`Browser`], applying the same
/// rules as the `mdns-sd` path: only instances with `v=0`, a well-formed `id` and a non-zero
/// port are reported (`Found`, repeated when the data changes); `Lost(id)` when the last
/// instance of that device id is removed. Cheap to clone; every method returns `false` once
/// the [`Browser`] is gone (stop browsing then).
#[derive(Clone)]
pub struct DiscoveryFeed {
    tx: tokio::sync::mpsc::Sender<DiscoveryEvent>,
    /// Instance full name → device id, to translate removals.
    instances: Arc<parking_lot::Mutex<HashMap<String, String>>>,
}

impl std::fmt::Debug for DiscoveryFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryFeed")
            .field("closed", &self.tx.is_closed())
            .finish_non_exhaustive()
    }
}

impl DiscoveryFeed {
    /// A resolved instance: its full name (any unique string per instance), TXT record,
    /// addresses and TCP port. An instance that is not a valid v0 hub is ignored.
    pub fn resolved(
        &self,
        instance: &str,
        txt: &[(String, String)],
        addrs: &[IpAddr],
        port: u16,
    ) -> bool {
        let get = |key: &str| {
            txt.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
        };
        self.found(instance, hub_info(get, addrs.iter().copied(), port))
    }

    /// The instance with this full name disappeared.
    pub fn removed(&self, instance: &str) -> bool {
        let lost = {
            let mut instances = self.instances.lock();
            match instances.remove(instance) {
                Some(id) if !instances.values().any(|other| *other == id) => Some(id),
                _ => None,
            }
        };
        match lost {
            Some(id) => self.send(DiscoveryEvent::Lost(id)),
            None => !self.is_closed(),
        }
    }

    /// `true` once the [`Browser`] was dropped.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    fn found(&self, instance: &str, info: Option<HubInfo>) -> bool {
        let Some(info) = info else {
            tracing::debug!(%instance, "ignoring an invalid hfa service");
            return !self.is_closed();
        };
        self.instances
            .lock()
            .insert(instance.to_owned(), info.device_id.clone());
        self.send(DiscoveryEvent::Found(info))
    }

    fn send(&self, event: DiscoveryEvent) -> bool {
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(ev)) => {
                tracing::warn!(event = ?ev, "discovery consumer is not keeping up; dropping an event");
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// Builds a [`HubInfo`] from a resolved instance's TXT lookup, addresses and port, `None` if
/// it is not a valid v0 hub.
fn hub_info<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    addrs: impl IntoIterator<Item = IpAddr>,
    port: u16,
) -> Option<HubInfo> {
    let version = get(TXT_VERSION)?;
    if version != hfa_proto::PROTOCOL_VERSION.to_string() {
        return None;
    }
    let device_id = get(TXT_ID)?;
    if !hfa_proto::is_fingerprint(device_id) || port == 0 {
        return None;
    }
    let name = get(TXT_NAME)
        .map(hfa_proto::sanitize_name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| device_id.to_owned());
    let platform: String = get(TXT_PLATFORM)
        .unwrap_or("unknown")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TXT_PLATFORM_LEN)
        .collect();
    let mut addrs: Vec<IpAddr> = addrs
        .into_iter()
        .filter(|ip| match ip {
            // A link-local IPv6 address is useless without its scope id.
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
            IpAddr::V4(_) => true,
        })
        .collect();
    addrs.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    addrs.dedup();
    Some(HubInfo {
        device_id: device_id.to_owned(),
        name,
        addrs,
        port,
        platform,
    })
}

/// `"<name> (<first 4 id chars>)"`, fitting one DNS label; `.` and `\` are replaced so the
/// label needs no escaping.
fn instance_name(name: &str, device_id: &str) -> String {
    let short: String = device_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(4)
        .collect();
    let suffix = format!(" ({short})");
    let clean: String = hfa_proto::sanitize_name(name)
        .trim()
        .chars()
        .map(|c| if c == '.' || c == '\\' { '-' } else { c })
        .collect();
    let base = if clean.is_empty() {
        "hfa hub"
    } else {
        clean.as_str()
    };
    let mut out = truncate_utf8(base, MAX_INSTANCE_LEN.saturating_sub(suffix.len()));
    out.push_str(&suffix);
    out
}

/// A DNS host label from a device id (`[a-z0-9-]`, at most 50 chars).
fn host_label(device_id: &str) -> String {
    device_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(50)
        .collect()
}

/// Truncates `s` to at most `max` bytes on a character boundary.
fn truncate_utf8(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// A running browse session. Dropping it stops browsing.
pub struct Browser {
    events: tokio::sync::mpsc::Receiver<DiscoveryEvent>,
    daemon: Option<ServiceDaemon>,
    /// The platform backend's browse guard.
    guard: Option<DiscoveryGuard>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("running", &(self.daemon.is_some() || self.guard.is_some()))
            .finish()
    }
}

impl Browser {
    /// Waits for the next event. `None` once browsing stopped.
    pub async fn recv(&mut self) -> Option<DiscoveryEvent> {
        self.events.recv().await
    }

    /// Returns an already queued event without waiting.
    pub fn try_recv(&mut self) -> Option<DiscoveryEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.stop_browse(hfa_proto::SERVICE_TYPE);
            // Exiting the daemon disconnects the event receiver, which ends the thread.
            let _ = daemon.shutdown();
        }
        // Close the feed before the platform backend is told to stop.
        self.events.close();
        drop(self.guard.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_fit_a_dns_label() {
        let id = "ab12-cd34-ef56-7890";
        assert_eq!(instance_name("Desk", id), "Desk (ab12)");
        assert_eq!(instance_name("my.mac\\pc", id), "my-mac-pc (ab12)");
        assert_eq!(instance_name("  ", id), "hfa hub (ab12)");
        let long = "Küche🎧".repeat(20);
        let n = instance_name(&long, id);
        assert!(n.len() <= MAX_INSTANCE_LEN, "{n}");
        assert!(n.ends_with(" (ab12)"));
        assert_eq!(host_label(id), id);
        assert_eq!(truncate_utf8("äöü", 3), "ä");
    }
}
