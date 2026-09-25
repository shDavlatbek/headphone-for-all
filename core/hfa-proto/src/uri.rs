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
#[derive(Debug, Clone, PartialEq, Eq)]
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
