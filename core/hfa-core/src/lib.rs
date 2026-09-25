//! # hfa-core
//!
//! Networking and engines of headphone-for-all (tokio):
//!
//! - [`config`]: persisted [`Settings`].
//! - [`identity`]: the device [`Identity`] (Noise static key) and the [`TrustStore`].
//! - [`pairing`]: hub-side [`PairingManager`] (PIN / token windows).
//! - [`control`]: the Noise-encrypted TCP [`ControlChannel`].
//! - [`discovery`]: mDNS advertising and browsing of hubs.
//! - [`media`]: encrypted UDP media send/receive helpers.
//! - [`sender`]: [`SenderEngine`] (capture → Opus → UDP).
//! - [`hub`]: [`HubEngine`] (UDP → jitter buffer → decode → drift → mix → output).
//!
//! See `docs/CONTRACTS.md` §6 for the binding contract.

pub mod config;
pub mod control;
pub mod discovery;
pub mod error;
pub mod hub;
pub mod identity;
pub mod media;
pub mod pairing;
pub mod sender;

pub use config::Settings;
pub use control::{ControlChannel, PeerInfo};
pub use discovery::{browse, Advertiser, Browser, DiscoveryEvent, HubInfo};
pub use error::CoreError;
pub use hub::{HubConfig, HubEngine, HubEvent, HubHandle, SourceInfo, StreamStats};
pub use identity::{Identity, TrustStore, TrustedPeer};
pub use pairing::{PairingAttempt, PairingInfo, PairingManager};
pub use sender::{
    HubAddress, SenderConfig, SenderEngine, SenderEvent, SenderHandle, SenderState, SenderStatus,
};

/// Result type used throughout this crate.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Application version reported in `Hello`.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Platform name reported in `Hello` and mDNS TXT records:
/// `windows`, `macos`, `linux`, `android`, `ios` or `unknown`.
pub fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

// Compile-time guarantees used by hfa-ffi (handles live in a global manager shared across
// threads) and by the engines (state moved into tokio tasks).
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    const fn assert_send<T: Send>() {}
    assert_send_sync::<HubHandle>();
    assert_send_sync::<SenderHandle>();
    assert_send_sync::<PairingManager>();
    assert_send_sync::<TrustStore>();
    assert_send::<SenderConfig>();
    assert_send::<HubConfig>();
    assert_send::<ControlChannel>();
    // Held across `.await` inside `ControlChannel::accept`.
    assert_send::<PairingAttempt<'static>>();
};

#[cfg(test)]
mod tests {
    #[test]
    fn platform_name_is_known_on_ci_targets() {
        let name = super::platform_name();
        assert!(["windows", "macos", "linux", "android", "ios", "unknown"].contains(&name));
        #[cfg(target_os = "linux")]
        assert_eq!(name, "linux");
    }
}
