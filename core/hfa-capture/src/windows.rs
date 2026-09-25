//! Windows capture: WASAPI loopback of the default render endpoint, process loopback via
//! `ActivateAudioInterfaceAsync` + `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` (include or
//! exclude a process tree; `exclude_self` uses exclude mode with our own PID), and audio session
//! enumeration for `list_apps`.
//!
//! This file is owned by `feat/capture-windows`. It must expose exactly the four `pub(crate)` functions
//! below (the platform-module interface, see `docs/CONTRACTS.md` §5).

use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource};

/// Capture capabilities on Windows (stub until `feat/capture-windows` lands).
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        system_mix: false,
        per_app: false,
        mutes_local_output: false,
        notes: "Windows capture backend not implemented yet (feat/capture-windows)".to_owned(),
    }
}

/// Opens system-mix capture; with `exclude_self` the current process is excluded.
pub(crate) fn open_system(_exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "Windows system capture not implemented yet (feat/capture-windows)".to_owned(),
    ))
}

/// Opens capture of one process (tree).
pub(crate) fn open_process(_pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "Windows per-process capture not implemented yet (feat/capture-windows)".to_owned(),
    ))
}

/// Lists processes currently playing audio.
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError> {
    Err(CaptureError::Unsupported(
        "Windows app enumeration not implemented yet (feat/capture-windows)".to_owned(),
    ))
}
