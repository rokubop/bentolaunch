//! Who is allowed on flick's socket. Two callers, two gates.
//!
//! Web page: can script a WebSocket to localhost, so without a check any site
//! could enumerate your tabs. The browser stamps `Origin` on the handshake and
//! page JS cannot forge it, so an allowlist closes it.
//!
//! Local program: not a browser, so `Origin` is whatever it types. Only the
//! token stops it.
//!
//! Neither stops code already running as you. That code has better targets.

/// Logged, never sent back. A refused caller learns only that it failed.
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
    /// `None` refuses to listen. Both halves required: no token lets any local
    /// process in, no allowlist lets in any page that learns the token.
    pub fn new(allow: &[String], token: &str) -> Option<Policy> {
        if token.len() < MIN_TOKEN_LEN || allow.is_empty() {
            return None;
        }
        Some(Policy {
            allow: allow.iter().map(|o| o.trim().to_ascii_lowercase()).collect(),
            token: token.to_owned(),
        })
    }

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

const MIN_TOKEN_LEN: usize = 24;

/// No early return. A secret comparison that leaks its progress stops being
/// harmless as soon as something else changes.
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

/// OS CSPRNG, hex encoded. Not the clock or a pid: this is the only thing
/// between a local process and the tab list.
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
