//! Turns the hub fields of `SenderStartDto` / the extension's JSON config into a
//! [`HubAddress`] and the expected hub key (shared by the Flutter API and the C ABI).
//!
//! Rules (see docs/CONTRACTS.md §8.1 and the feat/ffi refinements):
//! - `hub_host` non-empty → [`HubAddress::Direct`] (`hub_port` 0 → the settings port, or
//!   [`hfa_proto::DEFAULT_PORT`] when that is 0 too);
//!   empty host with a `hub_device_id` → [`HubAddress::Discover`] by that id (mDNS; the
//!   engine checks the fingerprint).
//! - `hub_key` (base64url static key from a pairing URI, padded or not) becomes
//!   `expected_hub_key`; if `hub_device_id` is also given it must be that key's fingerprint.
//! - Without `hub_key`, the key of the trusted peer `hub_device_id` is used (if trusted);
//!   otherwise pairing must establish trust.
//! - A sender never targets its own device (loop protection).

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use hfa_core::HubAddress;

use crate::error::{FfiError, Result};

/// The hub as the caller described it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HubRequest {
    /// Host name or IP address; empty to discover by device id.
    pub host: String,
    /// TCP control port; 0 = default.
    pub port: u16,
    /// Hub device id (fingerprint), from discovery or a trusted peer.
    pub device_id: Option<String>,
    /// base64url static key, from a pairing URI.
    pub key: Option<String>,
}

/// Where and whom to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HubTarget {
    /// Address for `SenderConfig::hub`.
    pub address: HubAddress,
    /// Key for `SenderConfig::expected_hub_key`.
    pub expected_key: Option<[u8; 32]>,
}

/// Decodes a base64url (padded or unpadded) 32-byte static key.
pub(crate) fn decode_key(text: &str) -> Result<[u8; 32]> {
    let text = text.trim();
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| URL_SAFE.decode(text))
        .map_err(|e| FfiError::InvalidArgument(format!("hub key is not base64url: {e}")))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        FfiError::InvalidArgument(format!("hub key must be 32 bytes, got {}", bytes.len()))
    })
}

