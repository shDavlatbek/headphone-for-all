//! The native discovery C ABI end to end on the host: fake "Swift" callbacks registered with
//! `hfa_discovery_register` receive hfa-core's browse and advertise requests, and reports fed
//! back through `hfa_discovery_resolved` / `hfa_discovery_removed` arrive as discovery events.
//! Own test binary (one test): the backend is process-wide.

use std::ffi::{c_char, c_void, CStr, CString};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use hfa_core::discovery::{browse, Advertiser, DiscoveryEvent};
use hfa_ffi::native_discovery::{
    hfa_discovery_last_error, hfa_discovery_register, hfa_discovery_removed,
    hfa_discovery_resolved, hfa_discovery_unregister, HfaDiscoveryCallbacks, HFA_DISCOVERY_CLOSED,
    HFA_DISCOVERY_ERR_INVALID_ARGUMENT, HFA_DISCOVERY_OK,
};
use parking_lot::Mutex;

/// What the fake native side was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    BrowseStart(u64),
    BrowseStop(u64),
    AdvertiseStart(u64, String),
    AdvertiseStop(u64),
}

static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());
/// Makes the next `*_start` callback fail.
static FAIL_NEXT: AtomicBool = AtomicBool::new(false);
/// The context pointer handed to `hfa_discovery_register`.
static CONTEXT: u8 = 0;

fn context() -> *mut c_void {
    std::ptr::addr_of!(CONTEXT).cast_mut().cast()
}

fn record(ctx: *mut c_void, call: Call) {
    assert_eq!(ctx, context(), "ctx must be passed back unchanged");
    CALLS.lock().push(call);
}

fn start_result() -> i32 {
    if FAIL_NEXT.swap(false, Ordering::SeqCst) {
        7
    } else {
        0
    }
}

unsafe extern "C" fn browse_start(ctx: *mut c_void, id: u64) -> i32 {
    record(ctx, Call::BrowseStart(id));
    start_result()
}

unsafe extern "C" fn browse_stop(ctx: *mut c_void, id: u64) {
    record(ctx, Call::BrowseStop(id));
}

unsafe extern "C" fn advertise_start(ctx: *mut c_void, id: u64, json: *const c_char) -> i32 {
    assert!(!json.is_null());
    // SAFETY: Rust passes a NUL-terminated string valid during the call.
    let text = unsafe { CStr::from_ptr(json) }
        .to_str()
        .expect("UTF-8")
        .to_owned();
    record(ctx, Call::AdvertiseStart(id, text));
    start_result()
}

unsafe extern "C" fn advertise_stop(ctx: *mut c_void, id: u64) {
    record(ctx, Call::AdvertiseStop(id));
}

fn take_calls() -> Vec<Call> {
    std::mem::take(&mut *CALLS.lock())
}

fn resolved(browse_id: u64, json: &str) -> i32 {
    let text = CString::new(json).expect("no NUL");
    // SAFETY: valid C string for the duration of the call.
    unsafe { hfa_discovery_resolved(browse_id, text.as_ptr()) }
}

fn removed(browse_id: u64, instance: &str) -> i32 {
    let text = CString::new(instance).expect("no NUL");
    // SAFETY: valid C string for the duration of the call.
    unsafe { hfa_discovery_removed(browse_id, text.as_ptr()) }
}

