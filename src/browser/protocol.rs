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
    /// Which folder it is filed under, as a path. Empty for the bar, which is
    /// one level and needs no saying. Only the whole tree fills it, where it is
    /// the difference between two bookmarks with the same title.
    #[serde(default)]
    pub folder: String,
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
        for px in rgba.as_chunks::<4>().0 {
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
        /// The extension's own version, for the log. The protocol number says
        /// what it can speak; this says which build is speaking, which is the
        /// question when Chrome keeps a registered service worker across a
        /// reload and old code carries on answering pings. Defaults, so a build
        /// that predates it is not refused over a label.
        #[serde(default)]
        ext: String,
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
    /// The whole tree, flattened, each entry carrying its folder path.
    ///
    /// Only ever sent in answer to `WantTree`, which is what keeps this off the
    /// version number. An extension that predates it never sends one and the
    /// all-bookmarks square comes up empty; a browser paired with an older exe
    /// is never asked, so it never sends one. Nothing mismatches - the feature
    /// is simply there or not.
    ///
    /// No icons. A favicon is filed by origin, and the origins worth having are
    /// already in hand from the tabs and the bar; the rest fall back to the
    /// shell, exactly as a hand-written site favorite does.
    Tree {
        bookmarks: Vec<Bookmark>,
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
    /// Ask for the whole bookmark tree. Sent when the all-bookmarks square is
    /// clicked, not on connect: the bar is what the panel shows all the time,
    /// and an archive of thousands does not need sending until it is asked for.
    ///
    /// Not a PROTOCOL bump either, for the same reason as `NewTab`. An older
    /// extension drops it, which costs one square rather than every tab.
    WantTree,
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

    /// The extension is half of this app and ships from the same tag, so both
    /// numbers it carries have to match the exe's. Neither did.
    fn extension_manifest() -> &'static str {
        include_str!("../../extension/manifest.json")
    }

    #[test]
    fn the_extension_speaks_this_protocol() {
        let js = include_str!("../../extension/bridge.js");
        let theirs = js
            .lines()
            .find_map(|l| l.trim().strip_prefix("const BRIDGE_PROTOCOL = "))
            .and_then(|v| v.strip_suffix(";"))
            .expect("bridge.js declares BRIDGE_PROTOCOL");
        assert_eq!(theirs, PROTOCOL.to_string(), "the two halves would refuse to pair");
    }

    #[test]
    fn the_extension_carries_the_app_version() {
        // Chrome shows this in chrome://extensions. It sat two releases behind,
        // so a 0.6.0 download introduced itself as 0.4.1.
        let version = extension_manifest()
            .lines()
            .find_map(|l| l.trim().strip_prefix("\"version\": \""))
            .and_then(|v| v.strip_suffix("\","))
            .expect("manifest.json has a version");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
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
    fn a_whole_tree_parses_with_the_folder_each_one_is_filed_under() {
        let json = r#"{"type":"tree","bookmarks":[
            {"id":"31","title":"Rust","url":"https://rust-lang.org/","folder":"dev / langs"},
            {"id":"32","title":"Loose","url":"https://example.com/"}
        ]}"#;
        let Inbound::Tree { bookmarks } = serde_json::from_str(json).unwrap() else {
            panic!("expected a tree");
        };
        assert_eq!(bookmarks[0].folder, "dev / langs");
        // Filed at a root, which is a real place to be. Empty, never absent.
        assert!(bookmarks[1].folder.is_empty());
    }

    /// The bar and the tree are the same shape on the wire, so an old extension
    /// sending a bar without folders still parses.
    #[test]
    fn a_bar_entry_still_parses_without_a_folder() {
        let json = r#"{"type":"bookmarks","bookmarks":[{"id":"9","url":"https://example.com/"}]}"#;
        let Inbound::Bookmarks { bookmarks, .. } = serde_json::from_str(json).unwrap() else {
            panic!("expected a bookmark list");
        };
        assert!(bookmarks[0].folder.is_empty());
    }

    #[test]
    fn asking_for_the_tree_is_a_type_the_extension_can_switch_on() {
        let json = serde_json::to_string(&Outbound::WantTree).unwrap();
        assert_eq!(json, r#"{"type":"wanttree"}"#);
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
