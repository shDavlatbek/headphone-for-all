//! Pairing URI shown as a QR code by the hub.
//!
//! Format: `hfa://pair?v=0&h=<host>&p=<port>&id=<b64url pubkey>&t=<token>&n=<urlencoded name>`
//! (`id` is the hub's 32-byte static public key, base64url without padding).
//!
//! Rules (wire contract):
//! - Values are percent-encoded (UTF-8); everything except ASCII alphanumerics and
//!   `- . _ ~ :` is escaped. `+` is a literal plus, not a space.
//! - An IPv6 host is written in brackets (`h=%5Bfe80::1%5D`); [`PairingUri::host`] holds it
//!   without brackets. Zone ids (`%eth0`) are not supported.
//! - Parsing is strict: scheme `hfa` and host `pair` (both case-insensitive), no fragment,
//!   every parameter exactly once, `v=0`, `p` in 1..=65535 (decimal digits only), `id`
//!   decoding to exactly 32 bytes, `t` non-empty base64url (at most [`MAX_TOKEN_LEN`] chars),
//!   `h` a bracketed IPv6 address or a host name / IPv4 address made of ASCII letters, digits,
//!   `.`, `-`, `_` (at most 253 chars), `n` at most [`MAX_NAME_LEN`] bytes without control
//!   characters, well-formed percent escapes, at most [`MAX_URI_LEN`] bytes overall.
//!   Unknown parameters are ignored (forward compatibility). Surrounding whitespace (e.g. a
//!   trailing newline from a QR scanner) is trimmed.

use std::fmt;
use std::net::Ipv6Addr;
use std::str::FromStr;

use base64::Engine as _;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::ProtoError;

/// URI scheme.
pub const URI_SCHEME: &str = "hfa";
/// The fixed "host" part after `hfa://`.
pub const URI_PATH: &str = "pair";
/// Longest accepted URI in bytes.
pub const MAX_URI_LEN: usize = 2048;
/// Longest accepted hub name in bytes (UTF-8).
pub const MAX_NAME_LEN: usize = 256;
/// Longest accepted token in characters.
pub const MAX_TOKEN_LEN: usize = 128;
/// Longest accepted host name.
const MAX_HOST_LEN: usize = 253;

/// Characters escaped in parameter values (all but unreserved characters and `:`).
const VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b':');

fn invalid(msg: impl Into<String>) -> ProtoError {
    ProtoError::InvalidUri(msg.into())
}

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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bare = self
            .host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&self.host);
        let host = if bare.parse::<Ipv6Addr>().is_ok() {
            format!("[{bare}]")
        } else {
            self.host.clone()
        };
        write!(
            f,
            "{URI_SCHEME}://{URI_PATH}?v={}&h={}&p={}&id={}&t={}&n={}",
            crate::PROTOCOL_VERSION,
            utf8_percent_encode(&host, VALUE),
            self.port,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.hub_id),
            utf8_percent_encode(&self.token, VALUE),
            utf8_percent_encode(&self.name, VALUE),
        )
    }
}

impl FromStr for PairingUri {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() > MAX_URI_LEN {
            return Err(invalid(format!("longer than {MAX_URI_LEN} bytes")));
        }
        let (scheme, rest) = s
            .split_once("://")
            .ok_or_else(|| invalid("missing scheme"))?;
        if !scheme.eq_ignore_ascii_case(URI_SCHEME) {
            return Err(invalid("scheme is not hfa"));
        }
        let (path, query) = rest
            .split_once('?')
            .ok_or_else(|| invalid("missing query"))?;
        if !path.eq_ignore_ascii_case(URI_PATH) {
            return Err(invalid("expected hfa://pair?..."));
        }
        if query.contains('#') {
            return Err(invalid("fragments are not allowed"));
        }

        let mut params = Params::default();
        for pair in query.split('&') {
            let (key, raw) = pair
                .split_once('=')
                .ok_or_else(|| invalid("parameter without '='"))?;
            let slot = match key {
                "v" => &mut params.v,
                "h" => &mut params.h,
                "p" => &mut params.p,
                "id" => &mut params.id,
                "t" => &mut params.t,
                "n" => &mut params.n,
                _ => continue,
            };
            if slot.is_some() {
                return Err(invalid(format!("duplicate parameter '{key}'")));
            }
            *slot = Some(decode_value(raw)?);
        }

        let version = params.v.ok_or_else(|| invalid("missing v"))?;
        if version != crate::PROTOCOL_VERSION.to_string() {
            return Err(invalid("unsupported version"));
        }
        Ok(PairingUri {
            host: parse_host(&params.h.ok_or_else(|| invalid("missing h"))?)?,
            port: parse_port(&params.p.ok_or_else(|| invalid("missing p"))?)?,
            hub_id: parse_id(&params.id.ok_or_else(|| invalid("missing id"))?)?,
            token: parse_token(params.t.ok_or_else(|| invalid("missing t"))?)?,
            name: parse_name(params.n.ok_or_else(|| invalid("missing n"))?)?,
        })
    }
}

