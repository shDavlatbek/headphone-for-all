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
//! - [`payload`]: the media payload container (Opus packet + optional redundant copy).
//! - [`sender`]: [`SenderEngine`] (capture → Opus → UDP).
//! - [`hub`]: [`HubEngine`] (UDP → jitter buffer → decode → drift → mix → output).
//! - [`netsim`]: a lossy/jittery UDP relay for tests and `hfa selftest`.
//!
//! See `docs/CONTRACTS.md` §6 for the binding contract.

pub mod config;
pub mod control;
pub mod discovery;
pub mod error;
pub mod hub;
mod hub_mixer;
pub mod identity;
pub mod media;
pub mod netsim;
pub mod pairing;
pub mod payload;
pub mod sender;
mod sender_adapt;
mod sender_encoder;

pub use config::Settings;
pub use control::{ControlChannel, PeerInfo};
pub use discovery::{browse, Advertiser, Browser, DiscoveryEvent, HubInfo};
pub use error::CoreError;
pub use hub::{HubConfig, HubEngine, HubEvent, HubHandle, SourceInfo, StreamCounters, StreamStats};
pub use identity::{Identity, PeerRole, PeerRoles, TrustStore, TrustedPeer};
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

/// Allocation counting for the real-time path tests (`hub_mixer`, `sender_encoder`): a
/// global allocator that counts the current thread's allocations while a probe is active.
#[cfg(test)]
pub(crate) mod test_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    struct Counting;

    thread_local! {
        static TRACKING: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    fn note() {
        if TRACKING.try_with(|t| t.get()).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
    }

    // SAFETY: forwards every call to the system allocator; only adds a counter.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note();
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            note();
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note();
            // SAFETY: same contract as the caller's.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    /// Runs `f` and returns how many allocations it made on this thread.
    pub(crate) fn count_allocs(f: impl FnOnce()) -> usize {
        let before = ALLOCATIONS.with(Cell::get);
        TRACKING.with(|t| t.set(true));
        f();
        TRACKING.with(|t| t.set(false));
        ALLOCATIONS.with(Cell::get) - before
    }
}

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
