/*
 * hfa_ext.h - C ABI of the headphone-for-all sender for the iOS ReplayKit broadcast
 * upload extension (link the `hfa_ffi` static library).
 *
 * Hand-written; keep in sync with core/hfa-ffi/src/c_api.rs (a unit test checks the
 * constants and prototypes). See docs/CONTRACTS.md section 8.
 *
 * config_json (UTF-8; unknown keys are ignored):
 *   {
 *     "data_dir":      "<App Group container>/hfa",   required: identity, settings, paired hubs
 *     "hub_host":      "192.168.1.20",                 "" = find hub_device_id over mDNS
 *                                                      (refused on iOS: no multicast there)
 *     "hub_port":      47810,                          0 or missing = settings port (47810 if that is 0)
 *     "hub_device_id": "ab12-cd34-ef56-7890" | null,
 *     "hub_key":       "<base64url static key>" | null,
 *     "label":         "iPhone"                        missing = "iOS audio"
 *   }
 * The hub must already be paired by the app: its key (hub_key, or the key of hub_device_id)
 * must be in the trust store, otherwise HFA_ERR_CONFIG. Pairing never happens inside the
 * extension.
 *
 * Errors: every function returns HFA_OK or a negative HFA_ERR_* code
 * (hfa_ext_sender_start: a handle or NULL). After a failure, hfa_ext_last_error() describes
 * it. The last error is thread-local and cleared by every hfa_ext_* call, so it is NULL
 * after a successful call; the returned string stays valid until the next hfa_ext_* call on
 * the same thread. No function ever unwinds (panics) across this boundary.
 */
#ifndef HFA_EXT_H
#define HFA_EXT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Success. */
#define HFA_OK (0)
/* A pointer was NULL or a number out of range (format, buffer size). */
#define HFA_ERR_INVALID_ARGUMENT (-1)
/* The configuration JSON is malformed or incomplete, or the hub is not paired. */
#define HFA_ERR_CONFIG (-2)
/* The engine failed (settings/identity I/O, capture, runtime). */
#define HFA_ERR_ENGINE (-3)
/* Unknown external feed id (Android JNI only). */
#define HFA_ERR_UNKNOWN_FEED (-4)
/* A Rust panic was caught at the boundary (a bug). */
#define HFA_ERR_INTERNAL (-5)
/* Not implemented in this build. */
#define HFA_ERR_NOT_IMPLEMENTED (-100)

/* Opaque handle to a running extension sender. */
typedef struct HfaExtSender HfaExtSender;

/*
 * Starts a sender streaming the PCM pushed with hfa_ext_push_pcm to the configured hub.
 * Returns once the engine runs (the connection is made in the background: poll
 * hfa_ext_sender_state), or NULL.
 */
HfaExtSender *hfa_ext_sender_start(const char *config_json);

/*
 * Pushes `frames` frames of interleaved float PCM in [-1, 1] with `channels` channels
 * (1..=8) at `rate` Hz (8000..=192000); at most 1536000 samples per call. Every buffer is
 * converted to 48 kHz stereo; the format may change between calls. Audio pushed while the
 * sender is still connecting is dropped (HFA_OK). Do not call it concurrently with itself
 * for the same handle (hfa_ext_sender_state may run meanwhile).
 */
int32_t hfa_ext_push_pcm(HfaExtSender *handle, const float *samples, uint32_t frames,
                         uint32_t channels, uint32_t rate);

/* Stops the sender and frees `handle` (never use it again). */
int32_t hfa_ext_sender_stop(HfaExtSender *handle);

/* Message of the last failed hfa_ext_* call on this thread, or NULL. */
const char *hfa_ext_last_error(void);

/*
 * Writes the sender's state as NUL-terminated UTF-8 JSON into buf:
 *   {"state": "connecting" | "pairing" | "streaming" | "reconnecting" | "stopped" | "failed",
 *    "error": failure reason when failed, else the last non-fatal error, or null,
 *    "hub_name": "Desk" | null, "bitrate": 128000, "loss_pct": 0.5, "rtt_ms": 3.2,
 *    "level_db": -18.5, "hub_gain": 1.0, "hub_muted": false, "hub_priority": false}
 * Returns the JSON length in bytes without the NUL, like snprintf: if it is >= len, only an
 * empty string was written (len > 0); call again with at least that length + 1. Negative:
 * an HFA_ERR_* code. "failed" is final (e.g. pairing required, key mismatch): end the
 * broadcast with the error. May run while another thread is in hfa_ext_push_pcm with the
 * same handle, but never concurrently with or after hfa_ext_sender_stop. buf may be NULL
 * only when len is 0.
 */
int32_t hfa_ext_sender_state(HfaExtSender *handle, char *buf, uint32_t len);

#ifdef __cplusplus
}
#endif

#endif /* HFA_EXT_H */