/// Decoded query parameters (each at most once).
#[derive(Default)]
struct Params {
    v: Option<String>,
    h: Option<String>,
    p: Option<String>,
    id: Option<String>,
    t: Option<String>,
    n: Option<String>,
}

/// Checks the percent escapes and decodes a value as UTF-8.
fn decode_value(raw: &str) -> Result<String, ProtoError> {
    let bytes = raw.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'%'
            && !(bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return Err(invalid("malformed percent escape"));
        }
    }
    percent_decode_str(raw)
        .decode_utf8()
        .map(|v| v.into_owned())
        .map_err(|_| invalid("value is not UTF-8"))
}

fn parse_host(h: &str) -> Result<String, ProtoError> {
    if let Some(inner) = h.strip_prefix('[') {
        let addr = inner
            .strip_suffix(']')
            .ok_or_else(|| invalid("unterminated IPv6 bracket"))?;
        return addr
            .parse::<Ipv6Addr>()
            .map(|_| addr.to_string())
            .map_err(|_| invalid("bad IPv6 address"));
    }
    let valid = !h.is_empty()
        && h.len() <= MAX_HOST_LEN
        && h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if valid {
        Ok(h.to_string())
    } else {
        Err(invalid("bad host (IPv6 addresses need brackets)"))
    }
}

fn parse_port(p: &str) -> Result<u16, ProtoError> {
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("bad port"));
    }
    match p.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(invalid("port out of range")),
    }
}

fn parse_id(id: &str) -> Result<[u8; 32], ProtoError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .map_err(|_| invalid("id is not unpadded base64url"))?;
    bytes
        .try_into()
        .map_err(|_| invalid("id must decode to 32 bytes"))
}

fn parse_token(t: String) -> Result<String, ProtoError> {
    let valid = !t.is_empty()
        && t.len() <= MAX_TOKEN_LEN
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(t)
    } else {
        Err(invalid("bad token"))
    }
}

