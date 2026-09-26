//! C ABI through which a native Bonjour implementation becomes hfa-core's platform discovery
//! backend (header: `core/hfa-ffi/include/hfa_discovery.h`, keep in sync).
//!
//! `mdns-sd` cannot run on iOS: its own multicast sockets on UDP 5353 need the restricted
//! `com.apple.developer.networking.multicast` entitlement. The iOS app therefore browses and
//! advertises with Apple's Bonjour APIs in Swift (`app/ios/Runner/HfaBonjourDiscovery.swift`)
//! and plugs them into [`hfa_core::discovery::set_platform_backend`] through this module:
//!
//! ```c
//! int32_t hfa_discovery_register(const HfaDiscoveryCallbacks *callbacks);
//! int32_t hfa_discovery_unregister(void);
//! int32_t hfa_discovery_resolved(uint64_t browse_id, const char *service_json);
//! int32_t hfa_discovery_removed(uint64_t browse_id, const char *instance);
//! const char *hfa_discovery_last_error(void);
//! ```
//!
//! - **Rust → native.** The app registers four callbacks once at start-up, before the engine
//!   runs. `browse_start(ctx, browse_id)` / `browse_stop(ctx, browse_id)` bracket one
//!   `hfa_core::discovery::browse()` session; `advertise_start(ctx, advert_id, json)` /
//!   `advertise_stop(ctx, advert_id)` bracket one hub registration. The advert JSON is
//!   `{"instance": "Desk (ab12)", "type": "_hfa._tcp", "domain": "local.", "port": 47810,
//!   "txt": [["v","0"], ["id","…"], ["name","Desk"], ["platform","ios"]]}` and is only valid
//!   during the call. A non-zero return from a `*_start` callback fails the browse or the
//!   registration with [`hfa_core::CoreError::Discovery`]. The callbacks are called from any
//!   thread, never while a lock of this module is held, and must return quickly (start the work
//!   asynchronously).
//! - **Native → Rust.** For a running browse the app reports every resolved instance with
//!   [`hfa_discovery_resolved`] (`{"instance": "Desk (ab12)", "txt": {"v": "0", …},
//!   "addrs": ["192.168.1.20", "fd00::20"], "port": 47810}`) and every vanished one with
//!   [`hfa_discovery_removed`]. They return [`HFA_DISCOVERY_CLOSED`] once Rust no longer
//!   listens (the browse was stopped): the app then stops that browse. The same TXT/`v`/`id`
//!   validation and `Found`/`Lost` rules as on the `mdns-sd` path apply
//!   ([`hfa_core::discovery::DiscoveryFeed`]); a report without a usable address is ignored.
//!
//! Ids instead of pointers: Rust hands the native side plain 64-bit ids and keeps the browse
//! feeds in a map, so a late or duplicate report for a stopped browse is harmless.
//!
//! Every exported function catches panics and never unwinds across the boundary; after a
//! negative return [`hfa_discovery_last_error`] describes the failure (thread-local, cleared by
//! every `hfa_discovery_*` call). The functions are `#[no_mangle]` on iOS only; elsewhere they
//! are ordinary Rust functions, so the whole module is unit tested on the host.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::net::IpAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use hfa_core::discovery::{
    set_platform_backend, DiscoveryFeed, DiscoveryGuard, PlatformDiscovery, ServiceAdvert,
};
use hfa_core::CoreError;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::panic_message;

/// Success.
pub const HFA_DISCOVERY_OK: i32 = 0;
/// The browse is no longer running on the Rust side: stop it natively.
pub const HFA_DISCOVERY_CLOSED: i32 = 1;
/// A pointer argument was null, a string not UTF-8, or the JSON malformed.
pub const HFA_DISCOVERY_ERR_INVALID_ARGUMENT: i32 = -1;
/// A panic was caught at the FFI boundary (a bug; see the last error).
pub const HFA_DISCOVERY_ERR_INTERNAL: i32 = -5;

