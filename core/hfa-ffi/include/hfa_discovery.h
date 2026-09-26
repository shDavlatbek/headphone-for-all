/*
 * hfa_discovery.h - C ABI through which the iOS app's native Bonjour code (NWBrowser +
 * dns_sd, app/ios/Runner/HfaBonjourDiscovery.swift) becomes the discovery backend of the Rust
 * core. mdns-sd cannot run on iOS without the restricted multicast entitlement.
 *
 * Hand-written; keep in sync with core/hfa-ffi/src/native_discovery.rs (a unit test checks the
 * constants and prototypes). Exported by the app's hfa_ffi library on iOS only. See
 * docs/CONTRACTS.md section 8.9.2.
 *
 * Rust -> native: register the callbacks once at app start, before the Rust engine browses or
 * starts a hub. browse_start / browse_stop bracket one browse session (browse_id);
 * advertise_start / advertise_stop bracket one hub registration (advert_id). The advert
 * service_json is only valid during the call:
 *   {"instance": "Desk (ab12)", "type": "_hfa._tcp", "domain": "local.", "port": 47810,
 *    "txt": [["v","0"], ["id","ab12-..."], ["name","Desk"], ["platform","ios"]]}
 * A non-zero return from a *_start callback fails that browse or registration. Callbacks are
 * called from any thread (never while Rust holds a lock of this module) and must return quickly.
 *
 * Native -> Rust: while a browse runs, report every resolved instance with
 * hfa_discovery_resolved (UTF-8 JSON):
 *   {"instance": "Desk (ab12)", "txt": {"v": "0", "id": "...", "name": "Desk", "platform": "ios"},
 *    "addrs": ["192.168.1.20", "fd00::20"], "port": 47810}
 * again whenever it changes, and every vanished instance with hfa_discovery_removed. Both return
 * HFA_DISCOVERY_CLOSED once Rust stopped listening to that browse: stop it natively. A report
 * without a usable address is ignored. Rust validates the TXT record (v=0, a well-formed id).
 *
 * Errors: negative HFA_DISCOVERY_ERR_* codes; hfa_discovery_last_error() then describes the
 * failure (thread-local, cleared by every hfa_discovery_* call, valid until the next one on the
 * same thread). No function ever unwinds (panics) across this boundary.
 */
#ifndef HFA_DISCOVERY_H
#define HFA_DISCOVERY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Success. */
#define HFA_DISCOVERY_OK (0)
/* Rust no longer listens to this browse: stop it. */
#define HFA_DISCOVERY_CLOSED (1)
/* A pointer was NULL, a string not UTF-8, the JSON malformed or a callback missing. */
#define HFA_DISCOVERY_ERR_INVALID_ARGUMENT (-1)
/* A Rust panic was caught at the boundary (a bug). */
#define HFA_DISCOVERY_ERR_INTERNAL (-5)

/*
 * The native side of discovery. Every function pointer is required; the context pointer is
 * passed back unchanged (may be NULL) and must stay valid for the rest of the process.
 */
typedef struct HfaDiscoveryCallbacks {
  void *ctx;
  int32_t (*browse_start)(void *ctx, uint64_t browse_id);
  void (*browse_stop)(void *ctx, uint64_t browse_id);
  int32_t (*advertise_start)(void *ctx, uint64_t advert_id, const char *service_json);
  void (*advertise_stop)(void *ctx, uint64_t advert_id);
} HfaDiscoveryCallbacks;

/*
 * Makes the callbacks the Rust core's discovery backend (the struct is copied; a new call
 * replaces the old callbacks for later sessions). HFA_DISCOVERY_OK or
 * HFA_DISCOVERY_ERR_INVALID_ARGUMENT.
 */
int32_t hfa_discovery_register(const HfaDiscoveryCallbacks *callbacks);

/* Removes the backend (running sessions are not affected). HFA_DISCOVERY_OK. */
int32_t hfa_discovery_unregister(void);

/* A resolved instance of browse browse_id (JSON above). OK, CLOSED or an error. */
int32_t hfa_discovery_resolved(uint64_t browse_id, const char *service_json);

/* The instance (its "instance" name) of browse browse_id vanished. OK, CLOSED or an error. */
int32_t hfa_discovery_removed(uint64_t browse_id, const char *instance);

/* Message of the last failed hfa_discovery_* call on this thread, or NULL. */
const char *hfa_discovery_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* HFA_DISCOVERY_H */
