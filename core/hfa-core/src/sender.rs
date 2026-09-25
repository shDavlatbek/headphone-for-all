//! The sender engine: capture → (convert to 48 kHz stereo) → Opus → seal → UDP, controlled
//! over a [`crate::ControlChannel`].
//!
//! - Sends DTX keep-alives (`FLAG_DTX`, empty payload) during silence.
//! - Adapts bitrate, FEC and expected loss from the hub's `Stats`.
//! - Reconnects with exponential backoff (1 s .. 30 s) after connection loss.
//! - The encoder runs on a dedicated soft-real-time thread; networking on tokio tasks.

use std::fmt;

use hfa_capture::CaptureSource;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::config::Settings;
use crate::Result;

/// How to reach the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HubAddress {
    /// A host name or IP address and TCP port (`--to host[:port]`, pairing URI).
    Direct {
        /// Host name or IP address.
        host: String,
        /// TCP control port.
        port: u16,
    },
    /// Resolve through mDNS by device id or display name (`--hub <name-or-id>`).
    ///
    /// mDNS records are unauthenticated, so: when several hubs match a name, trusted hubs
    /// (in the sender's [`crate::TrustStore`]) are preferred; when `name_or_id` is a device
    /// id, the engine checks after the handshake that the fingerprint of the hub's Noise
    /// static key equals it and fails with [`crate::CoreError::KeyMismatch`] otherwise.
    Discover {
        /// Device id (exact) or display name (case-insensitive).
        name_or_id: String,
    },
}

/// Everything needed to start a sender.
pub struct SenderConfig {
    /// Hub to stream to.
    pub hub: HubAddress,
    /// Settings (bitrate, frame size, FEC, data dir for identity/trust store...).
    pub settings: Settings,
    /// The capture source (not yet started; the engine starts and stops it).
    pub capture: Box<dyn CaptureSource>,
    /// Label shown on the hub (e.g. "System audio").
    pub label: String,
    /// The hub's Noise static public key, if known in advance (`PairingUri::hub_id`, or a
    /// trusted peer chosen by the caller). Passed to [`crate::ControlChannel::connect`], which
    /// fails with [`crate::CoreError::KeyMismatch`] if the hub presents another key.
    pub expected_hub_key: Option<[u8; 32]>,
    /// PIN or token, needed whenever pairing is: this device does not trust the hub yet, or
    /// the hub does not trust this device. Without it such a connection fails with
    /// [`crate::CoreError::PairingRequired`]; the sender never streams to an untrusted hub.
    pub pairing_secret: Option<String>,
}

impl fmt::Debug for SenderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SenderConfig")
            .field("hub", &self.hub)
            .field("settings", &self.settings)
            .field("capture", &self.capture.describe())
            .field("label", &self.label)
            .field("expected_hub_key", &self.expected_hub_key)
            .field(
                "pairing_secret",
                &self.pairing_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Sender lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SenderState {
    /// Resolving/connecting to the hub.
    Connecting,
    /// Pairing with the hub.
    Pairing,
    /// Streaming audio.
    Streaming,
    /// Connection lost; waiting to reconnect.
    Reconnecting,
    /// Stopped by the user.
    Stopped,
    /// Gave up (e.g. pairing failed, key mismatch).
    Failed(String),
}

/// Snapshot of the sender's state and metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SenderStatus {
    /// Lifecycle state.
    pub state: SenderState,
    /// Current Opus bitrate in bits per second.
    pub bitrate: u32,
    /// Loss reported by the hub, in percent.
    pub loss_pct: f32,
    /// Control-channel round-trip time in ms.
    pub rtt_ms: f32,
    /// Current capture level in dBFS.
    pub level_db: f32,
}

impl Default for SenderStatus {
    fn default() -> Self {
        Self {
            state: SenderState::Connecting,
            bitrate: 0,
            loss_pct: 0.0,
            rtt_ms: 0.0,
            level_db: hfa_audio::meter::SILENCE_DB,
        }
    }
}

/// Notifications from a running sender.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SenderEvent {
    /// The lifecycle state changed.
    StateChanged(SenderState),
    /// Connected to (and authenticated) the hub.
    Connected {
        /// Hub device id.
        device_id: String,
        /// Hub name.
        name: String,
    },
    /// Pairing with the hub succeeded (it is now trusted).
    Paired {
        /// Hub device id.
        device_id: String,
        /// Hub name.
        name: String,
    },
    /// The hub changed this stream's volume/mute/priority (informational).
    HubControl {
        /// Linear gain on the hub.
        gain: f32,
        /// Muted on the hub.
        muted: bool,
        /// Priority on the hub.
        priority: bool,
    },
    /// Periodic status (about once per second).
    Status(SenderStatus),
    /// A non-fatal error.
    Error(String),
}

/// Entry point of the sender engine.
pub struct SenderEngine;

impl SenderEngine {
    /// Starts the sender on the current tokio runtime. Loads the identity and trust store from
    /// `settings.data_dir`. Returns immediately; connection progress is reported through
    /// [`SenderHandle::status`] and [`SenderHandle::events`].
    ///
    /// # Errors
    /// Invalid settings or identity/trust store I/O errors.
    pub async fn start(_config: SenderConfig) -> Result<SenderHandle> {
        todo!("feat/core-engine")
    }
}

/// Handle to a running sender. `Send + Sync`.
pub struct SenderHandle {
    events: broadcast::Sender<SenderEvent>,
}

impl SenderHandle {
    /// Current status snapshot.
    pub fn status(&self) -> SenderStatus {
        todo!("feat/core-engine")
    }

    /// Subscribes to events.
    pub fn events(&self) -> broadcast::Receiver<SenderEvent> {
        self.events.subscribe()
    }

    /// Stops streaming (sends `StreamStop` + `Bye`), stops the capture and waits for all tasks.
    pub async fn stop(self) {
        todo!("feat/core-engine")
    }
}