/// Bonjour service type without the domain (`hfa_proto::SERVICE_TYPE` is `_hfa._tcp.local.`).
pub const BONJOUR_TYPE: &str = "_hfa._tcp";
/// Bonjour domain of [`BONJOUR_TYPE`].
pub const BONJOUR_DOMAIN: &str = "local.";

/// Starts browsing (`browse_id` names the session). Returns 0 or a failure code.
pub type BrowseStartFn = unsafe extern "C" fn(ctx: *mut c_void, browse_id: u64) -> i32;
/// Stops a browse started with [`BrowseStartFn`].
pub type BrowseStopFn = unsafe extern "C" fn(ctx: *mut c_void, browse_id: u64);
/// Registers a service described by the NUL-terminated JSON (valid during the call only).
/// Returns 0 or a failure code.
pub type AdvertiseStartFn =
    unsafe extern "C" fn(ctx: *mut c_void, advert_id: u64, service_json: *const c_char) -> i32;
/// Unregisters a service registered with [`AdvertiseStartFn`].
pub type AdvertiseStopFn = unsafe extern "C" fn(ctx: *mut c_void, advert_id: u64);

/// The native callbacks (`HfaDiscoveryCallbacks` in the header). Every function pointer is
/// required; `ctx` is passed back unchanged and may be null.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HfaDiscoveryCallbacks {
    /// Opaque context for the callbacks.
    pub ctx: *mut c_void,
    /// Starts a browse.
    pub browse_start: Option<BrowseStartFn>,
    /// Stops a browse.
    pub browse_stop: Option<BrowseStopFn>,
    /// Starts advertising a service.
    pub advertise_start: Option<AdvertiseStartFn>,
    /// Stops advertising a service.
    pub advertise_stop: Option<AdvertiseStopFn>,
}

/// Validated callbacks (every pointer present).
#[derive(Clone, Copy)]
struct Callbacks {
    ctx: *mut c_void,
    browse_start: BrowseStartFn,
    browse_stop: BrowseStopFn,
    advertise_start: AdvertiseStartFn,
    advertise_stop: AdvertiseStopFn,
}

// SAFETY: `ctx` is an opaque value owned by the registrant, which promises (header contract)
// that the callbacks may be called from any thread with it until the process exits; Rust never
// dereferences it.
unsafe impl Send for Callbacks {}
// SAFETY: see `Send`; `Callbacks` is immutable.
unsafe impl Sync for Callbacks {}

impl Callbacks {
    fn from_raw(raw: &HfaDiscoveryCallbacks) -> Result<Self, &'static str> {
        Ok(Self {
            ctx: raw.ctx,
            browse_start: raw.browse_start.ok_or("browse_start is null")?,
            browse_stop: raw.browse_stop.ok_or("browse_stop is null")?,
            advertise_start: raw.advertise_start.ok_or("advertise_start is null")?,
            advertise_stop: raw.advertise_stop.ok_or("advertise_stop is null")?,
        })
    }
}

/// Feeds of the running browses by id (a `BTreeMap` so the static needs no lazy init).
static BROWSES: Mutex<BTreeMap<u64, DiscoveryFeed>> = Mutex::new(BTreeMap::new());
/// Next browse or advert id (never 0).
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The [`PlatformDiscovery`] backend that forwards to the native callbacks.
struct NativeBackend {
    callbacks: Callbacks,
}

impl PlatformDiscovery for NativeBackend {
    fn browse(&self, feed: DiscoveryFeed) -> hfa_core::Result<DiscoveryGuard> {
        let id = next_id();
        BROWSES.lock().insert(id, feed);
        let cb = self.callbacks;
        // SAFETY: a registered, non-null callback; the registrant allows calls from any thread.
        let code = unsafe { (cb.browse_start)(cb.ctx, id) };
        if code != 0 {
            BROWSES.lock().remove(&id);
            return Err(CoreError::Discovery(format!(
                "the native Bonjour browser did not start (code {code})"
            )));
        }
        tracing::debug!(browse_id = id, "native Bonjour browse started");
        Ok(Box::new(BrowseGuard { id, callbacks: cb }))
    }