fn last_error() -> String {
    let ptr = hfa_discovery_last_error();
    assert!(!ptr.is_null(), "expected an error message");
    // SAFETY: valid until the next hfa_discovery_* call on this thread.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn native_backend_round_trip() {
    let callbacks = HfaDiscoveryCallbacks {
        ctx: context(),
        browse_start: Some(browse_start),
        browse_stop: Some(browse_stop),
        advertise_start: Some(advertise_start),
        advertise_stop: Some(advertise_stop),
    };
    // SAFETY: a valid struct; the functions live for the whole process.
    assert_eq!(
        unsafe { hfa_discovery_register(&callbacks) },
        HFA_DISCOVERY_OK
    );

    // --- Advertising: the hub's registration reaches the native side as JSON.
    let id = hfa_proto::fingerprint(&[9; 32]);
    let advertiser = Advertiser::start("Desk", &id, 47_810, "ios").expect("advertise");
    let calls = take_calls();
    let [Call::AdvertiseStart(advert_id, json)] = calls.as_slice() else {
        panic!("unexpected calls {calls:?}");
    };
    let doc: serde_json::Value = serde_json::from_str(json).expect("advert json");
    assert_eq!(
        doc,
        serde_json::json!({
            "instance": format!("Desk ({})", &id[..4]),
            "type": "_hfa._tcp",
            "domain": "local.",
            "port": 47810,
            "txt": [["v", "0"], ["id", id], ["name", "Desk"], ["platform", "ios"]],
        })
    );
    advertiser.stop().expect("stop");
    assert_eq!(take_calls(), [Call::AdvertiseStop(*advert_id)]);

    // A refused registration fails Advertiser::start (the hub reports it) and is not stopped.
    FAIL_NEXT.store(true, Ordering::SeqCst);
    let err = Advertiser::start("Desk", &id, 47_810, "ios").expect_err("refused");
    assert!(err.to_string().contains("code 7"), "{err}");
    assert!(matches!(
        take_calls().as_slice(),
        [Call::AdvertiseStart(..)]
    ));

    // --- Browsing.
    let mut browser = browse().expect("browse");
    let calls = take_calls();
    let [Call::BrowseStart(browse_id)] = calls.as_slice() else {
        panic!("unexpected calls {calls:?}");
    };
    let browse_id = *browse_id;
    let instance = format!("Desk ({})", &id[..4]);
    let report = serde_json::json!({
        "instance": instance,
        "txt": {"v": "0", "id": id, "name": "Desk", "platform": "macos"},
        "addrs": ["fd00::20", "192.168.1.20", "fe80::1%en0"],
        "port": 47810,
    })
    .to_string();
    assert_eq!(resolved(browse_id, &report), HFA_DISCOVERY_OK);
    match browser.try_recv() {
        Some(DiscoveryEvent::Found(hub)) => {
            assert_eq!(hub.device_id, id);
            assert_eq!(hub.name, "Desk");
            assert_eq!(hub.platform, "macos");
            assert_eq!(hub.port, 47_810);
            let expected: Vec<IpAddr> = vec![
                "192.168.1.20".parse().expect("ip"),
                "fd00::20".parse().expect("ip"),
            ];
            assert_eq!(hub.addrs, expected, "IPv4 first, scoped link-local dropped");
        }
        other => panic!("expected Found, got {other:?}"),
    }

    // Invalid reports are errors with a message and change nothing.
    assert_eq!(resolved(browse_id, "{"), HFA_DISCOVERY_ERR_INVALID_ARGUMENT);
    assert!(last_error().starts_with("service json"), "{}", last_error());
    // Not a v0 hub, or no usable address: ignored.
    let not_a_hub = r#"{"instance":"x","txt":{"v":"9"},"addrs":["10.0.0.1"],"port":1}"#;
    assert_eq!(resolved(browse_id, not_a_hub), HFA_DISCOVERY_OK);
    let no_addr = serde_json::json!({
        "instance": "y", "txt": {"v": "0", "id": id}, "addrs": ["fe80::"], "port": 1,
    })
    .to_string();
    assert_eq!(resolved(browse_id, &no_addr), HFA_DISCOVERY_OK);
    assert_eq!(browser.try_recv(), None);

    // Removal of the only instance of the id: Lost.
    assert_eq!(removed(browse_id, "unknown"), HFA_DISCOVERY_OK);
    assert_eq!(removed(browse_id, &instance), HFA_DISCOVERY_OK);
    assert_eq!(browser.try_recv(), Some(DiscoveryEvent::Lost(id.clone())));

    // Dropping the browser stops the native browse; later reports say CLOSED.
    drop(browser);
    assert_eq!(take_calls(), [Call::BrowseStop(browse_id)]);
    assert_eq!(resolved(browse_id, &report), HFA_DISCOVERY_CLOSED);
    assert_eq!(removed(browse_id, &instance), HFA_DISCOVERY_CLOSED);

    // A browse the native side refuses fails browse() and leaves nothing behind.
    FAIL_NEXT.store(true, Ordering::SeqCst);
    let err = browse().expect_err("refused");
    assert!(err.to_string().contains("code 7"), "{err}");
    let calls = take_calls();
    let [Call::BrowseStart(refused_id)] = calls.as_slice() else {
        panic!("unexpected calls {calls:?}");
    };
    assert_eq!(resolved(*refused_id, &report), HFA_DISCOVERY_CLOSED);

    assert_eq!(hfa_discovery_unregister(), HFA_DISCOVERY_OK);
    assert!(take_calls().is_empty());
}
