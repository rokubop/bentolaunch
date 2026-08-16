//! Adding a tile to `dashpick.toml` without flattening the file.
//!
//! `toml_edit` rather than re-serialising through serde: the config is meant to
//! be hand-edited, and round-tripping it through `Config` would silently discard
//! every comment, blank line and key ordering the user put there. A tool that
//! eats your comments is a tool you stop hand-editing.

use std::path::Path;

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item as TomlItem, Table, Value, value};

use crate::config::Config;
use crate::{log_info, log_warn};

/// Section created when there is nowhere else to put a pin.
const FALLBACK_TITLE: &str = "Places";

/// Append a target to the first manual section, creating one if needed.
///
/// Returns the section it landed in. The config watcher picks the change up, so
/// there is no separate reload path.
pub fn add(target: &str) -> Option<String> {
    add_to(&Config::path()?, None, target)
}

/// Append a target to a named manual section, falling back to the first one when
/// that section is gone or is not manual. This is where a drop lands: on the
/// section the cursor was over.
pub fn add_into(section: Option<&str>, target: &str) -> Option<String> {
    add_to(&Config::path()?, section, target)
}

/// Drop one entry from a manual section. Returns whether the file changed.
pub fn remove(section: &str, target: &str) -> bool {
    Config::path().is_some_and(|path| remove_from(&path, section, target))
}

/// Rewrite a manual section's `items` in the given target order. Entries not
/// named keep their relative order at the end, so a stale list never loses a pin.
pub fn reorder(section: &str, targets: &[String]) -> bool {
    Config::path().is_some_and(|path| reorder_in(&path, section, targets))
}

/// Record the display order of a taskbar section.
pub fn set_order(section: &str, names: &[String]) -> bool {
    Config::path().is_some_and(|path| set_order_in(&path, section, names))
}

fn add_to(path: &Path, section: Option<&str>, target: &str) -> Option<String> {
    let mut doc = read(path)?;
    let sections = sections_mut(&mut doc)?;

    let index = match section.and_then(|title| manual_named(sections, title)) {
        Some(index) => index,
        None => match first_manual(sections) {
            Some(index) => index,
            None => {
                sections.push(new_manual_section());
                sections.len() - 1
            }
        },
    };
    let manual = sections.get_mut(index)?;
    let title = title_of(manual);

    let items = manual["items"]
        .or_insert(value(Array::new()))
        .as_array_mut()?;

    if items.iter().any(|entry| target_of(entry) == Some(target)) {
        log_info!("already pinned, skipping: {target}");
        return Some(title);
    }

    items.push(target);
    stack(items);

    write(path, &doc).then(|| {
        log_info!("pinned \"{target}\" to section \"{title}\"");
        title
    })
}

fn remove_from(path: &Path, section: &str, target: &str) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let Some(sections) = sections_mut(&mut doc) else { return false };
    let Some(index) = manual_named(sections, section) else {
        log_warn!("no manual section titled \"{section}\"; nothing removed");
        return false;
    };
    let Some(items) = sections
        .get_mut(index)
        .and_then(|table| table.get_mut("items"))
        .and_then(|items| items.as_array_mut())
    else {
        return false;
    };

    let before = items.len();
    items.retain(|entry| target_of(entry) != Some(target));
    if items.len() == before {
        return false;
    }
    stack(items);

    write(path, &doc) && {
        log_info!("unpinned \"{target}\" from section \"{section}\"");
        true
    }
}

fn reorder_in(path: &Path, section: &str, targets: &[String]) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let Some(sections) = sections_mut(&mut doc) else { return false };
    let Some(index) = manual_named(sections, section) else {
        log_warn!("no manual section titled \"{section}\"; order not saved");
        return false;
    };
    let Some(items) = sections
        .get_mut(index)
        .and_then(|table| table.get_mut("items"))
        .and_then(|items| items.as_array_mut())
    else {
        return false;
    };

    // Entries are moved, not rebuilt, so a `{ title = ..., target = ... }` form
    // keeps its title.
    let existing: Vec<Value> = items.iter().cloned().collect();
    let mut taken = vec![false; existing.len()];
    let mut ordered: Vec<Value> = Vec::with_capacity(existing.len());

    for want in targets {
        if let Some(at) = existing
            .iter()
            .enumerate()
            .position(|(slot, entry)| !taken[slot] && target_of(entry) == Some(want.as_str()))
        {
            taken[at] = true;
            ordered.push(existing[at].clone());
        }
    }
    for (slot, entry) in existing.iter().enumerate() {
        if !taken[slot] {
            ordered.push(entry.clone());
        }
    }

    items.clear();
    for entry in ordered {
        items.push_formatted(entry);
    }
    stack(items);

    write(path, &doc) && {
        log_info!("saved the order of section \"{section}\"");
        true
    }
}

