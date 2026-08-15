//! Adding a tile to `flick.toml` without flattening the file.
//!
//! `toml_edit` rather than re-serialising through serde: the config is meant to
//! be hand-edited, and round-tripping it through `Config` would silently discard
//! every comment, blank line and key ordering the user put there. A tool that
//! eats your comments is a tool you stop hand-editing.

use std::path::PathBuf;

use toml_edit::{Array, DocumentMut, Item as TomlItem, Table, value};

use crate::config::Config;
use crate::{log_info, log_warn};

/// Section created when there is nowhere else to put a pin.
const FALLBACK_TITLE: &str = "Places";

/// Append a target to the first manual section, creating one if needed.
///
/// Returns the section it landed in. The config watcher picks the change up, so
/// there is no separate reload path.
pub fn add(target: &str) -> Option<String> {
    add_to(&Config::path()?, target)
}

fn add_to(path: &PathBuf, target: &str) -> Option<String> {
    let mut doc = read(path)?;

    let sections = match doc["sections"].or_insert(TomlItem::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
    {
        Some(sections) => sections,
        None => {
            log_warn!("`sections` in the config is not a list of sections; not adding the pin");
            return None;
        }
    };

    let existing = sections
        .iter()
        .position(|t| t.get("source").and_then(|s| s.as_str()) == Some("manual"));

    let index = match existing {
        Some(index) => index,
        None => {
            sections.push(new_manual_section());
            sections.len() - 1
        }
    };
    let manual = sections.get_mut(index)?;

    let title = manual
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or(FALLBACK_TITLE)
        .to_owned();

    let items = manual["items"]
        .or_insert(value(Array::new()))
        .as_array_mut()?;

    if items
        .iter()
        .any(|existing| existing.as_str() == Some(target))
    {
        log_info!("already pinned, skipping: {target}");
        return Some(title);
    }

    items.push(target);
    // One entry per line: the list is meant to stay readable after editing.
    for entry in items.iter_mut() {
        entry.decor_mut().set_prefix("\n    ");
    }
    items.set_trailing("\n");
    items.set_trailing_comma(true);

    match std::fs::write(path, doc.to_string()) {
        Ok(()) => {
            log_info!("pinned \"{target}\" to section \"{title}\"");
            Some(title)
        }
        Err(e) => {
            log_warn!("could not write {}: {e}", path.display());
            None
        }
    }
}

fn read(path: &PathBuf) -> Option<DocumentMut> {
    // A missing config is normal on a first run that never showed the panel.
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| {
        toml::to_string_pretty(&Config::default()).unwrap_or_default()
    });
    match text.parse::<DocumentMut>() {
        Ok(doc) => Some(doc),
        Err(e) => {
            log_warn!("config is not valid TOML ({e}); refusing to overwrite it");
            None
        }
    }
}

fn new_manual_section() -> Table {
    let mut table = Table::new();
    table["title"] = value(FALLBACK_TITLE);
    table["source"] = value("manual");
    table["items"] = value(Array::new());
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("flick-pins-test-{name}.toml"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn adds_to_the_existing_manual_section() {
        let path = scratch("existing");
        std::fs::write(
            &path,
            "hotkey = \"alt+`\"\n\n[[sections]]\ntitle = \"Places\"\nsource = \"manual\"\nitems = []\n",
        )
        .unwrap();

        assert_eq!(add_to(&path, r"R:\dev").as_deref(), Some("Places"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r"R:\dev"), "target missing from {text}");

        let parsed: Config = toml::from_str(&text).unwrap();
        let places = parsed.sections.iter().find(|s| s.title == "Places").unwrap();
        assert_eq!(places.items.len(), 1);
        assert_eq!(places.items[0].target(), r"R:\dev");
    }

    #[test]
    fn hand_written_comments_and_keys_survive() {
        let path = scratch("comments");
        let original = "# my flick config\nhotkey = \"ctrl+alt+q\"  # trailing note\n\n\
             [[sections]]\ntitle = \"Windows\"\nsource = \"windows\"\n\n\
             # things I open a lot\n[[sections]]\ntitle = \"Places\"\nsource = \"manual\"\nitems = []\n";
        std::fs::write(&path, original).unwrap();

        add_to(&path, "ms-settings:display").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(text.contains("# my flick config"));
        assert!(text.contains("# trailing note"));
        assert!(text.contains("# things I open a lot"));
        assert!(text.contains("ctrl+alt+q"));
        assert!(text.contains("ms-settings:display"));
    }

    #[test]
    fn creates_a_manual_section_when_there_is_none() {
        let path = scratch("create");
        std::fs::write(
            &path,
            "[[sections]]\ntitle = \"Windows\"\nsource = \"windows\"\n",
        )
        .unwrap();

        assert_eq!(add_to(&path, r"C:\Windows").as_deref(), Some(FALLBACK_TITLE));
        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.sections[1].items[0].target(), r"C:\Windows");
    }

    #[test]
    fn pinning_the_same_target_twice_is_a_no_op() {
        let path = scratch("dupe");
        std::fs::write(&path, "[[sections]]\ntitle = \"P\"\nsource = \"manual\"\nitems = []\n")
            .unwrap();

        add_to(&path, r"R:\dev").unwrap();
        add_to(&path, r"R:\dev").unwrap();

        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.sections[0].items.len(), 1);
    }

    #[test]
    fn a_broken_config_is_left_untouched() {
        let path = scratch("broken");
        let garbage = "this is not = = valid toml [[[";
        std::fs::write(&path, garbage).unwrap();

        assert!(add_to(&path, r"R:\dev").is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), garbage);
    }

    #[test]
    fn the_result_still_parses_as_a_config() {
        let path = scratch("roundtrip");
        std::fs::write(&path, toml::to_string_pretty(&Config::default()).unwrap()).unwrap();

        add_to(&path, r"R:\dev").unwrap();
        add_to(&path, "ms-settings:display").unwrap();
        add_to(&path, r"shell:AppsFolder\Something!App").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&text).expect("config must survive three pins");
        let manual = parsed
            .sections
            .iter()
            .find(|s| s.source == crate::config::Source::Manual)
            .unwrap();
        assert_eq!(manual.items.len(), 3);
    }
}
