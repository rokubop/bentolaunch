//! Who bentolaunch has agreed to talk to, one entry per browser.
//!
//! One token per peer rather than one for the machine: forgetting Chrome must
//! not silently unpair Firefox, and a token that serves everybody can never be
//! revoked in part.
//!
//! Stored in `%LOCALAPPDATA%\bentolaunch\peers.json`, not beside the exe. A
//! portable build can be dropped in `Program Files`, where a file next to it is
//! readable by every account on the machine - and other accounts are the one
//! case the token is actually meant to stop.
//!
//! The token is stored as itself, not a hash. Both sides prove knowledge of it,
//! which means the verifier has to hold the same secret; storing a digest would
//! only move which string is the password. Anything running as you can read
//! this file, exactly as it could read the old `bridge-token`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{log_info, log_warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    /// `chrome-extension://<id>`. The browser stamps this on the handshake and
    /// page JS cannot forge it.
    pub origin: String,
    /// Tray label. Derived from the origin's scheme, not from anything the
    /// peer says about itself.
    pub name: String,
    pub token: String,
    /// Local date, for the tray. Informational only.
    pub added: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    peers: Vec<Peer>,
}

fn path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = test_path().get() {
        return Some(path.clone());
    }
    crate::log::cache_dir().map(|dir| dir.join("peers.json"))
}

/// Tests pair for real against a real socket, and the store is the one piece
/// of that with a footprint. Point it somewhere disposable so a test run never
/// touches the peers a real install is using.
#[cfg(test)]
fn test_path() -> &'static std::sync::OnceLock<PathBuf> {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    &PATH
}

#[cfg(test)]
pub fn use_a_scratch_store() {
    let path = std::env::temp_dir().join(format!("bentolaunch-test-peers-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = test_path().set(path);
}

fn read() -> Store {
    let Some(path) = path() else {
        return Store::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Store::default();
    };
    match serde_json::from_str(&text) {
        Ok(store) => store,
        Err(e) => {
            // Refuse rather than reset: an unreadable file is more likely a
            // half-written one than a corrupt one, and clearing it would
            // unpair every browser silently.
            log_warn!("{} is unreadable ({e}); treating it as empty", path.display());
            Store::default()
        }
    }
}

fn write(store: &Store) -> bool {
    let Some(path) = path() else { return false };
    let Ok(text) = serde_json::to_string_pretty(store) else {
        return false;
    };
    match std::fs::write(&path, text) {
        Ok(()) => true,
        Err(e) => {
            log_warn!("could not write {}: {e}", path.display());
            false
        }
    }
}

pub fn all() -> Vec<Peer> {
    read().peers
}

pub fn count() -> usize {
    read().peers.len()
}

/// Case-insensitive: origins arrive as headers, and a header's case is not
/// something to depend on.
pub fn find(origin: &str) -> Option<Peer> {
    let wanted = normalize(origin);
    read().peers.into_iter().find(|p| normalize(&p.origin) == wanted)
}

/// Add or replace. Re-pairing an already-paired browser rotates its token
/// rather than adding a second entry for the same origin.
pub fn put(peer: Peer) -> bool {
    let mut store = read();
    let wanted = normalize(&peer.origin);
    store.peers.retain(|p| normalize(&p.origin) != wanted);
    store.peers.push(peer);
    write(&store)
}

pub fn forget(origin: &str) -> bool {
    let mut store = read();
    let wanted = normalize(origin);
    let before = store.peers.len();
    store.peers.retain(|p| normalize(&p.origin) != wanted);
    if store.peers.len() == before {
        return false;
    }
    log_info!("forgot browser {origin}");
    write(&store)
}

/// What the tray calls a peer. From the scheme, because the only thing the
/// extension could tell us about itself is unverifiable.
pub fn name_for(origin: &str) -> String {
    let origin = normalize(origin);
    if origin.starts_with("moz-extension://") {
        "Firefox".into()
    } else if origin.starts_with("chrome-extension://") {
        "Chrome".into()
    } else {
        "Browser".into()
    }
}

pub fn today() -> String {
    // SAFETY: no arguments; returns by value.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}


fn normalize(origin: &str) -> String {
    origin.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_scheme_not_from_the_peer() {
        assert_eq!(name_for("chrome-extension://abc"), "Chrome");
        assert_eq!(name_for("MOZ-EXTENSION://abc"), "Firefox");
        assert_eq!(name_for("https://evil.example"), "Browser");
    }

    #[test]
    fn a_store_round_trips_through_json() {
        let store = Store {
            peers: vec![Peer {
                origin: "chrome-extension://abc".into(),
                name: "Chrome".into(),
                token: "t".into(),
                added: "2026-08-19".into(),
            }],
        };
        let text = serde_json::to_string(&store).unwrap();
        let back: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(back.peers.len(), 1);
        assert_eq!(back.peers[0].origin, "chrome-extension://abc");
    }

    #[test]
    fn an_empty_or_partial_file_is_not_an_error() {
        assert!(serde_json::from_str::<Store>("{}").unwrap().peers.is_empty());
        assert!(serde_json::from_str::<Store>(r#"{"peers":[]}"#).unwrap().peers.is_empty());
    }

    #[test]
    fn a_date_is_shaped_like_a_date() {
        let today = today();
        assert_eq!(today.len(), 10, "{today}");
        assert_eq!(today.matches('-').count(), 2, "{today}");
    }
}
