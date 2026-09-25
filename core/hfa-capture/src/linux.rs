//! Linux capture: PipeWire capture stream on the default sink monitor
//! (`stream.capture.sink = true`). Per-app capture is not offered (`list_apps` returns `Ok(vec![])`
//! once implemented).
//!
//! This file is owned by `feat/capture-linux`. It must expose exactly the four `pub(crate)` functions
//! below (the platform-module interface, see `docs/CONTRACTS.md` §5).

use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource};

/// Capture capabilities on Linux (stub until `feat/capture-linux` lands).
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        system_mix: false,
        per_app: false,
        mutes_local_output: false,
        notes: "Linux capture backend not implemented yet (feat/capture-linux)".to_owned(),
    }
}

/// Opens system-mix capture; with `exclude_self` the current process is excluded.
pub(crate) fn open_system(_exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "Linux system capture not implemented yet (feat/capture-linux)".to_owned(),
    ))
}

/// Opens capture of one process (tree).
pub(crate) fn open_process(_pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "Linux per-process capture not implemented yet (feat/capture-linux)".to_owned(),
    ))
}

/// Lists processes currently playing audio.
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError> {
    Err(CaptureError::Unsupported(
        "Linux app enumeration not implemented yet (feat/capture-linux)".to_owned(),
    ))
}
