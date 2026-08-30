//! Who is allowed on bentolaunch's socket, and how each side proves it.
//!
//! Web page: can script a WebSocket to localhost, so without a check any site
//! could enumerate your tabs. The browser stamps `Origin` on the handshake and
//! page JS cannot forge it, so a paired-peers list closes it.
//!
//! Local program: not a browser, so `Origin` is whatever it types. Only the
//! per-peer token stops it, and the token never travels - both sides prove they
//! know it against fresh nonces.
//!
//! That proof runs in both directions, which is the part that is not about
//! bentolaunch's safety at all. Whoever holds the port is what the extension
//! believes bentolaunch to be; a server that cannot prove itself gets no tabs.
//!
//! Neither gate stops code already running as you. That code has better targets.

use std::sync::{Mutex, OnceLock};

use crate::browser::crypto;
use crate::browser::peers::{self, Peer};
use crate::{log_info, log_warn};

/// Logged, never sent back. A refused caller learns only that it failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotLoopback,
    MissingOrigin,
    UnknownOrigin(String),
}

impl Refusal {
    pub fn reason(&self) -> String {
        match self {
            Refusal::NotLoopback => "not a loopback address".into(),
            Refusal::MissingOrigin => "no Origin header".into(),
            Refusal::UnknownOrigin(origin) => format!("origin {origin} is not paired"),
        }
    }
}

/// What a handshake earned. Neither one is trusted yet - both still have to
/// prove themselves over the socket before anything is registered.
pub enum Admission {
    /// A browser bentolaunch has paired with before.
    Known(Box<Peer>),
    /// Unknown, but a pairing window is open, so it gets to try the code.
    Pairing,
}

/// The handshake gate. Deliberately cheap: it can only check what a header
/// carries, because the tungstenite callback has no round trip available.
pub fn admit(loopback: bool, origin: Option<&str>) -> Result<Admission, Refusal> {
    if !loopback {
        return Err(Refusal::NotLoopback);
    }
    let origin = origin.map(str::trim).filter(|o| !o.is_empty()).ok_or(Refusal::MissingOrigin)?;

    if let Some(peer) = peers::find(origin) {
        return Ok(Admission::Known(Box::new(peer)));
    }
    if pairing_open() {
        return Ok(Admission::Pairing);
    }
    Err(Refusal::UnknownOrigin(origin.to_owned()))
}

// --- Proofs -----------------------------------------------------------------
//
// Two exchanges, and which side proves first differs between them because the
// secrets differ.
//
// Resuming, the secret is a 192-bit token, so the server proves first: the
// extension can then hang up before sending a single tab title to something
// that turned out not to be bentolaunch. Proving first costs nothing when the
// secret is too large to guess from the proof.
//
// Pairing, the secret is six digits a human retyped, which *is* guessable from
// a proof. So the client proves first and one wrong answer closes the window -
// a guess is worth one in a million, and there is no oracle to grind against.
//
// The reason that ordering is safe: pairing is only offered while bentolaunch
// itself holds the port. Something squatting the port means bentolaunch never
// bound, which means the tray refuses to open a pairing window at all, so
// there is no window in which a fake server can be handed a code proof.

pub fn server_resume_proof(token: &str, nonce_c: &str, nonce_s: &str) -> Option<String> {
    crypto::proof("resume-server", token, nonce_c, nonce_s)
}

pub fn client_resume_proof(token: &str, nonce_c: &str, nonce_s: &str) -> Option<String> {
    crypto::proof("resume-client", token, nonce_c, nonce_s)
}

pub fn client_pair_proof(code: &str, nonce_c: &str) -> Option<String> {
    crypto::proof("pair-client", code, nonce_c, "")
}

pub fn server_pair_proof(code: &str, nonce_c: &str) -> Option<String> {
    crypto::proof("pair-server", code, nonce_c, "")
}

// --- The pairing window -----------------------------------------------------

struct Pairing {
    code: String,
}

fn pairing() -> &'static Mutex<Option<Pairing>> {
    static PAIRING: OnceLock<Mutex<Option<Pairing>>> = OnceLock::new();
    PAIRING.get_or_init(|| Mutex::new(None))
}

