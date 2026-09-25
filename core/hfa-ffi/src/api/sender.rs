//! Sender control: hub discovery, capture sources, start/stop and status.

use crate::frb_generated::StreamSink;
use crate::manager::manager;

/// A hub found on the LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubInfoDto {
    /// Hub device id (pass it as `SenderStartDto.hub_device_id`).
    pub device_id: String,
    /// Hub display name.
    pub name: String,
    /// Resolved addresses (IPv4 first).
    pub addrs: Vec<String>,
    /// TCP control port.
    pub port: u16,
    /// Hub platform.
    pub platform: String,
    /// The hub is paired with this device (no PIN needed).
    pub trusted: bool,
}

/// A change in the set of visible hubs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEventDto {
    /// A hub appeared or changed.
    Found(HubInfoDto),
    /// A hub disappeared.
    Lost {
        /// Its device id.
        device_id: String,
    },
}

/// A process that can be captured on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureAppDto {
    /// Process id.
    pub pid: u32,
    /// Display name.
    pub name: String,
}

/// What to capture.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureSourceDto {
    /// Everything this device plays.
    System,
    /// Everything this device plays except this app.
    SystemExcludingSelf,
    /// One process (and its children where supported).
    Process {
        /// Process id (from `list_capture_apps`).
        pid: u32,
    },
    /// A test tone.
    Tone {
        /// Frequency in Hz, 0 < f < 24000.
        freq_hz: f32,
    },
    /// PCM pushed by native code (Android `NativeBridge.pushPcm`) into feed `feed_id`, in
    /// exactly this format.
    External {
        /// Feed id shared with the native side.
        feed_id: u32,
        /// Sample rate of the pushed PCM (8000..=192000).
        sample_rate: u32,
        /// Channels of the pushed PCM (1..=8).
        channels: u16,
    },
}

/// How to start a sender.
#[derive(Debug, Clone, PartialEq)]
pub struct SenderStartDto {
    /// Hub host name or IP; empty = find the hub by `hub_device_id` over mDNS.
    pub hub_host: String,
    /// Hub port; 0 = the port from the settings (the protocol default if that is 0 too).
    pub hub_port: u16,
    /// Hub device id (from discovery or a trusted peer). When `hub_key` is `None`, the key of
    /// this trusted peer is pinned.
    pub hub_device_id: Option<String>,
    /// Hub static key, base64url (`PairingUriDto.hub_id`).
    pub hub_key: Option<String>,
    /// PIN or token; required when the hub is not paired yet.
    pub pairing_secret: Option<String>,
    /// What to capture.
    pub source: CaptureSourceDto,
    /// Label shown on the hub; empty = a default for the source.
    pub label: String,
}

/// Sender state for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct SenderStatusDto {
    /// `idle` (no sender), `connecting`, `pairing`, `streaming`, `reconnecting`, `stopped`
    /// or `failed`.
    pub state: String,
    /// Reason when `failed`, else the last non-fatal error, if any.
    pub error: Option<String>,
    /// Name of the connected hub, once known.
    pub hub_name: Option<String>,
    /// Current Opus bitrate (bit/s).
    pub bitrate: u32,
    /// Loss reported by the hub, percent.
    pub loss_pct: f32,
    /// Control round-trip time, ms.
    pub rtt_ms: f32,
    /// Capture level, dBFS (-120 = silence).
    pub level_db: f32,
    /// Linear gain the hub applies to this stream (1.0 = unchanged; set on the hub).
    pub hub_gain: f32,
    /// The hub muted this stream: it is not heard there although it is sent.
    pub hub_muted: bool,
    /// The hub made this stream a priority source (it ducks the others).
    pub hub_priority: bool,
}

/// Browses for hubs until `stop_discovery` or until the Dart stream is cancelled. A new call
/// replaces (and closes) the previous discovery stream.
pub fn discover_hubs(sink: StreamSink<DiscoveryEventDto>) -> anyhow::Result<()> {
    Ok(manager()?.discover_hubs(Box::new(sink))?)
}

/// Stops browsing and closes the discovery stream.
pub fn stop_discovery() -> anyhow::Result<()> {
    manager()?.stop_discovery();
    Ok(())
}

/// Processes that can be captured individually (empty where unsupported).
pub fn list_capture_apps() -> anyhow::Result<Vec<CaptureAppDto>> {
    Ok(hfa_capture::list_capture_apps()?
        .into_iter()
        .map(|a| CaptureAppDto {
            pid: a.pid,
            name: a.name,
        })
        .collect())
}

/// Starts streaming to a hub. Returns once the capture is open and the engine started; the
/// connection progress arrives through `sender_status` / `sender_events`.
///
/// Fails with "a sender is already running" while a sender is live. A sender that ended by
/// itself (`failed`, e.g. pairing required or a wrong PIN, or `stopped`) is stopped and
/// replaced, so after a pairing failure just call this again with the PIN.
pub fn sender_start(request: SenderStartDto) -> anyhow::Result<()> {
    Ok(manager()?.sender_start(request)?)
}

/// Stops the sender (no-op if none runs).
pub fn sender_stop() -> anyhow::Result<()> {
    Ok(manager()?.sender_stop()?)
}

/// Current sender status (`state: "idle"` when none runs).
pub fn sender_status() -> SenderStatusDto {
    manager()
        .map(|m| m.sender_status())
        .unwrap_or_else(|_| crate::convert::idle_sender_status())
}

/// Subscribes to sender status updates: the current status right away, then one per engine
/// event (state changes, about once per second while streaming). The subscription lives
/// until the Dart stream is cancelled and survives sender restarts.
pub fn sender_events(sink: StreamSink<SenderStatusDto>) -> anyhow::Result<()> {
    manager()?.sender_events(Box::new(sink));
    Ok(())
}
