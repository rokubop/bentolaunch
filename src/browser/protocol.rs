//! What bentolaunch and the extension say to each other. JSON over the socket.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id: i64,
    #[serde(rename = "windowId")]
    pub window_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub active: bool,
    /// Key into the `icons` map. Shared by origin, so tabs on the same site
    /// resolve to one bitmap.
    #[serde(default)]
    pub icon: Option<String>,
}

impl Tab {
    /// Tile's second line. Whole URL if it does not parse as one.
    pub fn host(&self) -> &str {
        let rest = self
            .url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        host.strip_prefix("www.").unwrap_or(host)
    }
}

/// One entry off the browser's bookmarks bar. No `windowId` and no tab id:
/// activating this opens the URL through the shell, so it never goes back over
/// the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub icon: Option<String>,
}

impl Bookmark {
    /// Tile's second line, and what a titleless bookmark is named by.
    pub fn host(&self) -> &str {
        let rest = self
            .url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        host.strip_prefix("www.").unwrap_or(host)
    }
}

/// A favicon the extension already decoded. Raw RGBA rather than PNG, so bentolaunch
/// needs no image decoder and no COM on the socket thread.
#[derive(Debug, Clone, Deserialize)]
pub struct IconData {
    pub w: u32,
    pub h: u32,
    /// base64, row-major, top-down.
    pub rgba: String,
}

impl IconData {
    /// Premultiplied BGRA, which is what the renderer takes.
    pub fn to_pixels(&self) -> Option<crate::shell::icons::IconPixels> {
        if self.w == 0 || self.h == 0 || self.w > 512 || self.h > 512 {
            return None;
        }
        let rgba = crate::browser::base64::decode(&self.rgba)?;
        let expected = self.w as usize * self.h as usize * 4;
        if rgba.len() != expected {
            return None;
        }

        let mut bgra = Vec::with_capacity(expected);
        for px in rgba.chunks_exact(4) {
            let (r, g, b, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3]);
            let scale = |c: u32| ((c * a as u32 + 127) / 255) as u8;
            bgra.extend_from_slice(&[scale(b), scale(g), scale(r), a]);
        }
        Some(crate::shell::icons::IconPixels { width: self.w, height: self.h, bgra })
    }
}

/// What this build of the bridge speaks. Bumped when a frame changes shape or
/// an exchange changes meaning.
///
/// It exists because the exe and the extension are downloaded separately: they
/// used to be one checkout that changed together, and now they drift. Without
/// this, an extension one version behind fails as "not paired", which sends the
/// user looking for a pairing problem they do not have.
/// 3 is the BentoPick rename: the proof's domain separator carries the name, so
/// both sides changed at once. Version is checked before the proof, so a stale
/// extension gets "update it", not silence.
pub const PROTOCOL: u32 = 3;

/// Extension to bentolaunch.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Inbound {
    /// Always the first frame. Nothing else is read until the exchange it
    /// opens has finished.
    Hello {
        /// Absent from any build that predates versioning, which is exactly the
        /// case this needs to name, so it defaults rather than failing.
        #[serde(default)]
        v: u32,
        /// A string, not an enum: an unknown mode from a newer extension has to
        /// survive parsing far enough for the version check to explain itself.
        mode: String,
        nonce: String,
        /// Pairing only: the client goes first there.
        #[serde(default)]
        proof: String,
    },
    /// Resuming: the client's half, after it has checked the server's.
    Prove {
        proof: String,
    },
    Tabs {
        tabs: Vec<Tab>,
        /// Only the ones bentolaunch has not been sent yet on this connection.
        #[serde(default)]
        icons: HashMap<String, IconData>,
    },
    /// The bookmarks bar. Sent once on connect and again on every edit, so it
    /// is a whole list rather than a delta.
    Bookmarks {
        bookmarks: Vec<Bookmark>,
        #[serde(default)]
        icons: HashMap<String, IconData>,
    },
    Pong,
}

