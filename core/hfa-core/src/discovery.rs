//! mDNS / DNS-SD discovery of hubs (`_hfa._tcp.local.`, via `mdns-sd`).
//!
//! TXT records: `v` (protocol version), `id` (device id), `name`, `platform`.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::Result;

/// TXT key: protocol version.
pub const TXT_VERSION: &str = "v";
/// TXT key: device id (fingerprint).
pub const TXT_ID: &str = "id";
/// TXT key: display name.
pub const TXT_NAME: &str = "name";
/// TXT key: platform name.
pub const TXT_PLATFORM: &str = "platform";

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

/// Advertises this device as a hub while alive.
pub struct Advertiser {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Registers `_hfa._tcp` with the given data on all interfaces.
    ///
    /// # Errors
    /// [`crate::CoreError::Discovery`].
    pub fn start(_name: &str, _device_id: &str, _port: u16, _platform: &str) -> Result<Advertiser> {
        todo!("feat/core-engine")
    }

    /// Unregisters the service and shuts the daemon down.
    ///
    /// # Errors
    /// [`crate::CoreError::Discovery`].
    pub fn stop(self) -> Result<()> {
        let _ = (&self.daemon, &self.fullname);
        todo!("feat/core-engine")
    }
}

/// Starts browsing for hubs.
///
/// # Errors
/// [`crate::CoreError::Discovery`].
pub fn browse() -> Result<Browser> {
    todo!("feat/core-engine")
}

/// A running browse session. Dropping it stops browsing.
pub struct Browser {
    events: tokio::sync::mpsc::Receiver<DiscoveryEvent>,
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