    fn advertise(&self, service: &ServiceAdvert) -> hfa_core::Result<DiscoveryGuard> {
        let json = advert_json(service)?;
        let text = CString::new(json)
            .map_err(|_| CoreError::Discovery("service data contains a NUL byte".into()))?;
        let id = next_id();
        let cb = self.callbacks;
        // SAFETY: a registered, non-null callback; `text` outlives the call.
        let code = unsafe { (cb.advertise_start)(cb.ctx, id, text.as_ptr()) };
        if code != 0 {
            return Err(CoreError::Discovery(format!(
                "the native Bonjour registration failed (code {code})"
            )));
        }
        tracing::debug!(advert_id = id, instance = %service.instance, "native Bonjour registration started");
        Ok(Box::new(AdvertGuard { id, callbacks: cb }))
    }
}

/// Stops a native browse when dropped (after forgetting its feed).
struct BrowseGuard {
    id: u64,
    callbacks: Callbacks,
}

impl Drop for BrowseGuard {
    fn drop(&mut self) {
        BROWSES.lock().remove(&self.id);
        // SAFETY: registered callback, called without any lock held.
        unsafe { (self.callbacks.browse_stop)(self.callbacks.ctx, self.id) };
    }
}

/// Stops a native registration when dropped.
struct AdvertGuard {
    id: u64,
    callbacks: Callbacks,
}

impl Drop for AdvertGuard {
    fn drop(&mut self) {
        // SAFETY: registered callback, called without any lock held.
        unsafe { (self.callbacks.advertise_stop)(self.callbacks.ctx, self.id) };
    }
}

/// The advert JSON handed to `advertise_start` (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AdvertJson {
    instance: String,
    #[serde(rename = "type")]
    service_type: String,
    domain: String,
    port: u16,
    txt: Vec<(String, String)>,
}

/// Splits a full DNS-SD service type (`_hfa._tcp.local.`) into the Bonjour type (`_hfa._tcp`)
/// and domain (`local.`); a type without a domain gets `local.`.
fn split_service_type(full: &str) -> Option<(String, String)> {
    let trimmed = full.trim_end_matches('.');
    let mut labels = trimmed.splitn(3, '.');
    let service = labels
        .next()
        .filter(|l| l.len() > 1 && l.starts_with('_'))?;
    let proto = labels
        .next()
        .filter(|l| l.eq_ignore_ascii_case("_tcp") || l.eq_ignore_ascii_case("_udp"))?;
    let domain = match labels.next() {
        Some(d) if !d.is_empty() => format!("{d}."),
        _ => BONJOUR_DOMAIN.to_owned(),
    };
    Some((format!("{service}.{proto}"), domain))
}

fn advert_json(service: &ServiceAdvert) -> hfa_core::Result<String> {
    let (service_type, domain) = split_service_type(&service.service_type).ok_or_else(|| {
        CoreError::Discovery(format!("invalid service type {:?}", service.service_type))
    })?;
    let doc = AdvertJson {
        instance: service.instance.clone(),
        service_type,
        domain,
        port: service.port,
        txt: service.txt.clone(),
    };
    serde_json::to_string(&doc).map_err(|e| CoreError::Discovery(e.to_string()))
}

/// A resolved instance reported by the native side (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ResolvedJson {
    instance: String,
    #[serde(default)]
    txt: BTreeMap<String, String>,
    #[serde(default)]
    addrs: Vec<String>,
    port: u16,
}

/// Parses an address as Bonjour reports it; an IPv6 zone (`fe80::1%en0`) is dropped (the
/// engine cannot use a scoped address anyway). `None` for anything else.
fn parse_addr(text: &str) -> Option<IpAddr> {
    let bare = text.trim().split('%').next().unwrap_or_default();
    bare.parse().ok()
}