fn set_order_in(path: &Path, section: &str, names: &[String]) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let Some(sections) = sections_mut(&mut doc) else { return false };
    let Some(index) = sections
        .iter()
        .position(|table| title_of(table) == section && source_of(table) == Some("taskbar"))
    else {
        log_warn!("no taskbar section titled \"{section}\"; order not saved");
        return false;
    };
    let Some(table) = sections.get_mut(index) else {
        return false;
    };

    let mut list = Array::new();
    for name in names {
        list.push(name.as_str());
    }
    stack(&mut list);
    table["order"] = value(list);

    write(path, &doc) && {
        log_info!("saved the order of section \"{section}\"");
        true
    }
}

fn sections_mut(doc: &mut DocumentMut) -> Option<&mut ArrayOfTables> {
    match doc["sections"]
        .or_insert(TomlItem::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
    {
        Some(sections) => Some(sections),
        None => {
            log_warn!("`sections` in the config is not a list of sections; leaving it alone");
            None
        }
    }
}

fn title_of(table: &Table) -> String {
    table
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or(FALLBACK_TITLE)
        .to_owned()
}

fn source_of(table: &Table) -> Option<&str> {
    table.get("source").and_then(|s| s.as_str())
}

fn first_manual(sections: &ArrayOfTables) -> Option<usize> {
    sections
        .iter()
        .position(|table| source_of(table) == Some("manual"))
}

fn manual_named(sections: &ArrayOfTables, title: &str) -> Option<usize> {
    sections
        .iter()
        .position(|table| title_of(table) == title && source_of(table) == Some("manual"))
}

/// A manual entry is either the bare parsing name or `{ title, target }`.
fn target_of(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(text) => Some(text.value()),
        Value::InlineTable(table) => table.get("target").and_then(|t| t.as_str()),
        _ => None,
    }
}

/// One entry per line: these lists are meant to stay readable after editing.
fn stack(items: &mut Array) {
    if items.is_empty() {
        items.set_trailing("");
        items.set_trailing_comma(false);
        return;
    }
    for entry in items.iter_mut() {
        entry.decor_mut().set_prefix("\n    ");
    }
    items.set_trailing("\n");
    items.set_trailing_comma(true);
}

/// Through `toml_edit` like every other write, so comments survive.
pub fn set_browser_token(token: &str) -> Option<String> {
    let path = Config::path()?;
    let mut doc = read(&path)?;
    doc["browser"]["token"] = value(token);
    write(&path, &doc).then(|| token.to_owned())
}

fn write(path: &Path, doc: &DocumentMut) -> bool {
    match std::fs::write(path, doc.to_string()) {
        Ok(()) => true,
        Err(e) => {
            log_warn!("could not write {}: {e}", path.display());
            false
        }
    }
}

