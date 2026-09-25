//! JNI exports for the Android capture service (`feat/ffi` / `feat/android`).
//!
//! The Kotlin `CaptureService` (MediaProjection + `AudioRecord` with
//! `AudioPlaybackCaptureConfiguration`) registers an external feed and pushes PCM through
//! these functions into [`hfa_capture::ExternalFeed::push`].