/// Whether the engine can dial `ip` (see [`ResolvedJson::addrs`]).
fn is_usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_unspecified() && !v4.is_multicast() && !v4.is_broadcast(),
        IpAddr::V6(v6) => {
            !v6.is_unspecified() && !v6.is_multicast() && (v6.segments()[0] & 0xffc0) != 0xfe80
        }
    }
}

impl ResolvedJson {
    fn parse(json: &str) -> Result<Self, String> {
        let doc: ResolvedJson =
            serde_json::from_str(json).map_err(|e| format!("service json: {e}"))?;
        if doc.instance.is_empty() {
            return Err("service json: empty instance".into());
        }
        Ok(doc)
    }

    /// The usable addresses: parsed, without unspecified, multicast and IPv6 link-local ones
    /// (useless without their zone), sorted and deduplicated.
    fn addrs(&self) -> Vec<IpAddr> {
        let mut out: Vec<IpAddr> = self
            .addrs
            .iter()
            .filter_map(|a| parse_addr(a))
            .filter(is_usable)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    fn txt(&self) -> Vec<(String, String)> {
        self.txt
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: &str) {
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| {
        if let Ok(mut slot) = e.try_borrow_mut() {
            *slot = Some(text);
        }
    });
}

fn clear_last_error() {
    LAST_ERROR.with(|e| {
        if let Ok(mut slot) = e.try_borrow_mut() {
            *slot = None;
        }
    });
}

/// Runs `f` at the boundary: clears the last error, catches panics, records failures.
fn boundary(f: impl FnOnce() -> Result<i32, (i32, String)>) -> i32 {
    clear_last_error();
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(code)) => code,
        Ok(Err((code, message))) => {
            set_last_error(&message);
            code
        }
        Err(payload) => {
            let message = format!("internal panic: {}", panic_message(payload.as_ref()));
            let _ = catch_unwind(|| tracing::error!("{message}"));
            set_last_error(&message);
            HFA_DISCOVERY_ERR_INTERNAL
        }
    }
}

/// Borrows a NUL-terminated UTF-8 argument.
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated C string for the lifetime `'a`.
unsafe fn utf8_arg<'a>(ptr: *const c_char, name: &str) -> Result<&'a str, (i32, String)> {
    if ptr.is_null() {
        return Err((
            HFA_DISCOVERY_ERR_INVALID_ARGUMENT,
            format!("{name} is null"),
        ));
    }
    // SAFETY: non-null and NUL-terminated per the caller's contract.
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| {
        (
            HFA_DISCOVERY_ERR_INVALID_ARGUMENT,
            format!("{name} is not UTF-8"),
        )
    })
}

/// The feed of a running browse, `None` if it stopped (or never existed).
fn feed(browse_id: u64) -> Option<DiscoveryFeed> {
    let browses = BROWSES.lock();
    browses.get(&browse_id).filter(|f| !f.is_closed()).cloned()
}

/// Maps a feed's "still open" answer to a return code, forgetting a closed browse.
fn open_or_closed(browse_id: u64, open: bool) -> i32 {
    if open {
        HFA_DISCOVERY_OK
    } else {
        BROWSES.lock().remove(&browse_id);
        HFA_DISCOVERY_CLOSED
    }
}

