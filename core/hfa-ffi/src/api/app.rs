//! App lifecycle, settings, trust store and pairing-URI parsing.

use std::path::Path;

use flutter_rust_bridge::frb;

use crate::convert;
use crate::manager::manager;

/// What the app shows about this device.
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    /// Fingerprint of this device's static key (`ab12-cd34-ef56-7890`).
    pub device_id: String,
    /// Name shown to other devices.
    pub device_name: String,
    /// `windows`, `macos`, `linux`, `android`, `ios` or `unknown`.
    pub platform: String,
    /// App (crate) version.
    pub version: String,
    /// What capture can do here.
    pub capabilities: CapabilitiesDto,
}

/// Capture capabilities of this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitiesDto {
    /// Rust can capture the system mix (`CaptureSourceDto::System` / `SystemExcludingSelf`).
    pub system_mix: bool,
    /// Rust can capture single processes (`CaptureSourceDto::Process`, `list_capture_apps`).
    pub per_app: bool,
    /// Capturing silences the local speakers (macOS process taps).
    pub mutes_local_output: bool,
    /// Capture only works through native code pushing into an external feed (Android
    /// `CaptureService`, iOS broadcast extension): use `CaptureSourceDto::External`.
    pub external_only: bool,
    /// Human-readable caveats.
    pub notes: String,
}

/// Editable settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDto {
    /// Name shown to other devices (1..=64 characters, no control characters).
    pub device_name: String,
    /// Hub TCP/UDP port (0 = any free port) and default port for senders.
    pub port: u16,
    /// Initial Opus bitrate, 6000..=510000 bit/s.
    pub bitrate: u32,
    /// Opus frame length: 10 or 20 ms.
    pub frame_ms: u8,
    /// Opus in-band FEC.
    pub fec: bool,
    /// Minimum jitter-buffer target (ms), 1..=`jitter_max_ms`.
    pub jitter_min_ms: u32,
    /// Maximum jitter-buffer target (ms), at most 2000.
    pub jitter_max_ms: u32,
    /// Hub output device name (`list_output_devices`); `None` = the OS default output.
    pub output_device: Option<String>,
}

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedPeerDto {
    /// Device id (fingerprint).
    pub device_id: String,
    /// Name at pairing time.
    pub name: String,
    /// Unix time (seconds) of pairing.
    pub paired_at_unix: i64,
}

/// A parsed `hfa://pair?...` URI (from a QR code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingUriDto {
    /// Hub host (IPv6 without brackets).
    pub host: String,
    /// Hub port.
    pub port: u16,
    /// Hub static key, unpadded base64url: pass it as `SenderStartDto.hub_key`.
    pub hub_id: String,
    /// Hub device id (fingerprint of `hub_id`).
    pub hub_device_id: String,
    /// One-time pairing token: pass it as `SenderStartDto.pairing_secret`.
    pub token: String,
    /// Hub display name.
    pub name: String,
}

/// flutter_rust_bridge init hook, run by `RustLib.init()`: records panic backtraces and, on
/// Android/iOS, forwards Rust logs to logcat / os_log.
#[frb(init)]
pub fn init_bridge() {
    flutter_rust_bridge::setup_backtrace();
    crate::logging::init_platform_logger();
}

/// Loads (or creates) settings, identity and trust store in `data_dir` and sets up logging.
///
/// `device_name` is the name to use **on first run** (no `settings.json` yet), e.g. the
/// device model on mobile where the OS host name is useless; afterwards the saved name wins
/// (change it with `update_settings`). Idempotent: calling it again with the same
/// `data_dir` returns the current info; another `data_dir` is only accepted while neither a
/// hub nor a sender runs.
pub fn init_app(data_dir: String, device_name: Option<String>) -> anyhow::Result<AppInfo> {
    Ok(manager()?.init_app(Path::new(&data_dir), device_name.as_deref())?)
}

/// The current settings.
pub fn get_settings() -> anyhow::Result<SettingsDto> {
    Ok(manager()?.get_settings()?)
}

/// Validates, saves and applies settings. A running hub or sender keeps its settings until it
/// is restarted.
pub fn update_settings(settings: SettingsDto) -> anyhow::Result<()> {
    Ok(manager()?.update_settings(settings)?)
}

/// Names of the available output devices (for `SettingsDto.output_device`).
pub fn list_output_devices() -> anyhow::Result<Vec<String>> {
    Ok(hfa_capture::list_output_devices()?)
}

/// The paired devices.
pub fn trusted_peers() -> anyhow::Result<Vec<TrustedPeerDto>> {
    Ok(manager()?.trusted_peers()?)
}

/// Removes a paired device (no-op for an unknown id). A running hub or sender keeps its own
/// copy of the trust store until it is restarted.
pub fn forget_peer(device_id: String) -> anyhow::Result<()> {
    Ok(manager()?.forget_peer(&device_id)?)
}

/// Parses a pairing URI scanned from a QR code.
pub fn parse_pairing_uri(uri: String) -> anyhow::Result<PairingUriDto> {
    let parsed: hfa_proto::PairingUri = uri.parse()?;
    Ok(convert::pairing_uri_dto(&parsed))
}
