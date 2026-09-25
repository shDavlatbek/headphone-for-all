//! Hub-side pairing windows.
//!
//! [`PairingManager::start`] opens a window with a fresh 6-digit PIN and one-time token. While
//! it is open, an unknown sender may pair using either secret (SPAKE2, see
//! [`hfa_proto::pairing`]). After [`MAX_FAILED_ATTEMPTS`] failures the window is invalidated.

use std::time::Duration;

use hfa_proto::control::PairMethod;
use serde::{Deserialize, Serialize};

/// Failed attempts after which the current pairing window is invalidated.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;
/// Default lifetime of a pairing window.
pub const DEFAULT_PAIRING_TTL: Duration = Duration::from_secs(300);

/// What the hub shows the user while pairing is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInfo {
    /// 6-digit PIN.
    pub pin: String,
    /// One-time token (also inside `uri`).
    pub token: String,
    /// `hfa://pair?...` URI for the QR code.
    pub uri: String,
    /// Unix time (seconds) when the window closes.
    pub expires_at_unix: u64,
}

/// Hub-side pairing state. `Send + Sync`; share it with `Arc`.
#[derive(Debug)]
pub struct PairingManager {
    hub_id: [u8; 32],
    name: String,
    port: u16,
    /// The open window and its failed-attempt counter.
    window: parking_lot::Mutex<Option<(PairingInfo, u32)>>,
}

impl PairingManager {
    /// Creates a manager for the hub with static public key `hub_id`, display `name`, listening
    /// on `port` (used to build the URI; the host is the hub's primary LAN address).
    pub fn new(hub_id: [u8; 32], name: String, port: u16) -> Self {
        Self {
            hub_id,
            name,
            port,
            window: parking_lot::Mutex::new(None),
        }
    }

    /// Opens (or replaces) a pairing window valid for `ttl`.
    pub fn start(&self, _ttl: Duration) -> PairingInfo {
        let _ = (&self.hub_id, &self.name, self.port, &self.window);
        todo!("feat/core-engine")
    }

    /// Closes the current window.
    pub fn cancel(&self) {
        todo!("feat/core-engine")
    }

    /// The open window, if any and not expired.
    pub fn current(&self) -> Option<PairingInfo> {
        todo!("feat/core-engine")
    }

    /// The secret for `method` (PIN or token) of the open window, if any. The hub runs SPAKE2
    /// with it.
    pub fn secret_for(&self, _method: PairMethod) -> Option<String> {
        todo!("feat/core-engine")
    }

    /// `true` if `password` equals the PIN or token of the open, unexpired window
    /// (constant-time comparison). Does not count failures.
    pub fn verify_password(&self, _password: &str) -> bool {
        todo!("feat/core-engine")
    }

    /// Records a failed pairing attempt; invalidates the window after
    /// [`MAX_FAILED_ATTEMPTS`].
    pub fn record_failure(&self) {
        todo!("feat/core-engine")
    }

    /// Records a successful pairing (closes the window: tokens and PINs are one-time).
    pub fn record_success(&self) {
        todo!("feat/core-engine")
    }
}

/// Which [`PairMethod`] a sender uses for a user-provided secret: exactly 6 ASCII digits is a
/// PIN, anything else is a token.
pub fn method_for_secret(secret: &str) -> PairMethod {
    if secret.len() == 6 && secret.bytes().all(|b| b.is_ascii_digit()) {
        PairMethod::Pin
    } else {
        PairMethod::Token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_kind_detection() {
        assert_eq!(method_for_secret("012345"), PairMethod::Pin);
        assert_eq!(method_for_secret("12345"), PairMethod::Token);
        assert_eq!(method_for_secret("1234567"), PairMethod::Token);
        assert_eq!(method_for_secret("12a456"), PairMethod::Token);
        assert_eq!(
            method_for_secret("AAECAwQFBgcICQoLDA0ODw"),
            PairMethod::Token
        );
    }
}