/// bentolaunch to the extension.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Outbound {
    /// The one thing said to a caller that has proved nothing, and the only
    /// refusal that is ever explained: a version gap is the user's to fix, and
    /// it gives away nothing an attacker could not read in the repo.
    Outdated {
        protocol: u32,
    },
    /// Resuming: the server's half, sent before the client has proved
    /// anything, so a client talking to an impostor can hang up before it
    /// sends a single tab title.
    Challenge {
        nonce: String,
        proof: String,
    },
    /// Pairing succeeded. The token is this peer's alone, and this is the only
    /// time it travels; `proof` is what tells the extension the token came
    /// from the app that showed the code.
    Paired {
        token: String,
        proof: String,
    },
    Focus {
        #[serde(rename = "tabId")]
        tab_id: i64,
        #[serde(rename = "windowId")]
        window_id: i64,
    },
    /// Open a tab. No id: which window it lands in is the browser's business,
    /// and it already knows which one you were last in.
    ///
    /// Deliberately not a PROTOCOL bump. An extension predating this drops an
    /// unknown type on the floor, which costs one button; refusing the whole
    /// connection over it would cost every tab.
    NewTab,
    /// Keeps the MV3 worker alive. bentolaunch drives it: the worker cannot be
    /// trusted to wake itself.
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str) -> Tab {
        Tab {
            id: 1,
            window_id: 2,
            title: "t".into(),
            url: url.into(),
            active: false,
            icon: None,
        }
    }

    #[test]
    fn a_hello_from_a_build_that_predates_versioning_still_parses() {
        // The whole point of the version field: this has to get far enough to
        // be told it is out of date, not fail as unreadable.
        let json = r#"{"type":"hello","mode":"resume","nonce":"aa"}"#;
        let Inbound::Hello { v, mode, proof, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a hello");
        };
        assert_eq!(v, 0);
        assert_eq!(mode, "resume");
        assert!(proof.is_empty());
    }

    #[test]
    fn a_hello_naming_a_mode_this_build_has_never_heard_of_still_parses() {
        let json = r#"{"type":"hello","v":9,"mode":"something-newer","nonce":"aa"}"#;
        let Inbound::Hello { v, mode, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a hello");
        };
        assert_eq!(v, 9);
        assert_eq!(mode, "something-newer");
    }

    #[test]
    fn outdated_names_the_version_this_build_speaks() {
        let json = serde_json::to_string(&Outbound::Outdated { protocol: PROTOCOL }).unwrap();
        assert!(json.contains(r#""type":"outdated""#), "{json}");
        assert!(json.contains(&format!(r#""protocol":{PROTOCOL}"#)), "{json}");
    }

    #[test]
    fn a_tab_list_from_the_extension_parses() {
        let json = r#"{"type":"tabs","tabs":[
            {"id":7,"windowId":3,"title":"Docs","url":"https://doc.rust-lang.org/std/","active":true}
        ]}"#;
        let Inbound::Tabs { tabs, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a tab list");
        };
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].id, 7);
        assert_eq!(tabs[0].window_id, 3);
        assert_eq!(tabs[0].title, "Docs");
        assert!(tabs[0].active);
    }

    #[test]
    fn a_bookmark_list_from_the_extension_parses() {
        let json = r#"{"type":"bookmarks","bookmarks":[
            {"id":"14","title":"Rust","url":"https://www.rust-lang.org/learn"}
        ]}"#;
        let Inbound::Bookmarks { bookmarks, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a bookmark list");
        };
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].id, "14");
        assert_eq!(bookmarks[0].title, "Rust");
        assert_eq!(bookmarks[0].host(), "rust-lang.org");
    }

    #[test]
    fn a_bookmark_with_no_title_keeps_its_url() {
        // The bar carries these: a bookmarklet, or one saved from a page that
        // had no title. The host is what names the tile.
        let json = r#"{"type":"bookmarks","bookmarks":[{"id":"9","url":"https://example.com/x"}]}"#;
        let Inbound::Bookmarks { bookmarks, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a bookmark list");
        };
        assert!(bookmarks[0].title.is_empty());
        assert_eq!(bookmarks[0].host(), "example.com");
    }

    #[test]
    fn a_tab_missing_optional_fields_still_parses() {
        // A loading tab has no title yet. Do not drop the whole list for it.
        let json = r#"{"type":"tabs","tabs":[{"id":1,"windowId":1}]}"#;
        let Inbound::Tabs { tabs, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a tab list");
        };
        assert_eq!(tabs[0].title, "");
    }

    #[test]
    fn focus_serializes_to_the_names_the_extension_reads() {
        let json = serde_json::to_string(&Outbound::Focus { tab_id: 7, window_id: 3 }).unwrap();
        assert!(json.contains(r#""type":"focus""#), "{json}");
        assert!(json.contains(r#""tabId":7"#), "{json}");
        assert!(json.contains(r#""windowId":3"#), "{json}");
    }

    #[test]
    fn hosts_are_stripped_to_what_fits_a_tile() {
        assert_eq!(tab("https://www.github.com/rust-lang/rust").host(), "github.com");
        assert_eq!(tab("https://doc.rust-lang.org/std/?q=1").host(), "doc.rust-lang.org");
        assert_eq!(tab("http://localhost:3000/x").host(), "localhost:3000");
        assert_eq!(tab("about:blank").host(), "about:blank");
        assert_eq!(tab("").host(), "");
    }

    #[test]
    fn a_favicon_becomes_premultiplied_bgra() {
        // One opaque red pixel and one half-transparent white one.
        let icon = IconData { w: 2, h: 1, rgba: "/wAA/////4A=".into() };
        let px = icon.to_pixels().unwrap();
        assert_eq!((px.width, px.height), (2, 1));
        assert_eq!(px.bgra, vec![0, 0, 255, 255, 128, 128, 128, 128]);
    }

    #[test]
    fn a_favicon_that_does_not_add_up_is_dropped() {
        assert!(IconData { w: 4, h: 4, rgba: "Zm9v".into() }.to_pixels().is_none());
        assert!(IconData { w: 0, h: 0, rgba: String::new() }.to_pixels().is_none());
        assert!(IconData { w: 9999, h: 9999, rgba: String::new() }.to_pixels().is_none());
        assert!(IconData { w: 1, h: 1, rgba: "!!!!".into() }.to_pixels().is_none());
    }

    #[test]
    fn junk_on_the_socket_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Inbound>("not json").is_err());
        assert!(serde_json::from_str::<Inbound>(r#"{"type":"nope"}"#).is_err());
        assert!(serde_json::from_str::<Inbound>(r#"{"type":"tabs"}"#).is_err());
    }
}
