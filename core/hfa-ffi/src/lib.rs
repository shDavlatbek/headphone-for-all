//! # hfa-ffi
//!
//! Foreign-function surface of headphone-for-all:
//!
//! - [`api`]: the `flutter_rust_bridge` v2 API used by the Flutter app. It is backed by one
//!   global engine manager (`manager.rs`) that owns a tokio runtime, the loaded settings,
//!   identity and trust store, and at most one hub and one sender.
//! - [`c_api`]: a small C ABI for the iOS ReplayKit broadcast extension (sender only; no
//!   Flutter in the extension). Header: `include/hfa_ext.h`.
//! - `android` (Android only): JNI exports for the Kotlin capture service, pushing PCM into an
//!   [`hfa_capture::ExternalFeed`].
//!
//! Built as `cdylib` (Android, desktop), `staticlib` (iOS) and `rlib` (tests).
//! See `docs/CONTRACTS.md` §8.
//!
//! Feature `flutter` (default) compiles the flutter_rust_bridge API; without it only the C
//! ABI and the JNI exports remain (used for `cargo check` of Apple targets on Linux, where
//! flutter_rust_bridge's C shims cannot be built).

#[cfg(feature = "flutter")]
pub mod api;
pub mod c_api;
#[cfg(feature = "flutter")]
mod convert;
pub mod error;
mod feeds;
#[cfg(feature = "flutter")]
mod frb_generated;
mod hub_target;
mod logging;
#[cfg(feature = "flutter")]
mod manager;
pub mod pcm;
mod runtime;

#[cfg(target_os = "android")]
pub mod android;

pub use error::FfiError;
