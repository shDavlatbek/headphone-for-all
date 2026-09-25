//! Platform module for every target without an in-Rust capture backend (Android, iOS, BSDs...).
//!
//! On Android and iOS, system audio is captured natively (MediaProjection + `AudioRecord` in
//! Kotlin, ReplayKit in the Swift broadcast extension) and pushed into Rust through
//! [`crate::register_external`], so there is nothing to do here. This module is final.

use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource};

fn notes() -> &'static str {
    if cfg!(target_os = "android") {
        "Android captures system audio in the Kotlin CaptureService (MediaProjection + \
         AudioPlaybackCapture, Android 10+) and feeds it through an external feed. Apps can opt \
         out of capture and calls cannot be captured."
    } else if cfg!(target_os = "ios") {
        "iOS captures app audio only through the ReplayKit broadcast extension, which feeds it \
         through an external feed. DRM-protected audio is silent."
    } else {
        "System audio capture is not supported on this platform."
    }
}

/// No native Rust capture on this platform.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        system_mix: false,
        per_app: false,
        mutes_local_output: false,
        notes: notes().to_owned(),
    }
}

/// Always [`CaptureError::Unsupported`]; use an external feed instead.
pub(crate) fn open_system(_exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(notes().to_owned()))
}

/// Always [`CaptureError::Unsupported`].
pub(crate) fn open_process(_pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "per-application capture is not supported on this platform".to_owned(),
    ))
}

/// No per-app capture here: always an empty list.
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError> {
    Ok(Vec::new())
}