/// Registers the native callbacks as hfa-core's platform discovery backend (replacing an
/// earlier registration; running browses and registrations keep the callbacks they started
/// with). Returns [`HFA_DISCOVERY_OK`] or [`HFA_DISCOVERY_ERR_INVALID_ARGUMENT`] (null
/// `callbacks` or a null function pointer).
///
/// # Safety
/// `callbacks` must be null or point to a readable `HfaDiscoveryCallbacks` (copied during the
/// call). The functions must be callable from any thread with `ctx` for the rest of the
/// process's life (or until [`hfa_discovery_unregister`] and the end of every browse and
/// registration started before it).
#[cfg_attr(target_os = "ios", no_mangle)]
pub unsafe extern "C" fn hfa_discovery_register(callbacks: *const HfaDiscoveryCallbacks) -> i32 {
    boundary(|| {
        if callbacks.is_null() {
            return Err((
                HFA_DISCOVERY_ERR_INVALID_ARGUMENT,
                "callbacks is null".into(),
            ));
        }
        // SAFETY: non-null and readable per the function contract; copied out immediately.
        let raw = unsafe { *callbacks };
        let callbacks = Callbacks::from_raw(&raw)
            .map_err(|e| (HFA_DISCOVERY_ERR_INVALID_ARGUMENT, e.to_owned()))?;
        set_platform_backend(Some(Arc::new(NativeBackend { callbacks })));
        tracing::info!("native Bonjour discovery backend registered");
        Ok(HFA_DISCOVERY_OK)
    })
}

/// Removes the platform discovery backend (later browses and registrations fail on iOS, use
/// `mdns-sd` elsewhere). Running ones are not affected. Returns [`HFA_DISCOVERY_OK`].
#[cfg_attr(target_os = "ios", no_mangle)]
pub extern "C" fn hfa_discovery_unregister() -> i32 {
    boundary(|| {
        set_platform_backend(None);
        Ok(HFA_DISCOVERY_OK)
    })
}

/// Reports a resolved instance of the browse `browse_id` (JSON, see the module docs). Returns
/// [`HFA_DISCOVERY_OK`], [`HFA_DISCOVERY_CLOSED`] (stop that browse) or
/// [`HFA_DISCOVERY_ERR_INVALID_ARGUMENT`].
///
/// # Safety
/// `service_json` must be null or a valid NUL-terminated C string for the duration of the call.
#[cfg_attr(target_os = "ios", no_mangle)]
pub unsafe extern "C" fn hfa_discovery_resolved(
    browse_id: u64,
    service_json: *const c_char,
) -> i32 {
    boundary(|| {
        // SAFETY: forwarded function contract.
        let json = unsafe { utf8_arg(service_json, "service_json") }?;
        let Some(feed) = feed(browse_id) else {
            return Ok(HFA_DISCOVERY_CLOSED);
        };
        let doc = ResolvedJson::parse(json).map_err(|e| (HFA_DISCOVERY_ERR_INVALID_ARGUMENT, e))?;
        let addrs = doc.addrs();
        if addrs.is_empty() {
            tracing::debug!(instance = %doc.instance, "ignoring a Bonjour service without a usable address");
            return Ok(open_or_closed(browse_id, !feed.is_closed()));
        }
        let open = feed.resolved(&doc.instance, &doc.txt(), &addrs, doc.port);
        Ok(open_or_closed(browse_id, open))
    })
}

/// Reports that `instance` of the browse `browse_id` disappeared. Returns
/// [`HFA_DISCOVERY_OK`], [`HFA_DISCOVERY_CLOSED`] (stop that browse) or
/// [`HFA_DISCOVERY_ERR_INVALID_ARGUMENT`].
///
/// # Safety
/// `instance` must be null or a valid NUL-terminated C string for the duration of the call.
#[cfg_attr(target_os = "ios", no_mangle)]
pub unsafe extern "C" fn hfa_discovery_removed(browse_id: u64, instance: *const c_char) -> i32 {
    boundary(|| {
        // SAFETY: forwarded function contract.
        let instance = unsafe { utf8_arg(instance, "instance") }?;
        let Some(feed) = feed(browse_id) else {
            return Ok(HFA_DISCOVERY_CLOSED);
        };
        let open = feed.removed(instance);
        Ok(open_or_closed(browse_id, open))
    })
}