/// Encodes a static key as unpadded base64url (the pairing-URI form).
#[cfg(any(feature = "flutter", test))]
pub(crate) fn encode_key(key: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

/// The port to dial: `requested`, else `default_port` (the settings port), else — when the
/// settings port is 0 too ("hub binds any free port") — [`hfa_proto::DEFAULT_PORT`]. Port 0
/// can never be dialled.
fn effective_port(requested: u16, default_port: u16) -> u16 {
    match (requested, default_port) {
        (0, 0) => hfa_proto::DEFAULT_PORT,
        (0, p) | (p, _) => p,
    }
}

/// Resolves `req`.
///
/// - `default_port`: used when `req.port == 0` (itself replaced by
///   [`hfa_proto::DEFAULT_PORT`] when 0).
/// - `trusted_key`: looks up the pinned key of a trusted peer by device id.
/// - `own_key`: this device's static key (loop protection).
///
/// # Errors
/// [`FfiError::InvalidArgument`] for a missing address, a malformed key, a key that does
/// not match `device_id`, or a target that is this device.
pub(crate) fn resolve(
    req: &HubRequest,
    default_port: u16,
    trusted_key: impl Fn(&str) -> Option<[u8; 32]>,
    own_key: &[u8; 32],
) -> Result<HubTarget> {
    let host = req.host.trim();
    let device_id = req
        .device_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let key = req
        .key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(decode_key)
        .transpose()?;
    if let (Some(key), Some(id)) = (key, device_id) {
        if hfa_proto::fingerprint(&key) != id {
            return Err(FfiError::InvalidArgument(format!(
                "hub key does not belong to device {id}"
            )));
        }
    }
    let expected_key = key.or_else(|| device_id.and_then(&trusted_key));
    let own_id = hfa_proto::fingerprint(own_key);
    if expected_key.as_ref() == Some(own_key) || device_id == Some(own_id.as_str()) {
        return Err(FfiError::InvalidArgument(
            "a device cannot stream to its own hub".to_owned(),
        ));
    }
    let address = if !host.is_empty() {
        HubAddress::Direct {
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
            port: effective_port(req.port, default_port),
        }
    } else if let Some(id) = device_id {
        HubAddress::Discover {
            name_or_id: id.to_owned(),
        }
    } else {
        return Err(FfiError::InvalidArgument(
            "hub_host or hub_device_id is required".to_owned(),
        ));
    };
    Ok(HubTarget {
        address,
        expected_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: [u8; 32] = [1; 32];
    const HUB: [u8; 32] = [2; 32];

    fn none(_: &str) -> Option<[u8; 32]> {
        None
    }

    #[test]
    fn keys_round_trip_padded_or_not() {
        let text = encode_key(&HUB);
        assert_eq!(text.len(), 43);
        assert_eq!(decode_key(&text).expect("unpadded"), HUB);
        assert_eq!(decode_key(&format!(" {text}= ")).expect("padded"), HUB);
        assert!(decode_key("not base64!").is_err());
        assert!(decode_key(&URL_SAFE_NO_PAD.encode([0u8; 16])).is_err());
    }

    #[test]
    fn direct_address_with_key() {
        let req = HubRequest {
            host: " 192.168.1.20 ".into(),
            port: 0,
            device_id: None,
            key: Some(encode_key(&HUB)),
        };
        let t = resolve(&req, 47810, none, &OWN).expect("resolve");
        assert_eq!(
            t.address,
            HubAddress::Direct {
                host: "192.168.1.20".into(),
                port: 47810
            }
        );
        assert_eq!(t.expected_key, Some(HUB));
        let req = HubRequest {
            host: "[fe80::1]".into(),
            port: 5000,
            ..req
        };
        let t = resolve(&req, 47810, none, &OWN).expect("resolve");
        assert_eq!(
            t.address,
            HubAddress::Direct {
                host: "fe80::1".into(),
                port: 5000
            }
        );
    }

    #[test]
    fn port_zero_never_reaches_the_dialer() {
        let req = HubRequest {
            host: "hub.local".into(),
            port: 0,
            device_id: None,
            key: Some(encode_key(&HUB)),
        };
        let port_of = |t: HubTarget| match t.address {
            HubAddress::Direct { port, .. } => port,
            other => panic!("unexpected {other:?}"),
        };
        // Settings port 0 ("bind any free port") is not a dialable fallback.
        assert_eq!(
            port_of(resolve(&req, 0, none, &OWN).expect("resolve")),
            hfa_proto::DEFAULT_PORT
        );
        assert_eq!(
            port_of(resolve(&req, 5001, none, &OWN).expect("resolve")),
            5001
        );
        let explicit = HubRequest { port: 6000, ..req };
        assert_eq!(
            port_of(resolve(&explicit, 0, none, &OWN).expect("resolve")),
            6000
        );
    }

    #[test]
    fn trusted_key_is_looked_up_by_device_id() {
        let id = hfa_proto::fingerprint(&HUB);
        let req = HubRequest {
            host: String::new(),
            port: 0,
            device_id: Some(id.clone()),
            key: None,
        };
        let lookup = |d: &str| (d == id).then_some(HUB);
        let t = resolve(&req, 47810, lookup, &OWN).expect("resolve");
        assert_eq!(
            t.address,
            HubAddress::Discover {
                name_or_id: id.clone()
            }
        );
        assert_eq!(t.expected_key, Some(HUB));
        // Not trusted: no key, pairing will have to establish trust.
        let t = resolve(&req, 47810, none, &OWN).expect("resolve");
        assert_eq!(t.expected_key, None);
    }

    #[test]
    fn key_must_match_device_id() {
        let req = HubRequest {
            host: "hub.local".into(),
            port: 1,
            device_id: Some(hfa_proto::fingerprint(&[3; 32])),
            key: Some(encode_key(&HUB)),
        };
        assert!(matches!(
            resolve(&req, 47810, none, &OWN),
            Err(FfiError::InvalidArgument(_))
        ));
    }

    #[test]
    fn refuses_self_and_missing_address() {
        let by_key = HubRequest {
            host: "127.0.0.1".into(),
            key: Some(encode_key(&OWN)),
            ..HubRequest::default()
        };
        assert!(resolve(&by_key, 47810, none, &OWN).is_err());
        let by_id = HubRequest {
            device_id: Some(hfa_proto::fingerprint(&OWN)),
            ..HubRequest::default()
        };
        assert!(resolve(&by_id, 47810, none, &OWN).is_err());
        assert!(resolve(&HubRequest::default(), 47810, none, &OWN).is_err());
        let bad_key = HubRequest {
            host: "h".into(),
            key: Some("%%".into()),
            ..HubRequest::default()
        };
        assert!(resolve(&bad_key, 47810, none, &OWN).is_err());
    }
}
