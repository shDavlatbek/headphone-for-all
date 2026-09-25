//! mDNS / DNS-SD discovery of hubs (`_hfa._tcp.local.`, via `mdns-sd`).
//!
//! - **Advertising** ([`Advertiser`]): one service instance per hub. The instance name is the
//!   display name plus a short device-id suffix (`Desk (ab12)`), so two hubs with the same
//!   name never collide; the mDNS host name is `hfa-<device id>.local.`. The addresses follow
//!   the host's interfaces automatically. TXT records: `v` (protocol version), `id` (device
//!   id), `name`, `platform`.
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

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
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

/// Advertises this device as a hub while alive.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    stopped: bool,
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
    /// Registers `_hfa._tcp` with the given data on all interfaces (addresses follow
    /// interface changes).
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
        let host = format!("hfa-{}.local.", host_label(device_id));
        let txt_name = truncate_utf8(&hfa_proto::sanitize_name(name), MAX_TXT_NAME_LEN);
        let version = hfa_proto::PROTOCOL_VERSION.to_string();
        let properties = [
            (TXT_VERSION, version.as_str()),
            (TXT_ID, device_id),
            (TXT_NAME, txt_name.as_str()),
            (TXT_PLATFORM, platform),
        ];
        let info = ServiceInfo::new(
            hfa_proto::SERVICE_TYPE,
            &instance,
            &host,
            "",
            port,
            &properties[..],
        )
        .map_err(discovery_err)?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        let daemon = ServiceDaemon::new().map_err(discovery_err)?;
        if let Err(e) = daemon.register(info) {
            let _ = daemon.shutdown();
            return Err(discovery_err(e));
        }
        tracing::info!(%fullname, port, "advertising hub via mDNS");
        Ok(Advertiser {
            daemon,
            fullname,
            stopped: false,
        })
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
        // The daemon executes commands in order: the goodbye packets go out before it exits.
        let unregistered = self.daemon.unregister(&self.fullname).map(drop);
        let shut_down = self.daemon.shutdown().map(drop);
        tracing::debug!(fullname = %self.fullname, "stopped advertising");
        unregistered.and(shut_down).map_err(discovery_err)
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            tracing::debug!(error = %e, "mDNS advertiser shutdown");
        }
    }
}

/// Starts browsing for hubs.
///
/// # Errors
/// [`crate::CoreError::Discovery`] if the daemon or the browse thread cannot start.
pub fn browse() -> Result<Browser> {
    let daemon = ServiceDaemon::new().map_err(discovery_err)?;
    let receiver = match daemon.browse(hfa_proto::SERVICE_TYPE) {
        Ok(r) => r,
        Err(e) => {
            let _ = daemon.shutdown();
            return Err(discovery_err(e));
        }
    };
    let (tx, events) = tokio::sync::mpsc::channel(EVENT_QUEUE);
    let spawned = std::thread::Builder::new()
        .name("hfa-mdns-browse".into())
        .spawn(move || browse_loop(&receiver, &tx));
    if let Err(e) = spawned {
        let _ = daemon.shutdown();
        return Err(discovery_err(e));
    }
    Ok(Browser {
        events,
        daemon: Some(daemon),
    })
}

/// Converts daemon events into [`DiscoveryEvent`]s until the daemon stops or the
/// [`Browser`] is dropped.
fn browse_loop(
    receiver: &mdns_sd::Receiver<ServiceEvent>,
    tx: &tokio::sync::mpsc::Sender<DiscoveryEvent>,
) {
    // Instance full name → device id, to translate removals.
    let mut instances: HashMap<String, String> = HashMap::new();
    while let Ok(event) = receiver.recv() {
        let out = match event {
            ServiceEvent::ServiceResolved(service) => match hub_info(&service) {
                Some(info) => {
                    instances.insert(service.fullname.clone(), info.device_id.clone());
                    DiscoveryEvent::Found(info)
                }
                None => {
                    tracing::debug!(fullname = %service.fullname, "ignoring an invalid hfa service");
                    continue;
                }
            },
            ServiceEvent::ServiceRemoved(_, fullname) => {
                let Some(id) = instances.remove(&fullname) else {
                    continue;
                };
                if instances.values().any(|other| *other == id) {
                    continue;
                }
                DiscoveryEvent::Lost(id)
            }
            ServiceEvent::SearchStopped(_) => break,
            _ => continue,
        };
        match tx.try_send(out) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(ev)) => {
                tracing::warn!(event = ?ev, "discovery consumer is not keeping up; dropping an event");
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
    tracing::debug!("mDNS browse thread finished");
}

/// Builds a [`HubInfo`] from a resolved instance, `None` if it is not a valid v0 hub.
fn hub_info(service: &ResolvedService) -> Option<HubInfo> {
    let version = service.get_property_val_str(TXT_VERSION)?;
    if version != hfa_proto::PROTOCOL_VERSION.to_string() {
        return None;
    }
    let device_id = service.get_property_val_str(TXT_ID)?;
    if !hfa_proto::is_fingerprint(device_id) || service.port == 0 {
        return None;
    }
    let name = service
        .get_property_val_str(TXT_NAME)
        .map(hfa_proto::sanitize_name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| device_id.to_owned());
    let platform: String = service
        .get_property_val_str(TXT_PLATFORM)
        .unwrap_or("unknown")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TXT_PLATFORM_LEN)
        .collect();
    let mut addrs: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|a| a.to_ip_addr())
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
        port: service.port,
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
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("running", &self.daemon.is_some())
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
