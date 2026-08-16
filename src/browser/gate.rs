//! Who is allowed to talk to flick's socket.
//!
//! A loopback WebSocket is not a private channel. Two different callers can
//! reach it, and they need two different answers:
//!
//! - **A web page.** Any site you have open can script
//!   `new WebSocket("ws://127.0.0.1:8777/")`. Browsers attach an `Origin`
//!   header to that handshake and page JavaScript cannot forge or suppress it,
//!   so an origin allowlist stops this completely. This is the threat that
//!   matters: without the check, a random tab could enumerate every other tab
//!   you have open, titles and URLs both.
//!
//! - **Another program on this machine.** Not a browser, so `Origin` is
//!   whatever it feels like typing. Only a shared secret stops that, hence the
//!   token in the request path.
//!
//! Neither gate defends against code already running as you with your files in
//! reach — such code has better targets than this socket. They defend against
//! the drive-by cases, which are the realistic ones.

/// Why a connection was refused. Logged, never sent back: a caller that failed
/// the gate learns only that it failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotLoopback,
    MissingOrigin,
    UnknownOrigin(String),
    BadToken,
}

impl Refusal {
    pub fn reason(&self) -> String {
        match self {
            Refusal::NotLoopback => "not a loopback address".into(),
            Refusal::MissingOrigin => "no Origin header".into(),
            Refusal::UnknownOrigin(origin) => {
                format!("origin {origin} is not in browser.allow")
            }
            Refusal::BadToken => "wrong token".into(),
        }
    }
}

/// What the gate checks against. Built once from config.
#[derive(Clone)]
pub struct Policy {
    allow: Vec<String>,
    token: String,
}

impl Policy {
    /// `None` when the bridge is not configured to a point where it would be
    /// safe to listen. Both halves are required: an allowlist with no token
    /// lets any local process in, and a token with no allowlist lets any web
    /// page that learns the token in.
    pub fn new(allow: &[String], token: &str) -> Option<Policy> {
        if token.len() < MIN_TOKEN_LEN || allow.is_empty() {
            return None;
        }
        Some(Policy {
            allow: allow.iter().map(|o| o.trim().to_ascii_lowercase()).collect(),
            token: token.to_owned(),
        })
    }

    /// Every gate, in order. `Ok` means the handshake may proceed.
    pub fn admit(&self, loopback: bool, origin: Option<&str>, path: &str) -> Result<(), Refusal> {
        if !loopback {
            return Err(Refusal::NotLoopback);
        }
        let origin = origin.ok_or(Refusal::MissingOrigin)?;
        let normalized = origin.trim().to_ascii_lowercase();
        if !self.allow.contains(&normalized) {
            return Err(Refusal::UnknownOrigin(origin.to_owned()));
        }
        if !token_matches(&self.token, path.strip_prefix('/').unwrap_or(path)) {
            return Err(Refusal::BadToken);
        }
        Ok(())
    }
}

/// Short tokens are refused outright rather than accepted weakly.
const MIN_TOKEN_LEN: usize = 24;

/// Compared without an early return. Over loopback a timing attack is already
/// impractical, but a secret comparison that leaks its progress is the kind of
/// thing that stops being harmless once something else changes.
fn token_matches(expected: &str, given: &str) -> bool {
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

/// A fresh token from the OS CSPRNG, hex encoded.
///
/// `BCryptGenRandom` rather than anything derived from the clock or a pid: this
/// is the only thing standing between a local process and the tab list.
pub fn generate_token() -> Option<String> {
    use windows::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };

    let mut bytes = [0u8; 24];
    // SAFETY: the buffer is a live stack local sized by the slice itself, and
    // the system-preferred RNG needs no algorithm handle.
    let status = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn policy() -> Policy {
        Policy::new(&[ORIGIN.to_string()], TOKEN).expect("fixture must be admissible")
    }

    #[test]
    fn a_paired_extension_gets_in() {
        assert_eq!(policy().admit(true, Some(ORIGIN), &format!("/{TOKEN}")), Ok(()));
    }

    #[test]
    fn a_web_page_is_refused_however_right_its_token_is() {
        // The whole point: a page that somehow learned the token still fails,
        // because a browser stamps its own origin on the handshake.
        let path = format!("/{TOKEN}");
        for origin in ["https://evil.example", "http://localhost:3000", "null"] {
            assert_eq!(
                policy().admit(true, Some(origin), &path),
                Err(Refusal::UnknownOrigin(origin.into()))
            );
        }
    }

    #[test]
    fn a_local_program_is_refused_however_right_its_origin_is() {
        // Not a browser, so it can claim any origin it likes. The token is what
        // it cannot guess.
        assert_eq!(
            policy().admit(true, Some(ORIGIN), "/not-the-token"),
            Err(Refusal::BadToken)
        );
        assert_eq!(policy().admit(true, Some(ORIGIN), "/"), Err(Refusal::BadToken));
    }

    #[test]
    fn a_handshake_with_no_origin_at_all_is_refused() {
        assert_eq!(
            policy().admit(true, None, &format!("/{TOKEN}")),
            Err(Refusal::MissingOrigin)
        );
    }

    #[test]
    fn nothing_off_the_loopback_is_even_considered() {
        assert_eq!(
            policy().admit(false, Some(ORIGIN), &format!("/{TOKEN}")),
            Err(Refusal::NotLoopback)
        );
    }

    #[test]
    fn half_a_configuration_refuses_to_listen() {
        assert!(Policy::new(&[], TOKEN).is_none(), "no allowlist must not listen");
        assert!(Policy::new(&[ORIGIN.into()], "").is_none(), "no token must not listen");
        assert!(Policy::new(&[ORIGIN.into()], "short").is_none(), "weak token must not listen");
    }

    #[test]
    fn origins_match_regardless_of_case_or_stray_space() {
        let policy = Policy::new(&[format!("  {} ", ORIGIN.to_uppercase())], TOKEN).unwrap();
        assert_eq!(policy.admit(true, Some(ORIGIN), &format!("/{TOKEN}")), Ok(()));
    }

    #[test]
    fn the_token_comparison_does_not_take_a_prefix() {
        assert!(token_matches(TOKEN, TOKEN));
        assert!(!token_matches(TOKEN, &TOKEN[..8]));
        assert!(!token_matches(TOKEN, &format!("{TOKEN}extra")));
        assert!(!token_matches(TOKEN, ""));
    }

    #[test]
    fn generated_tokens_are_long_and_not_repeated() {
        let a = generate_token().expect("the OS RNG must work");
        let b = generate_token().expect("the OS RNG must work");
        assert!(a.len() >= MIN_TOKEN_LEN);
        assert_ne!(a, b);
        assert!(Policy::new(&["x".into()], &a).is_some());
    }
}
