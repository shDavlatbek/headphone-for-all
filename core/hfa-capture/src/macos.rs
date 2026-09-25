//! macOS capture: Core Audio process taps (`AudioHardwareCreateProcessTap` with a
//! `CATapDescription`, a private aggregate device and an IOProc; `muteBehavior =
//! mutedWhenTapped`). Requires macOS 14.2+ and the "System audio recording" permission.
//!
//! This file is owned by `feat/capture-macos`. It must expose exactly the four `pub(crate)` functions
//! below (the platform-module interface, see `docs/CONTRACTS.md` §5).

use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource};

/// Capture capabilities on macOS (stub until `feat/capture-macos` lands).
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        system_mix: false,
        per_app: false,
        mutes_local_output: false,
        notes: "macOS capture backend not implemented yet (feat/capture-macos)".to_owned(),
    }
}

/// Opens system-mix capture; with `exclude_self` the current process is excluded.
pub(crate) fn open_system(_exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "macOS system capture not implemented yet (feat/capture-macos)".to_owned(),
    ))
}

/// Opens capture of one process (tree).
pub(crate) fn open_process(_pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Err(CaptureError::Unsupported(
        "macOS per-process capture not implemented yet (feat/capture-macos)".to_owned(),
    ))
}

/// Lists processes currently playing audio.
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError> {
    Err(CaptureError::Unsupported(
        "macOS app enumeration not implemented yet (feat/capture-macos)".to_owned(),
    ))
}
