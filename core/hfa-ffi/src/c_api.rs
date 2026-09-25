//! C ABI for the iOS ReplayKit broadcast upload extension.
//!
//! The extension (Swift, `app/ios/BroadcastExtension`) links the `hfa_ffi` static library and
//! calls:
//!
//! ```c
//! typedef struct HfaExtSender HfaExtSender;
//! HfaExtSender *hfa_ext_sender_start(const char *config_json);
//! int32_t hfa_ext_push_pcm(HfaExtSender *handle, const float *samples,
//!                          uint32_t frames, uint32_t channels, uint32_t rate);
//! int32_t hfa_ext_sender_stop(HfaExtSender *handle);
//! ```
//!
//! `config_json` is defined by `feat/ffi` (hub address, device name, data directory in the
//! App Group container, label, bitrate...). Every function returns [`HFA_OK`] or a negative
//! `HFA_ERR_*` code; `hfa_ext_sender_start` returns null on failure. No function panics
//! across the FFI boundary.

use std::ffi::c_char;

/// Success.
pub const HFA_OK: i32 = 0;
/// A pointer argument was null or a numeric argument was out of range.
pub const HFA_ERR_INVALID_ARGUMENT: i32 = -1;
/// The configuration JSON could not be parsed.
pub const HFA_ERR_CONFIG: i32 = -2;
/// The engine failed (connection, pairing, internal error).
pub const HFA_ERR_ENGINE: i32 = -3;
/// The function is not implemented in this build.
pub const HFA_ERR_NOT_IMPLEMENTED: i32 = -100;

/// Opaque handle to a running extension sender. Created by [`hfa_ext_sender_start`] and
/// destroyed by [`hfa_ext_sender_stop`].
pub struct HfaExtSender {
    _private: (),
}

/// Starts a sender that streams PCM pushed with [`hfa_ext_push_pcm`] to the configured hub.
/// Returns null on failure.
///
/// # Safety
/// `config_json` must be null or a valid, NUL-terminated UTF-8 C string that stays valid for
/// the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_sender_start(config_json: *const c_char) -> *mut HfaExtSender {
    let _ = config_json;
    // Implemented by feat/ffi.
    std::ptr::null_mut()
}

/// Pushes `frames` frames of interleaved `f32` PCM (`channels` channels at `rate` Hz).
/// Returns [`HFA_OK`] or a negative error code.
///
/// # Safety
/// `handle` must be null or a pointer returned by [`hfa_ext_sender_start`] that has not been
/// passed to [`hfa_ext_sender_stop`]. `samples` must be null or point to at least
/// `frames * channels` readable `f32` values.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_push_pcm(
    handle: *mut HfaExtSender,
    samples: *const f32,
    frames: u32,
    channels: u32,
    rate: u32,
) -> i32 {
    let _ = (frames, channels, rate);
    if handle.is_null() || samples.is_null() {
        return HFA_ERR_INVALID_ARGUMENT;
    }
    // Implemented by feat/ffi.
    HFA_ERR_NOT_IMPLEMENTED
}

/// Stops the sender and frees the handle. Returns [`HFA_OK`] or a negative error code.
///
/// # Safety
/// `handle` must be null or a pointer returned by [`hfa_ext_sender_start`]; it must not be
/// used again after this call.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_sender_stop(handle: *mut HfaExtSender) -> i32 {
    if handle.is_null() {
        return HFA_ERR_INVALID_ARGUMENT;
    }
    // Implemented by feat/ffi.
    HFA_ERR_NOT_IMPLEMENTED
}
