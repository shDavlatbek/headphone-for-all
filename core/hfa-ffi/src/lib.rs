//! # hfa-ffi
//!
//! Foreign-function surface of headphone-for-all:
//!
//! - [`api`]: the `flutter_rust_bridge` v2 API used by the Flutter app (one global engine
//!   manager that owns a tokio runtime).
//! - [`c_api`]: a small C ABI for the iOS ReplayKit broadcast extension (sender only; no
//!   Flutter in the extension).
//! - `android` (Android only): JNI exports for the Kotlin capture service, pushing PCM into an
//!   [`hfa_capture::ExternalFeed`].
//!
//! Built as `cdylib` (Android, desktop), `staticlib` (iOS) and `rlib` (tests).
//! See `docs/CONTRACTS.md` §8.

pub mod api;
pub mod c_api;

#[cfg(target_os = "android")]
pub mod android;