/// Message of the last failed `hfa_discovery_*` call on this thread, or null. Valid until the
/// next `hfa_discovery_*` call on this thread.
#[cfg_attr(target_os = "ios", no_mangle)]
pub extern "C" fn hfa_discovery_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match e.try_borrow() {
        Ok(slot) => slot.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        Err(_) => std::ptr::null(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_types_split_into_type_and_domain() {
        let split = |s: &str| split_service_type(s);
        assert_eq!(
            split(hfa_proto::SERVICE_TYPE),
            Some((BONJOUR_TYPE.to_owned(), BONJOUR_DOMAIN.to_owned()))
        );
        assert_eq!(
            split("_hfa._tcp"),
            Some(("_hfa._tcp".to_owned(), "local.".to_owned()))
        );
        assert_eq!(
            split("_x._udp.example.org."),
            Some(("_x._udp".to_owned(), "example.org.".to_owned()))
        );
        assert_eq!(split("hfa._tcp.local."), None);
        assert_eq!(split("_hfa.tcp.local."), None);
        assert_eq!(split(""), None);
    }

    #[test]
    fn advert_json_has_the_documented_shape() {
        let service = ServiceAdvert {
            instance: "Desk (ab12)".into(),
            service_type: hfa_proto::SERVICE_TYPE.into(),
            port: 47_810,
            txt: vec![
                ("v".into(), "0".into()),
                ("name".into(), "Desk \"1\"".into()),
            ],
        };
        let json = advert_json(&service).expect("json");
        let doc: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            doc,
            serde_json::json!({
                "instance": "Desk (ab12)", "type": "_hfa._tcp", "domain": "local.",
                "port": 47810, "txt": [["v", "0"], ["name", "Desk \"1\""]]
            })
        );
        let bad = ServiceAdvert {
            service_type: "nonsense".into(),
            ..service
        };
        assert!(advert_json(&bad).is_err());
    }

    #[test]
    fn resolved_json_is_validated() {
        let doc = ResolvedJson::parse(
            r#"{"instance":"Desk (ab12)","txt":{"v":"0","id":"x"},
                "addrs":["fd00::1","192.168.1.2","fe80::1%en0","bogus","192.168.1.2","0.0.0.0","ff02::fb"],"port":47810}"#,
        )
        .expect("parse");
        assert_eq!(doc.port, 47_810);
        let addrs: Vec<String> = doc.addrs().iter().map(ToString::to_string).collect();
        assert_eq!(
            addrs,
            ["192.168.1.2", "fd00::1"],
            "link-local and junk dropped"
        );
        assert_eq!(
            doc.txt(),
            vec![("id".into(), "x".into()), ("v".into(), "0".into())]
        );
        assert!(ResolvedJson::parse(r#"{"instance":"","port":1}"#).is_err());
        assert!(ResolvedJson::parse(r#"{"instance":"a","port":70000}"#).is_err());
        assert!(ResolvedJson::parse("not json").is_err());
        // Missing optional parts default to empty.
        let bare = ResolvedJson::parse(r#"{"instance":"a","port":1}"#).expect("parse");
        assert!(bare.addrs().is_empty() && bare.txt().is_empty());
    }

    #[test]
    fn null_and_invalid_arguments_are_rejected() {
        // SAFETY: null pointers are allowed by the contract.
        unsafe {
            assert_eq!(
                hfa_discovery_register(std::ptr::null()),
                HFA_DISCOVERY_ERR_INVALID_ARGUMENT
            );
            assert!(!hfa_discovery_last_error().is_null());
            assert_eq!(
                hfa_discovery_resolved(1, std::ptr::null()),
                HFA_DISCOVERY_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                hfa_discovery_removed(1, std::ptr::null()),
                HFA_DISCOVERY_ERR_INVALID_ARGUMENT
            );
        }
        let invalid = [0xff_u8, 0xfe, 0];
        // SAFETY: a NUL-terminated buffer that outlives the call.
        let code = unsafe { hfa_discovery_removed(1, invalid.as_ptr().cast()) };
        assert_eq!(code, HFA_DISCOVERY_ERR_INVALID_ARGUMENT);
        // SAFETY: the pointer is valid until the next call on this thread.
        let message = unsafe { CStr::from_ptr(hfa_discovery_last_error()) };
        assert_eq!(message.to_str(), Ok("instance is not UTF-8"));

        // A report for a browse that does not exist tells the native side to stop it.
        let json =
            CString::new(r#"{"instance":"a","port":1,"addrs":["10.0.0.1"]}"#).expect("no NUL");
        // SAFETY: valid C string.
        assert_eq!(
            unsafe { hfa_discovery_resolved(u64::MAX, json.as_ptr()) },
            HFA_DISCOVERY_CLOSED
        );
        assert!(hfa_discovery_last_error().is_null(), "cleared on success");
    }

    #[test]
    fn a_missing_callback_is_rejected() {
        unsafe extern "C" fn start(_: *mut c_void, _: u64) -> i32 {
            0
        }
        let callbacks = HfaDiscoveryCallbacks {
            ctx: std::ptr::null_mut(),
            browse_start: Some(start),
            browse_stop: None,
            advertise_start: None,
            advertise_stop: None,
        };
        // SAFETY: a valid struct that outlives the call.
        assert_eq!(
            unsafe { hfa_discovery_register(&callbacks) },
            HFA_DISCOVERY_ERR_INVALID_ARGUMENT
        );
        // SAFETY: set by the failed call on this thread.
        let message = unsafe { CStr::from_ptr(hfa_discovery_last_error()) };
        assert_eq!(message.to_str(), Ok("browse_stop is null"));
    }

    #[test]
    fn panics_become_internal_errors() {
        let code = boundary(|| panic!("boom"));
        assert_eq!(code, HFA_DISCOVERY_ERR_INTERNAL);
        // SAFETY: set by the call above on this thread.
        let message = unsafe { CStr::from_ptr(hfa_discovery_last_error()) };
        assert_eq!(message.to_str(), Ok("internal panic: boom"));
    }

    /// `include/hfa_discovery.h` must declare the same codes and functions as this module.
    #[test]
    fn header_matches_the_rust_definitions() {
        let header = include_str!("../include/hfa_discovery.h").replace("\r\n", "\n");
        for (name, value) in [
            ("HFA_DISCOVERY_OK", HFA_DISCOVERY_OK),
            ("HFA_DISCOVERY_CLOSED", HFA_DISCOVERY_CLOSED),
            (
                "HFA_DISCOVERY_ERR_INVALID_ARGUMENT",
                HFA_DISCOVERY_ERR_INVALID_ARGUMENT,
            ),
            ("HFA_DISCOVERY_ERR_INTERNAL", HFA_DISCOVERY_ERR_INTERNAL),
        ] {
            let line = format!("#define {name} ({value})");
            assert!(header.contains(&line), "header lacks `{line}`");
        }
        for decl in [
            "  void *ctx;",
            "  int32_t (*browse_start)(void *ctx, uint64_t browse_id);",
            "  void (*browse_stop)(void *ctx, uint64_t browse_id);",
            "  int32_t (*advertise_start)(void *ctx, uint64_t advert_id, const char *service_json);",
            "  void (*advertise_stop)(void *ctx, uint64_t advert_id);",
            "int32_t hfa_discovery_register(const HfaDiscoveryCallbacks *callbacks);",
            "int32_t hfa_discovery_unregister(void);",
            "int32_t hfa_discovery_resolved(uint64_t browse_id, const char *service_json);",
            "int32_t hfa_discovery_removed(uint64_t browse_id, const char *instance);",
            "const char *hfa_discovery_last_error(void);",
        ] {
            assert!(header.contains(decl), "header lacks `{decl}`");
        }
        // The struct's field order is the ABI.
        let order: Vec<usize> = [
            "ctx;",
            "(*browse_start)",
            "(*browse_stop)",
            "(*advertise_start)",
            "(*advertise_stop)",
        ]
        .iter()
        .map(|f| header.find(f).unwrap_or(usize::MAX))
        .collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "field order {order:?}"
        );
    }
}
