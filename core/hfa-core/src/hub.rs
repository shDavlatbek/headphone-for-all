//! The hub engine: accepts senders, receives their media, and mixes everything into one
//! output.
//!
//! - Receiver task (tokio): UDP → [`crate::media::MediaDemux`] → per-stream jitter buffer.
//! - Mixer thread (soft real-time): paced by the output ring fill level; every `frame_ms` it
//!   tops the ring up to the target latency. Per stream: pop → decode / FEC / PLC → drift
//!   resample → mixer (gain, mute, ducking, limiter) → output ring.
//! - Sends `Stats` to each sender every [`STATS_INTERVAL`]. A stream without packets for
//!   [`IDLE_AFTER`] is marked inactive, and removed after [`REMOVE_AFTER`].

use std::fmt;
use std::time::Duration;

use hfa_capture::AudioOutput;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::config::Settings;
use crate::pairing::PairingInfo;
use crate::Result;

/// Interval between `Stats` messages to each sender.
pub const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// A stream without packets for this long is shown as inactive.
pub const IDLE_AFTER: Duration = Duration::from_secs(2);
/// A stream without packets for this long is removed.
pub const REMOVE_AFTER: Duration = Duration::from_secs(30);

/// Everything needed to start a hub.
pub struct HubConfig {
    /// Settings (port, jitter bounds, data dir...).
    pub settings: Settings,
    /// The output (not yet started; the engine starts and stops it).
    pub output: Box<dyn AudioOutput>,
    /// Advertise the hub over mDNS.
    pub advertise: bool,
}

impl fmt::Debug for HubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubConfig")
            .field("settings", &self.settings)
            .field("output_format", &self.output.format())
            .field("advertise", &self.advertise)
            .finish()
    }
}

/// Receive statistics of one stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamStats {
    /// Packet loss (after FEC) over the last second, in percent.
    pub loss_pct: f32,
    /// Interarrival jitter in ms.
    pub jitter_ms: f32,
    /// Jitter-buffer fill in ms.
    pub buffer_ms: f32,
    /// Estimated end-to-end latency in ms (buffer + frame + output latency).
    pub latency_ms: f32,
    /// Post-gain level in dBFS.
    pub level_db: f32,
}

impl Default for StreamStats {
    fn default() -> Self {
        Self {
            loss_pct: 0.0,
            jitter_ms: 0.0,
            buffer_ms: 0.0,
            latency_ms: 0.0,
            level_db: hfa_audio::meter::SILENCE_DB,
        }
    }
}

/// One incoming stream as shown in the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Stream id.
    pub stream_id: u32,
    /// Sender device id.
    pub device_id: String,
    /// Sender device name.
    pub device_name: String,
    /// Stream label.
    pub label: String,
    /// Sender platform.
    pub platform: String,
    /// Linear gain.
    pub gain: f32,
    /// Muted.
    pub muted: bool,
    /// Priority (ducks the others).
    pub priority: bool,
    /// Packets received within [`IDLE_AFTER`].
    pub active: bool,
    /// Receive statistics.
    pub stats: StreamStats,
}

/// Notifications from a running hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HubEvent {
    /// A stream started.
    SourceAdded(SourceInfo),
    /// A stream stopped or was removed after [`REMOVE_AFTER`].
    SourceRemoved {
        /// The removed stream.
        stream_id: u32,
    },
    /// A stream's controls, activity or statistics changed (stats: about once per second).
    SourceUpdated(SourceInfo),
    /// A sender paired successfully.
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
    Error(String),
}

/// Entry point of the hub engine.
pub struct HubEngine;

impl HubEngine {
    /// Starts the hub on the current tokio runtime: binds TCP and UDP on `settings.port`
    /// (0 = any free port, see [`HubHandle::local_port`]), starts the output and the mixer
    /// thread, and advertises over mDNS if requested.
    ///
    /// # Errors
    /// Bind, output or identity errors.
    pub async fn start(_config: HubConfig) -> Result<HubHandle> {
        todo!("feat/core-engine")
    }
}

/// Handle to a running hub. `Send + Sync`.
pub struct HubHandle {
    events: broadcast::Sender<HubEvent>,
    local_port: u16,
    device_id: String,
}

impl HubHandle {
    /// All current streams.
    pub fn sources(&self) -> Vec<SourceInfo> {
        todo!("feat/core-engine")
    }

    /// Sets a stream's linear gain (0.0..=4.0) and informs the sender.
    ///
    /// # Errors
    /// [`crate::CoreError::UnknownStream`].
    pub fn set_gain(&self, _stream_id: u32, _gain: f32) -> Result<()> {
        todo!("feat/core-engine")
    }

    /// Mutes/unmutes a stream and informs the sender.
    ///
    /// # Errors
    /// [`crate::CoreError::UnknownStream`].
    pub fn set_muted(&self, _stream_id: u32, _muted: bool) -> Result<()> {
        todo!("feat/core-engine")
    }

    /// Marks a stream as priority (ducks the others) and informs the sender.
    ///
    /// # Errors
    /// [`crate::CoreError::UnknownStream`].
    pub fn set_priority(&self, _stream_id: u32, _priority: bool) -> Result<()> {
        todo!("feat/core-engine")
    }

    /// Sets the master linear gain (0.0..=4.0).
    pub fn set_master_gain(&self, _gain: f32) {
        todo!("feat/core-engine")
    }

    /// Opens a pairing window ([`crate::pairing::DEFAULT_PAIRING_TTL`]).
    pub fn start_pairing(&self) -> PairingInfo {
        todo!("feat/core-engine")
    }

    /// Closes the pairing window.
    pub fn cancel_pairing(&self) {
        todo!("feat/core-engine")
    }

    /// Subscribes to events.
    pub fn events(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    /// The bound TCP/UDP port.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// This hub's device id.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Disconnects all senders (`Bye`), stops advertising, the mixer and the output, and
    /// waits for all tasks.
    pub async fn stop(self) {
        todo!("feat/core-engine")
    }
}
