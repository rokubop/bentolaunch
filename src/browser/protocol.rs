//! What flick and the extension say to each other. JSON over the socket.

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

/// A favicon the extension already decoded. Raw RGBA rather than PNG, so flick
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

/// Extension to flick.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Inbound {
    Tabs {
        tabs: Vec<Tab>,
        /// Only the ones flick has not been sent yet on this connection.
        #[serde(default)]
        icons: HashMap<String, IconData>,
    },
    Pong,
}

/// flick to the extension.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Outbound {
    Focus {
        #[serde(rename = "tabId")]
        tab_id: i64,
        #[serde(rename = "windowId")]
        window_id: i64,
    },
    /// Keeps the MV3 worker alive. flick drives it: the worker cannot be
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