/// Six digits, generated from the OS RNG rather than trimmed from a hash, so
/// every code is equally likely.
///
/// Single use and single attempt: `close_pairing` runs on the first wrong
/// answer as well as on success, so this is never a target worth grinding.
pub fn open_pairing() -> Option<String> {
    let hex = crypto::random_hex(4)?;
    let value = u32::from_str_radix(&hex, 16).ok()?;
    let code = format!("{:06}", value % 1_000_000);

    let Ok(mut slot) = pairing().lock() else {
        log_warn!("pairing state is poisoned; not opening a window");
        return None;
    };
    *slot = Some(Pairing { code: code.clone() });
    log_info!("pairing window open");
    Some(code)
}

pub fn close_pairing() {
    if let Ok(mut slot) = pairing().lock()
        && slot.take().is_some()
    {
        log_info!("pairing window closed");
    }
}

pub fn pairing_open() -> bool {
    pairing().lock().is_ok_and(|slot| slot.is_some())
}

/// The code, if a window is open. Taking it does not close the window; the
/// caller decides, because success and failure both end it but for different
/// reasons worth logging separately.
pub fn pairing_code() -> Option<String> {
    pairing().lock().ok()?.as_ref().map(|p| p.code.clone())
}

/// A token for one peer. Not the clock or a pid: this is the only thing between
/// a local process and the tab list.
pub fn generate_token() -> Option<String> {
    crypto::random_hex(24)
}

/// The pairing window, the peer store and the tab state are one per process,
/// exactly as they are in the app. Tests that drive them take turns rather
/// than pretending otherwise.
#[cfg(test)]
pub fn test_turn() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    peers::use_a_scratch_store();
    close_pairing();
    guard
}


#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn nothing_off_the_loopback_is_even_considered() {
        assert!(matches!(
            admit(false, Some(ORIGIN)),
            Err(Refusal::NotLoopback)
        ));
    }

    #[test]
    fn a_handshake_with_no_usable_origin_is_refused() {
        assert!(matches!(admit(true, None), Err(Refusal::MissingOrigin)));
        assert!(matches!(admit(true, Some("   ")), Err(Refusal::MissingOrigin)));
    }

    #[test]
    fn a_pairing_window_admits_an_unknown_origin_and_closing_it_stops_that() {
        let _turn = test_turn();
        let code = open_pairing().expect("the OS RNG must work");
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()), "{code}");
        assert!(matches!(
            admit(true, Some("chrome-extension://unpaired-for-this-test")),
            Ok(Admission::Pairing)
        ));

        close_pairing();
        assert!(!pairing_open());
        assert!(matches!(
            admit(true, Some("chrome-extension://unpaired-for-this-test")),
            Err(Refusal::UnknownOrigin(_))
        ));
    }

    #[test]
    fn a_proof_needs_the_secret_the_nonces_and_the_direction() {
        let (token, nc, ns) = ("0123456789abcdef", "aa", "bb");
        let server = server_resume_proof(token, nc, ns).unwrap();

        assert!(crypto::equal(&server, &server_resume_proof(token, nc, ns).unwrap()));
        assert!(!crypto::equal(&server, &client_resume_proof(token, nc, ns).unwrap()));
        assert!(!crypto::equal(&server, &server_resume_proof("wrong", nc, ns).unwrap()));
        assert!(!crypto::equal(&server, &server_resume_proof(token, "zz", ns).unwrap()));
        assert!(!crypto::equal(&server, &server_resume_proof(token, nc, "zz").unwrap()));
    }

    #[test]
    fn a_pairing_proof_is_not_a_resume_proof() {
        let client = client_pair_proof("123456", "aa").unwrap();
        assert!(!crypto::equal(&client, &server_pair_proof("123456", "aa").unwrap()));
        assert!(!crypto::equal(&client, &client_pair_proof("654321", "aa").unwrap()));
    }

    #[test]
    fn generated_tokens_are_long_and_not_repeated() {
        let a = generate_token().expect("the OS RNG must work");
        let b = generate_token().expect("the OS RNG must work");
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
    }
}
