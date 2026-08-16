//! What flick and the extension say to each other. JSON over the socket.

use serde::{Deserialize, Serialize};

/// One open tab, as the extension sees it.
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
}

impl Tab {
    /// Host without the `www.`, for the tile's second line. Falls back to the
    /// whole URL when it does not look like one.
    pub fn host(&self) -> &str {
        let rest = self
            .url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        host.strip_prefix("www.").unwrap_or(host)
    }
}

/// Extension to flick.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Inbound {
    /// The full tab list. Sent on connect and whenever it changes.
    Tabs { tabs: Vec<Tab> },
    Pong,
}

/// flick to the extension.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Outbound {
    /// Switch to this tab and bring its window forward.
    Focus {
        #[serde(rename = "tabId")]
        tab_id: i64,
        #[serde(rename = "windowId")]
        window_id: i64,
    },
    /// Keeps the MV3 service worker alive. Chrome resets its idle timer on
    /// socket traffic, so flick drives this rather than trusting the worker to
    /// wake itself up.
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str) -> Tab {
        Tab { id: 1, window_id: 2, title: "t".into(), url: url.into(), active: false }
    }

    #[test]
    fn a_tab_list_from_the_extension_parses() {
        let json = r#"{"type":"tabs","tabs":[
            {"id":7,"windowId":3,"title":"Docs","url":"https://doc.rust-lang.org/std/","active":true}
        ]}"#;
        let Inbound::Tabs { tabs } = serde_json::from_str(json).unwrap() else {
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
        // A tab still loading has no title yet. Dropping the whole list over
        // one of them would be the wrong trade.
        let json = r#"{"type":"tabs","tabs":[{"id":1,"windowId":1}]}"#;
        let Inbound::Tabs { tabs } = serde_json::from_str(json).unwrap() else {
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
    fn junk_on_the_socket_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Inbound>("not json").is_err());
        assert!(serde_json::from_str::<Inbound>(r#"{"type":"nope"}"#).is_err());
        assert!(serde_json::from_str::<Inbound>(r#"{"type":"tabs"}"#).is_err());
    }
}
