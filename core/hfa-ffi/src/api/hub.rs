//! Hub control: start/stop, sources, mixer controls, pairing and events.

use crate::frb_generated::StreamSink;
use crate::manager::manager;

/// Hub state for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubStatusDto {
    /// The hub is running.
    pub running: bool,
    /// Bound TCP/UDP port (0 when not running).
    pub port: u16,
    /// This device's name as shown to senders.
    pub device_name: String,
    /// Number of current streams.
    pub source_count: u32,
    /// The hub is announced over mDNS, so senders on the network can find it. `false` while
    /// stopped, or when advertising failed (see `advertise_error`): senders must then enter
    /// the hub's address.
    pub advertised: bool,
    /// Why advertising failed (e.g. no multicast on iOS without the entitlement), if it did.
    pub advertise_error: Option<String>,
}

/// One incoming stream.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDto {
    /// Stream id (use it with the `hub_set_*` functions).
    pub stream_id: u32,
    /// Sender device id.
    pub device_id: String,
    /// Sender device name.
    pub device_name: String,
    /// Stream label ("System audio", an app name...).
    pub label: String,
    /// Sender platform.
    pub platform: String,
    /// Linear gain 0.0..=4.0.
    pub gain: f32,
    /// Muted on the hub.
    pub muted: bool,
    /// Priority source (ducks the others).
    pub priority: bool,
    /// Packets received within the last 2 s.
    pub active: bool,
    /// Loss after FEC over the last second, percent.
    pub loss_pct: f32,
    /// Interarrival jitter, ms.
    pub jitter_ms: f32,
    /// Jitter-buffer fill, ms.
    pub buffer_ms: f32,
    /// Estimated end-to-end latency, ms.
    pub latency_ms: f32,
    /// Post-gain level, dBFS (-120 = silence).
    pub level_db: f32,
}

/// An open pairing window. Show `pin` and a QR code of `uri`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingInfoDto {
    /// 6-digit PIN.
    pub pin: String,
    /// One-time token (also inside `uri`).
    pub token: String,
    /// `hfa://pair?...` URI for the QR code.
    pub uri: String,
    /// Unix time (seconds) when the window closes.
    pub expires_at_unix: i64,
}

/// Hub notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum HubEventDto {
    /// A stream started.
    SourceAdded(SourceDto),
    /// A stream stopped or timed out.
    SourceRemoved {
        /// The removed stream.
        stream_id: u32,
    },
    /// Controls, activity or statistics of a stream changed (about once per second).
    SourceUpdated(SourceDto),
    /// A sender paired.
    PairingCompleted {
        /// Sender device id.
        device_id: String,
        /// Sender name.
        name: String,
    },
    /// A pairing attempt failed.
    PairingFailed {
        /// Human-readable reason.
        reason: String,
    },
    /// A non-fatal error.
    Error {
        /// Human-readable message.
        message: String,
    },
}

/// Starts the hub (output from the settings, mDNS advertising on). Idempotent: returns the
/// status of the running hub. A failure to advertise does not fail the start: it shows in
/// `HubStatusDto.advertised` / `advertise_error` and as a `HubEventDto::Error`.
pub fn hub_start() -> anyhow::Result<HubStatusDto> {
    Ok(manager()?.hub_start()?)
}

/// Stops the hub (no-op if it is not running).
pub fn hub_stop() -> anyhow::Result<()> {
    Ok(manager()?.hub_stop()?)
}

/// Current hub status (`running: false` when stopped).
pub fn hub_status() -> HubStatusDto {
    manager()
        .map(|m| m.hub_status())
        .unwrap_or_else(|_| crate::convert::stopped_hub_status(String::new()))
}

/// Current streams (empty when the hub is stopped).
pub fn hub_sources() -> Vec<SourceDto> {
    manager().map(|m| m.hub_sources()).unwrap_or_default()
}

/// Sets a stream's linear gain (0.0..=4.0).
pub fn hub_set_gain(stream_id: u32, gain: f32) -> anyhow::Result<()> {
    Ok(manager()?.hub_set_gain(stream_id, gain)?)
}

/// Mutes or unmutes a stream.
pub fn hub_set_muted(stream_id: u32, muted: bool) -> anyhow::Result<()> {
    Ok(manager()?.hub_set_muted(stream_id, muted)?)
}

/// Marks a stream as priority (it ducks the others while it plays).
pub fn hub_set_priority(stream_id: u32, priority: bool) -> anyhow::Result<()> {
    Ok(manager()?.hub_set_priority(stream_id, priority)?)
}

/// Sets the master linear gain (0.0..=4.0).
pub fn hub_set_master_gain(gain: f32) -> anyhow::Result<()> {
    Ok(manager()?.hub_set_master_gain(gain)?)
}

/// Opens a pairing window (5 minutes, one-time PIN and token).
pub fn hub_start_pairing() -> anyhow::Result<PairingInfoDto> {
    Ok(manager()?.hub_start_pairing()?)
}

/// The open pairing window, or `None`: none was opened, it was cancelled, it expired, a
/// sender paired, or the hub closed it after 5 failed attempts (a `PairingFailed` event does
/// not say which). `None` while the hub is stopped.
pub fn hub_pairing_status() -> anyhow::Result<Option<PairingInfoDto>> {
    Ok(manager()?.hub_pairing_status())
}

/// Closes the pairing window.
pub fn hub_cancel_pairing() -> anyhow::Result<()> {
    Ok(manager()?.hub_cancel_pairing()?)
}

/// Subscribes to hub events. The subscription lives until the Dart stream is cancelled and
/// survives hub restarts (events flow while a hub runs).
pub fn hub_events(sink: StreamSink<HubEventDto>) -> anyhow::Result<()> {
    manager()?.hub_events(Box::new(sink));
    Ok(())
}
