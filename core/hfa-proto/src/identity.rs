//! Device identifiers.

use sha2::{Digest, Sha256};

/// Length of a fingerprint string: 16 hex digits in 4 groups plus 3 dashes.
pub const FINGERPRINT_LEN: usize = 19;

/// Fingerprint of a static public key, used as the `device_id`: the first 8 bytes of
/// SHA-256(pubkey) as lowercase hex, grouped by 4 hex digits: `ab12-cd34-ef56-7890`.
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(pubkey);
    let mut out = String::with_capacity(FINGERPRINT_LEN);
    for (i, byte) in digest.iter().take(8).enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push('-');
        }
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// `true` if `s` has the exact shape of a [`fingerprint`] (`xxxx-xxxx-xxxx-xxxx`, lowercase
/// hex). Useful to tell a device id from a device name (e.g. `hfa send --hub <name-or-id>`).
pub fn is_fingerprint(s: &str) -> bool {
    s.len() == FINGERPRINT_LEN
        && s.bytes().enumerate().all(|(i, b)| {
            if i % 5 == 4 {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn known_vector() {
        // SHA-256 of 32 zero bytes = 66687aadf862bd776c8fc18b8e9f8e20...
        assert_eq!(fingerprint(&[0; 32]), "6668-7aad-f862-bd77");
    }

    #[test]
    fn shape_and_detection() {
        assert!(is_fingerprint("ab12-cd34-ef56-7890"));
        assert!(!is_fingerprint("AB12-cd34-ef56-7890"));
        assert!(!is_fingerprint("ab12cd34ef567890"));
        assert!(!is_fingerprint("ab12-cd34-ef56-789"));
        assert!(!is_fingerprint("ab12-cd34-ef56-789g"));
        assert!(!is_fingerprint("Living room"));
    }

    proptest! {
        #[test]
        fn format_is_grouped_lowercase_hex(key: [u8; 32]) {
            let fp = fingerprint(&key);
            prop_assert!(is_fingerprint(&fp), "{}", fp);
            let expected: String = Sha256::digest(key)[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            prop_assert_eq!(fp.replace('-', ""), expected);
        }
    }
}
