//! The three primitives the bridge needs: random secrets, SHA-256, and a
//! comparison that does not leak how far it got.
//!
//! `BCrypt` rather than a crate. It is already linked for `BCryptGenRandom`,
//! and a hash this small does not justify a dependency the extension would
//! have to be trusted to match.

use windows::Win32::Security::Cryptography::{
    BCRYPT_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, BCryptHash,
};

/// Hex, because everything derived from it travels as JSON and gets compared
/// as a string on both sides.
pub fn random_hex(bytes: usize) -> Option<String> {
    let mut buffer = vec![0u8; bytes];
    // SAFETY: the buffer is live and sized by the slice itself; the
    // system-preferred RNG needs no algorithm handle.
    let status = unsafe { BCryptGenRandom(None, &mut buffer, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        return None;
    }
    Some(hex(&buffer))
}

pub fn sha256_hex(input: &str) -> Option<String> {
    let mut digest = [0u8; 32];
    // SAFETY: the pseudo-handle needs no open/close, and both slices outlive
    // the call. No secret means a plain hash rather than an HMAC.
    let status =
        unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, input.as_bytes(), &mut digest) };
    if status.is_err() {
        return None;
    }
    Some(hex(&digest))
}

/// The one hash both sides compute. Every field is hex or digits, so `\0`
/// separators make the concatenation unambiguous - no two different inputs can
/// produce the same string.
///
/// `label` is what stops a proof being replayed in the other direction: the
/// client and the server hash the same secret and nonces under different names.
pub fn proof(label: &str, secret: &str, nonce_c: &str, nonce_s: &str) -> Option<String> {
    sha256_hex(&format!("bentolaunch\0{label}\0{secret}\0{nonce_c}\0{nonce_s}"))
}

/// No early return. A secret comparison that leaks its progress stops being
/// harmless as soon as something else changes.
pub fn equal(expected: &str, given: &str) -> bool {
    let (expected, given) = (expected.as_bytes(), given.as_bytes());
    if expected.len() != given.len() {
        return false;
    }
    let mut differences = 0u8;
    for (a, b) in expected.iter().zip(given) {
        differences |= a ^ b;
    }
    differences == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_digest_matches_the_reference_value() {
        // The extension computes this same hash in JavaScript. If this vector
        // ever changes, every paired browser stops being able to prove itself.
        assert_eq!(
            sha256_hex("abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex("").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_label_makes_a_proof_directional() {
        let (secret, nc, ns) = ("s", "a", "b");
        let client = proof("resume-client", secret, nc, ns).unwrap();
        let server = proof("resume-server", secret, nc, ns).unwrap();
        assert_ne!(client, server, "a proof must not be replayable back");
    }

    #[test]
    fn every_field_changes_the_proof() {
        let base = proof("l", "s", "a", "b").unwrap();
        assert_ne!(base, proof("l", "s2", "a", "b").unwrap());
        assert_ne!(base, proof("l", "s", "a2", "b").unwrap());
        assert_ne!(base, proof("l", "s", "a", "b2").unwrap());
    }

    #[test]
    fn random_values_are_the_right_size_and_not_repeated() {
        let a = random_hex(24).expect("the OS RNG must work");
        let b = random_hex(24).expect("the OS RNG must work");
        assert_eq!(a.len(), 48, "hex is two characters per byte");
        assert_ne!(a, b);
    }

    #[test]
    fn comparison_does_not_take_a_prefix() {
        assert!(equal("abcd", "abcd"));
        assert!(!equal("abcd", "ab"));
        assert!(!equal("abcd", "abcde"));
        assert!(!equal("abcd", ""));
    }
}