fn read(path: &Path) -> Option<DocumentMut> {
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
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("dashpick-pins-test-{name}.toml"));
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

        assert_eq!(add_to(&path, None, r"R:\dev").as_deref(), Some("Places"));
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
        let original = "# my dashpick config\nhotkey = \"ctrl+alt+q\"  # trailing note\n\n\
             [[sections]]\ntitle = \"Windows\"\nsource = \"windows\"\n\n\
             # things I open a lot\n[[sections]]\ntitle = \"Places\"\nsource = \"manual\"\nitems = []\n";
        std::fs::write(&path, original).unwrap();

        add_to(&path, None, "ms-settings:display").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(text.contains("# my dashpick config"));
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

        assert_eq!(add_to(&path, None, r"C:\Windows").as_deref(), Some(FALLBACK_TITLE));
        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.sections[1].items[0].target(), r"C:\Windows");
    }

    #[test]
    fn pinning_the_same_target_twice_is_a_no_op() {
        let path = scratch("dupe");
        std::fs::write(&path, "[[sections]]\ntitle = \"P\"\nsource = \"manual\"\nitems = []\n")
            .unwrap();

        add_to(&path, None, r"R:\dev").unwrap();
        add_to(&path, None, r"R:\dev").unwrap();

        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.sections[0].items.len(), 1);
    }

    #[test]
    fn a_broken_config_is_left_untouched() {
        let path = scratch("broken");
        let garbage = "this is not = = valid toml [[[";
        std::fs::write(&path, garbage).unwrap();

        assert!(add_to(&path, None, r"R:\dev").is_none());
        assert!(!remove_from(&path, "Places", r"R:\dev"));
        assert!(!reorder_in(&path, "Places", &[r"R:\dev".into()]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), garbage);
    }

    #[test]
    fn the_result_still_parses_as_a_config() {
        let path = scratch("roundtrip");
        std::fs::write(&path, toml::to_string_pretty(&Config::default()).unwrap()).unwrap();

        add_to(&path, None, r"R:\dev").unwrap();
        add_to(&path, None, "ms-settings:display").unwrap();
        add_to(&path, None, r"shell:AppsFolder\Something!App").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&text).expect("config must survive three pins");
        let manual = parsed
            .sections
            .iter()
            .find(|s| s.source == crate::config::Source::Manual)
            .unwrap();
        assert_eq!(manual.items.len(), 3);
    }

    // --- removing, reordering, taskbar order ---

    /// Two manual sections plus a taskbar one, which is the shape rearranging has
    /// to get right: writes must land in the section that was dragged.
    fn several_sections(name: &str) -> PathBuf {
        let path = scratch(name);
        std::fs::write(
            &path,
            "[[sections]]\ntitle = \"Launch\"\nsource = \"taskbar\"\n\n\
             [[sections]]\ntitle = \"Places\"\nsource = \"manual\"\n\
             items = [\"R:\\\\dev\", { title = \"Display\", target = \"ms-settings:display\" }, \"C:\\\\Windows\"]\n\n\
             [[sections]]\ntitle = \"Web\"\nsource = \"manual\"\nitems = [\"https://example.com\"]\n",
        )
        .unwrap();
        path
    }

    fn manual(path: &PathBuf, title: &str) -> Vec<String> {
        let parsed: Config = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        parsed
            .sections
            .iter()
            .find(|s| s.title == title)
            .unwrap()
            .items
            .iter()
            .map(|i| i.target().to_owned())
            .collect()
    }

    #[test]
    fn a_pin_lands_in_the_named_section_not_the_first_one() {
        let path = several_sections("named");
        assert_eq!(
            add_to(&path, Some("Web"), "https://rust-lang.org").as_deref(),
            Some("Web")
        );
        assert_eq!(manual(&path, "Web").len(), 2);
        assert_eq!(manual(&path, "Places").len(), 3);
    }

    #[test]
    fn a_pin_falls_back_to_the_first_manual_section() {
        let path = several_sections("fallback");
        // "Launch" exists but is a taskbar section, so it cannot take a pin.
        assert_eq!(add_to(&path, Some("Launch"), r"D:\x").as_deref(), Some("Places"));
        assert_eq!(manual(&path, "Places").len(), 4);
    }

    #[test]
    fn removing_takes_out_one_entry_and_leaves_the_rest() {
        let path = several_sections("remove");
        assert!(remove_from(&path, "Places", "ms-settings:display"));
        assert_eq!(manual(&path, "Places"), [r"R:\dev", r"C:\Windows"]);
        // Other sections are untouched.
        assert_eq!(manual(&path, "Web"), ["https://example.com"]);
    }

    #[test]
    fn removing_something_that_is_not_there_writes_nothing() {
        let path = several_sections("absent");
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(!remove_from(&path, "Places", r"Q:\nope"));
        assert!(!remove_from(&path, "Nowhere", r"R:\dev"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn reordering_rewrites_the_section_in_the_given_order() {
        let path = several_sections("reorder");
        let order = vec![
            r"C:\Windows".to_string(),
            "ms-settings:display".to_string(),
            r"R:\dev".to_string(),
        ];
        assert!(reorder_in(&path, "Places", &order));
        assert_eq!(manual(&path, "Places"), order);
    }

    #[test]
    fn reordering_keeps_a_named_entrys_title() {
        let path = several_sections("titles");
        let order = vec!["ms-settings:display".to_string(), r"R:\dev".to_string()];
        assert!(reorder_in(&path, "Places", &order));

        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let places = parsed.sections.iter().find(|s| s.title == "Places").unwrap();
        assert_eq!(places.items[0].title(), Some("Display"));
        // Anything the order left out follows, rather than vanishing.
        assert_eq!(places.items[2].target(), r"C:\Windows");
    }

    #[test]
    fn taskbar_order_is_written_as_names() {
        let path = several_sections("taskbar");
        let names = vec!["Steam".to_string(), "Google Chrome".to_string()];
        assert!(set_order_in(&path, "Launch", &names));
        assert!(!set_order_in(&path, "Places", &names), "manual is not taskbar");

        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let launch = parsed.sections.iter().find(|s| s.title == "Launch").unwrap();
        assert_eq!(launch.order, names);
    }

    #[test]
    fn an_emptied_section_stays_valid_toml() {
        let path = scratch("emptied");
        std::fs::write(
            &path,
            "[[sections]]\ntitle = \"P\"\nsource = \"manual\"\nitems = [\"R:\\\\dev\"]\n",
        )
        .unwrap();

        assert!(remove_from(&path, "P", r"R:\dev"));
        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(parsed.sections[0].items.is_empty());
    }
}
