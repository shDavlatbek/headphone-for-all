//! flutter_rust_bridge v2 API surface (docs/CONTRACTS.md §8.1).
//!
//! Every public function here becomes a Dart function (lowerCamelCase) in
//! `app/lib/src/rust/api/`; every public struct / enum used by them becomes a Dart class
//! (enums with fields are `freezed` sealed classes). The engine types never cross the
//! boundary: the functions return DTOs converted in [`crate::convert`], and all state lives
//! in the global [`crate::manager::EngineManager`].
//!
//! Threading: none of these functions is `#[frb(sync)]`, so Dart receives `Future`s and the
//! calls run on flutter_rust_bridge's worker pool, never on the UI thread (starting a hub or
//! a capture can block for a few seconds).
//!
//! Errors are `anyhow::Error` (Dart: `AnyhowException`) whose message is the
//! [`crate::FfiError`] text.

pub mod app;
pub mod hub;
pub mod sender;