fn parse_name(n: String) -> Result<String, ProtoError> {
    if n.len() > MAX_NAME_LEN {
        return Err(invalid(format!("name longer than {MAX_NAME_LEN} bytes")));
    }
    if n.chars().any(char::is_control) {
        return Err(invalid("name contains control characters"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> PairingUri {
        PairingUri {
            host: "192.168.1.20".into(),
            port: 47810,
            hub_id: [0xFB; 32],
            token: "AbCdEfGhIjKlMnOpQrSt-_".into(),
            name: "Living room".into(),
        }
    }

    fn with(key: &str, value: &str) -> String {
        let uri = sample().to_string();
        let mut parts: Vec<String> = uri
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .map(str::to_string)
            .collect();
        for p in &mut parts {
            if p.split_once('=').unwrap().0 == key {
                *p = format!("{key}={value}");
            }
        }
        format!("hfa://pair?{}", parts.join("&"))
    }

    #[test]
    fn exact_format() {
        let text = sample().to_string();
        assert_eq!(
            text,
            "hfa://pair?v=0&h=192.168.1.20&p=47810\
             &id=-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s\
             &t=AbCdEfGhIjKlMnOpQrSt-_&n=Living%20room"
        );
        assert_eq!(text.parse::<PairingUri>().unwrap(), sample());
    }

    #[test]
    fn unicode_name_roundtrips() {
        let uri = PairingUri {
            name: "Küche & Bad = 🎧 ü+ä/?#%".into(),
            ..sample()
        };
        let text = uri.to_string();
        assert!(
            text.contains("n=K%C3%BCche%20%26%20Bad%20%3D%20%F0%9F%8E%A7"),
            "{text}"
        );
        assert!(!text.contains('#') && !text.contains(' '), "{text}");
        assert_eq!(text.parse::<PairingUri>().unwrap(), uri);
    }

    #[test]
    fn ipv6_host_uses_brackets() {
        let uri = PairingUri {
            host: "fe80::1ff:fe23:4567:890a".into(),
            ..sample()
        };
        let text = uri.to_string();
        assert!(text.contains("h=%5Bfe80::1ff:fe23:4567:890a%5D&"), "{text}");
        assert_eq!(text.parse::<PairingUri>().unwrap(), uri);
        // Already-bracketed input is not double-bracketed; literal brackets also parse.
        let bracketed = PairingUri {
            host: "[::1]".into(),
            ..sample()
        };
        assert!(bracketed.to_string().contains("h=%5B::1%5D&"));
        assert_eq!(
            with("h", "[::1]").parse::<PairingUri>().unwrap().host,
            "::1"
        );
        assert!(with("h", "[::1").parse::<PairingUri>().is_err());
        assert!(with("h", "[nope]").parse::<PairingUri>().is_err());
        assert!(
            with("h", "::1").parse::<PairingUri>().is_err(),
            "unbracketed IPv6"
        );
        assert!(
            with("h", "%5Bfe80::1%25eth0%5D")
                .parse::<PairingUri>()
                .is_err(),
            "zone id"
        );
    }

    #[test]
    fn hostnames_are_accepted() {
        let uri = PairingUri {
            host: "My-PC_2.local".into(),
            ..sample()
        };
        assert_eq!(uri.to_string().parse::<PairingUri>().unwrap(), uri);
        for bad in ["", "a%20b", "a/b", "a@b", &"a".repeat(254)] {
            assert!(with("h", bad).parse::<PairingUri>().is_err(), "{bad}");
        }
    }

    #[test]
    fn scheme_path_and_whitespace() {
        let text = sample().to_string();
        assert_eq!(
            text.replacen("hfa://pair", "HFA://PAIR", 1)
                .parse::<PairingUri>()
                .unwrap(),
            sample()
        );
        assert_eq!(
            format!("  {text}\n").parse::<PairingUri>().unwrap(),
            sample()
        );
        for bad in [
            text.replacen("hfa://", "http://", 1),
            text.replacen("hfa://", "hfa:", 1),
            text.replacen("pair?", "pairing?", 1),
            text.replacen("pair?", "pair/?", 1),
            text.replacen('?', "", 1),
            format!("{text}#frag"),
            String::new(),
        ] {
            assert!(bad.parse::<PairingUri>().is_err(), "{bad}");
        }
    }

    #[test]
    fn strict_parameter_validation() {
        let bad = [
            with("v", "1"),
            with("v", ""),
            with("p", "0"),
            with("p", "65536"),
            with("p", "+80"),
            with("p", "-1"),
            with("p", "80a"),
            with("p", ""),
            with("id", "AAAA"),
            with("id", &"A".repeat(44)),
            with("id", &format!("{}=", "A".repeat(43))),
            with("id", &"*".repeat(43)),
            with("t", ""),
            with("t", "has%20space"),
            with("t", &"a".repeat(MAX_TOKEN_LEN + 1)),
            with("n", "%E2%82"),
            with("n", "bad%zzescape"),
            with("n", "trailing%4"),
            with("n", "new%0Aline"),
            with("n", &"a".repeat(MAX_NAME_LEN + 1)),
            format!("{}&p=1", sample()),
            format!("{}&", sample()),
            format!("{}&novalue", sample()),
            sample()
                .to_string()
                .replace("&t=AbCdEfGhIjKlMnOpQrSt-_", ""),
            sample().to_string().replace("v=0&", ""),
            format!("{}&x={}", sample(), "a".repeat(MAX_URI_LEN)),
        ];
        for uri in bad {
            assert!(
                matches!(uri.parse::<PairingUri>(), Err(ProtoError::InvalidUri(_))),
                "accepted: {uri}"
            );
        }
        // Boundary values that must pass.
        assert_eq!(with("p", "1").parse::<PairingUri>().unwrap().port, 1);
        assert_eq!(
            with("p", "65535").parse::<PairingUri>().unwrap().port,
            65535
        );
        assert_eq!(with("n", "").parse::<PairingUri>().unwrap().name, "");
        // Unknown parameters are ignored, parameter order does not matter.
        let reordered = format!(
            "hfa://pair?n=Living%20room&future=1&t=AbCdEfGhIjKlMnOpQrSt-_&id={}&p=47810&h=192.168.1.20&v=0",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0xFB; 32])
        );
        assert_eq!(reordered.parse::<PairingUri>().unwrap(), sample());
    }

    fn host_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<[u8; 4]>().prop_map(|o| std::net::Ipv4Addr::from(o).to_string()),
            any::<[u16; 8]>().prop_map(|s| Ipv6Addr::from(s).to_string()),
            "[a-zA-Z0-9][a-zA-Z0-9._-]{0,40}",
        ]
    }

    proptest! {
        #[test]
        fn roundtrip(host in host_strategy(), port in 1u16.., hub_id: [u8; 32],
                     token in "[A-Za-z0-9_-]{1,64}",
                     name in "[^\\p{Cc}]{0,40}") {
            let uri = PairingUri { host, port, hub_id, token, name };
            let text = uri.to_string();
            prop_assert!(text.is_ascii());
            prop_assert_eq!(text.parse::<PairingUri>().unwrap(), uri);
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,200}") {
            let _ = s.parse::<PairingUri>();
            let _ = format!("hfa://pair?{s}").parse::<PairingUri>();
        }
    }

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
