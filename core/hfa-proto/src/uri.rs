//! Pairing URI shown as a QR code by the hub.
//!
//! Format: `hfa://pair?v=0&h=<host>&p=<port>&id=<b64url pubkey>&t=<token>&n=<urlencoded name>`
//! (`id` is the hub's 32-byte static public key, base64url without padding).

use std::fmt;
use std::str::FromStr;

use crate::ProtoError;

/// URI scheme.
pub const URI_SCHEME: &str = "hfa";

/// Everything a sender needs to reach and pair with a hub.
///
/// `Debug` redacts the token. `Display` (the URI itself) necessarily contains the token, so
/// never log `to_string()`.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingUri {
    /// Hub host (IP address or host name).
    pub host: String,
    /// Hub TCP control port.
    pub port: u16,
    /// Hub static public key (pinned on first connection).
    pub hub_id: [u8; 32],
    /// One-time pairing token (used as the SPAKE2 password with `PairMethod::Token`).
    pub token: String,
    /// Hub display name.
    pub name: String,
}

impl fmt::Debug for PairingUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingUri")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("hub_id", &self.hub_id)
            .field("token", &"<redacted>")
            .field("name", &self.name)
            .finish()
    }
}

impl fmt::Display for PairingUri {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("feat/proto")
    }
}

impl FromStr for PairingUri {
    type Err = ProtoError;

    fn from_str(_s: &str) -> Result<Self, Self::Err> {
        todo!("feat/proto")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_token() {
        let uri = PairingUri {
            host: "10.0.0.2".into(),
            port: 47810,
            hub_id: [1; 32],
            token: "SECRET-token-value".into(),
            name: "Desk".into(),
        };
        let text = format!("{uri:?}");
        assert!(!text.contains("SECRET"), "{text}");
        assert!(
            text.contains("<redacted>") && text.contains("Desk"),
            "{text}"
        );
    }
}
