//! Sender facts learned from its events, shared by the flutter_rust_bridge manager and the C
//! ABI: [`hfa_core::SenderHandle::status`] has no hub name, no last error and no hub-side
//! controls, so each front end folds the engine's [`SenderEvent`]s into a [`SenderMeta`].

use hfa_core::{SenderEvent, SenderState, SenderStatus};

/// What the hub applies to this device's stream (`SetVolume` / `SetMute` / `SetPriority`,
/// reported as [`SenderEvent::HubControl`]). The engine sends the complete set whenever it
/// changes, including when a new stream after a reconnect has other controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HubControls {
    /// Linear gain on the hub (1.0 = unchanged).
    pub gain: f32,
    /// Muted on the hub: the hub plays nothing of this stream.
    pub muted: bool,
    /// Priority source on the hub (ducks the others).
    pub priority: bool,
}

impl Default for HubControls {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
            priority: false,
        }
    }
}

/// A sender's status plus the facts only its events carry.
#[derive(Debug, Clone)]
pub(crate) struct SenderMeta {
    /// The last status seen in an event (the handle's live status may be newer).
    pub status: SenderStatus,
    /// Name of the hub, from `Connected` / `Paired`.
    pub hub_name: Option<String>,
    /// The last non-fatal error (`SenderEvent::Error`), or a capture fallback warning.
    pub last_error: Option<String>,
    /// The hub's controls for this device's stream.
    pub hub: HubControls,
}

impl SenderMeta {
    /// A fresh snapshot for a sender that just started.
    pub(crate) fn new(status: SenderStatus, last_error: Option<String>) -> Self {
        Self {
            status,
            hub_name: None,
            last_error,
            hub: HubControls::default(),
        }
    }

    /// Folds one engine event into the snapshot.
    pub(crate) fn apply(&mut self, event: SenderEvent) {
        match event {
            SenderEvent::StateChanged(state) => self.status.state = state,
            SenderEvent::Connected { name, .. } | SenderEvent::Paired { name, .. } => {
                self.hub_name = Some(name);
            }
            SenderEvent::Status(status) => self.status = status,
            SenderEvent::Error(message) => self.last_error = Some(message),
            SenderEvent::HubControl {
                gain,
                muted,
                priority,
            } => {
                self.hub = HubControls {
                    gain,
                    muted,
                    priority,
                };
            }
        }
    }

    /// The error to show with `state`: the failure reason when failed, else the last
    /// non-fatal error.
    pub(crate) fn error_for<'a>(&'a self, state: &'a SenderState) -> Option<&'a str> {
        state_str(state).1.or(self.last_error.as_deref())
    }
}

/// The state string used by the API and the C ABI, and the failure reason, if any.
pub(crate) fn state_str(s: &SenderState) -> (&'static str, Option<&str>) {
    match s {
        SenderState::Connecting => ("connecting", None),
        SenderState::Pairing => ("pairing", None),
        SenderState::Streaming => ("streaming", None),
        SenderState::Reconnecting => ("reconnecting", None),
        SenderState::Stopped => ("stopped", None),
        SenderState::Failed(reason) => ("failed", Some(reason.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_fold_into_the_snapshot() {
        let mut meta = SenderMeta::new(SenderStatus::default(), Some("fallback".into()));
        assert_eq!(meta.hub, HubControls::default());
        meta.apply(SenderEvent::Paired {
            device_id: "id".into(),
            name: "Desk".into(),
        });
        meta.apply(SenderEvent::HubControl {
            gain: 0.5,
            muted: true,
            priority: false,
        });
        meta.apply(SenderEvent::StateChanged(SenderState::Streaming));
        assert_eq!(meta.hub_name.as_deref(), Some("Desk"));
        assert_eq!(
            meta.hub,
            HubControls {
                gain: 0.5,
                muted: true,
                priority: false
            }
        );
        assert_eq!(meta.error_for(&meta.status.state), Some("fallback"));
        let failed = SenderState::Failed("pairing required".into());
        assert_eq!(meta.error_for(&failed), Some("pairing required"));
        assert_eq!(state_str(&failed).0, "failed");
    }
}
