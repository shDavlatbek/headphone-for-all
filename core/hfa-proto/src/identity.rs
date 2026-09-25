//! Device identifiers.

/// Fingerprint of a static public key, used as the `device_id`: the first 8 bytes of
/// SHA-256(pubkey) as lowercase hex, grouped by 4 hex digits: `ab12-cd34-ef56-7890`.
pub fn fingerprint(_pubkey: &[u8; 32]) -> String {
    todo!("feat/proto")
}
