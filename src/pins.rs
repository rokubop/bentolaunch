//! Adding a tile to `bentopick.toml` without flattening the file.
//!
//! `toml_edit` rather than re-serialising through serde: the config is meant to
//! be hand-edited, and round-tripping it through `Config` would silently discard
//! every comment, blank line and key ordering the user put there. A tool that
//! eats your comments is a tool you stop hand-editing.

use std::path::Path;

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item as TomlItem, Table, Value, value};

use crate::config::{Config, Contents};
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

/// One section's bento placement, as edit mode understands it.
///
/// A whole struct rather than one setter per key: edit mode changes these
/// together, and three separate writes would leave the file briefly describing
/// a layout the user never asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Placement {
    /// `None` removes the key, putting the box back to filling whatever the
    /// other boxes left over.
    pub at: Option<String>,
    /// 0 removes the key: as many columns as the box's rectangle takes.
    pub columns: usize,
    /// 0 removes the key: no cap.
    pub max_items: usize,
}

/// Write one section's placement. Returns whether the file changed.
pub fn set_placement(section: &str, placement: Placement) -> bool {
    Config::path().is_some_and(|path| set_placement_in(&path, section, placement))
}

/// Which of the centre block's two lists a write means.
///
/// Two lists because they answer different questions - an app to start, a page
/// to open - and because keeping them apart is what lets the block be read
/// without reading it: the left half is always apps and the right always sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Apps,
    Sites,
}

impl Half {
    /// The key under `[favorites]`.
    pub fn key(self) -> &'static str {
        match self {
            Half::Apps => "apps",
            Half::Sites => "sites",
        }
    }

    /// Its place in the block, left to right. The same index `Contents::shows`
    /// asks about and the same one the store lays the halves out in.
    pub fn index(self) -> usize {
        match self {
            Half::Apps => 0,
            Half::Sites => 1,
        }
    }

    /// Which half a tile belongs in. Apps and folders are things to start;
    /// everything else the shell opens is a place to go.
    pub fn of(kind: crate::model::Kind) -> Half {
        use crate::model::Kind;
        match kind {
            Kind::App | Kind::Folder | Kind::Window => Half::Apps,
            Kind::Link | Kind::Tab | Kind::Action => Half::Sites,
        }
    }
}

/// Add one target to a half of the centre block.
///
/// Returns whether the file changed, which is `false` when it was already
/// there: favoriting something twice is a click, not an error.
pub fn add_favorite(half: Half, target: &str) -> bool {
    Config::path().is_some_and(|path| add_favorite_in(&path, half, target))
}

/// Take one target out of a half of the centre block.
pub fn remove_favorite(half: Half, target: &str) -> bool {
    Config::path().is_some_and(|path| remove_favorite_in(&path, half, target))
}

fn add_favorite_in(path: &Path, half: Half, target: &str) -> bool {
    favorites_in(path, half, |items| {
        if items.iter().any(|entry| target_of(entry) == Some(target)) {
            log_info!("already a favorite, skipping: {target}");
            return false;
        }
        items.push(target);
        true
    })
}

fn remove_favorite_in(path: &Path, half: Half, target: &str) -> bool {
    favorites_in(path, half, |items| {
        let before = items.len();
        items.retain(|entry| target_of(entry) != Some(target));
        items.len() != before
    })
}

/// Take a target out of whichever half is holding it.
///
/// Both halves, not the first that answers: a target hand-written into both
/// would otherwise need two clicks to remove, and the second would look like a
/// click that did nothing.
pub fn forget_favorite(target: &str) -> bool {
    let apps = remove_favorite(Half::Apps, target);
    let sites = remove_favorite(Half::Sites, target);
    apps || sites
}

/// Rewrite one half in the given order. Entries not named keep following, so a
/// stale list never loses a favorite.
pub fn order_favorites(half: Half, targets: &[String]) -> bool {
    Config::path().is_some_and(|path| order_favorites_in(&path, half, targets))
}

fn order_favorites_in(path: &Path, half: Half, targets: &[String]) -> bool {
    favorites_in(path, half, |items| {
        let mut rest: Vec<Value> = items.iter().cloned().collect();
        let mut sorted = Array::new();
        for wanted in targets {
            if let Some(at) = rest
                .iter()
                .position(|entry| target_of(entry) == Some(wanted.as_str()))
            {
                sorted.push_formatted(rest.remove(at));
            }
        }
        for left in rest {
            sorted.push_formatted(left);
        }
        *items = sorted;
        true
    })
}

/// Edit one of the two lists under `[favorites]`, creating the table and the
/// key if the config predates them.
fn favorites_in(path: &Path, half: Half, edit: impl FnOnce(&mut Array) -> bool) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let entry = &mut doc["favorites"][half.key()];
    if entry.is_none() {
        *entry = value(Array::new());
    }
    let Some(items) = entry.as_array_mut() else {
        log_warn!("favorites.{} is not a list; leaving it alone", half.key());
        return false;
    };
    if !edit(items) {
        return false;
    }
    stack(items);
    write(path, &doc)
}

/// Move a section `delta` places through `[[sections]]`.
///
/// Order in the file is the order boxes stack into whatever the claimed sides
/// left over, so this is how that stack is rearranged.
pub fn move_section(section: &str, delta: isize) -> bool {
    Config::path().is_some_and(|path| move_section_in(&path, section, delta))
}

fn move_section_in(path: &Path, section: &str, delta: isize) -> bool {
    if delta == 0 {
        return false;
    }
    let Some(mut doc) = read(path) else { return false };
    let Some(sections) = sections_mut(&mut doc) else { return false };
    let Some(from) = sections.iter().position(|table| title_of(table) == section) else {
        log_warn!("no section titled \"{section}\"; not moved");
        return false;
    };
    let to = from.saturating_add_signed(delta).min(sections.len() - 1);
    if to == from {
        return false;
    }

    // ArrayOfTables has no swap, so the tables are lifted out and put back in
    // the new order. Each table keeps its own decor, so comments ride along.
    //
    // Their positions do not travel with them: toml_edit renders a document by
    // each table's recorded position, so tables pushed in a new order but
    // holding their old positions come back out in the old one. The slots stay
    // where they are and the tables are dealt into them.
    let mut tables: Vec<Table> = sections.iter().cloned().collect();
    let slots: Vec<Option<isize>> = tables.iter().map(|table| table.position()).collect();
    let moved = tables.remove(from);
    tables.insert(to, moved);
    for (table, slot) in tables.iter_mut().zip(&slots) {
        table.set_position(*slot);
    }
    while !sections.is_empty() {
        sections.remove(0);
    }
    for table in tables {
        sections.push(table);
    }

    write(path, &doc) && {
        log_info!("moved section \"{section}\" to position {to}");
        true
    }
}

/// Give one section a whole side of the panel, and move anything else that was
/// claiming that side out of the way.
///
/// One write, not one per section: "make this the left side" is a single
/// intention, and doing it as several writes leaves the file briefly
/// describing a panel with two boxes fighting over the same half.
///
/// The displaced sections lose their placement rather than being sent
/// somewhere chosen for them. They then fill whatever is left over, which is
/// the arrangement anybody asking for a full-height side is picturing.
pub fn claim_side(section: &str, side: &str, share: f32) -> bool {
    Config::path().is_some_and(|path| claim_side_in(&path, section, side, share))
}

fn claim_side_in(path: &Path, section: &str, side: &str, share: f32) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let Some(sections) = sections_mut(&mut doc) else { return false };
    if !sections.iter().any(|table| title_of(table) == section) {
        log_warn!("no section titled \"{section}\"; side not claimed");
        return false;
    }

    let spelled = if share > 0.0 {
        format!("{side}@{:.0}", share * 100.0)
    } else {
        side.to_owned()
    };

    let mut displaced = Vec::new();
    for table in sections.iter_mut() {
        let title = title_of(table);
        if title == section {
            table["at"] = value(spelled.as_str());
            continue;
        }
        // Anything whose path starts with the same cut is on the side being
        // claimed, however deeply it was nested inside it.
        let theirs = table.get("at").and_then(|at| at.as_str()).unwrap_or("");
        let first = theirs.split(['/', '@']).next().unwrap_or("");
        if !first.is_empty() && first.eq_ignore_ascii_case(side) {
            table.remove("at");
            displaced.push(title);
        }
    }

    if !write(path, &doc) {
        return false;
    }
    if displaced.is_empty() {
        log_info!("section \"{section}\" now holds the {side}");
    } else {
        log_info!(
            "section \"{section}\" now holds the {side}; moved off it: {}",
            displaced.join(", ")
        );
    }
    true
}

fn set_placement_in(path: &Path, section: &str, placement: Placement) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let Some(sections) = sections_mut(&mut doc) else { return false };
    let Some(index) = sections.iter().position(|table| title_of(table) == section) else {
        log_warn!("no section titled \"{section}\"; layout not saved");
        return false;
    };
    let Some(table) = sections.get_mut(index) else {
        return false;
    };

    // A default is written as an absent key, not as a zero. The config is meant
    // to be read by a person, and `columns = 0` says less than nothing there.
    match &placement.at {
        Some(at) => table["at"] = value(at.as_str()),
        None => {
            table.remove("at");
        }
    }
    for (key, number) in [("columns", placement.columns), ("max_items", placement.max_items)] {
        if number == 0 {
            table.remove(key);
        } else {
            table[key] = value(number as i64);
        }
    }

    write(path, &doc) && {
        log_info!("saved the layout of section \"{section}\"");
        true
    }
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
        .position(|table| title_of(table) == section && has_source(table, "taskbar"))
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

/// `source` is a bare string, or a list when the section merges several. Both
/// forms answer the same question: does this section take entries of this kind?
fn has_source(table: &Table, source: &str) -> bool {
    let Some(item) = table.get("source") else {
        return false;
    };
    if item.as_str() == Some(source) {
        return true;
    }
    item.as_array()
        .is_some_and(|list| list.iter().any(|s| s.as_str() == Some(source)))
}

fn first_manual(sections: &ArrayOfTables) -> Option<usize> {
    sections.iter().position(|table| has_source(table, "manual"))
}

fn manual_named(sections: &ArrayOfTables, title: &str) -> Option<usize> {
    sections
        .iter()
        .position(|table| title_of(table) == title && has_source(table, "manual"))
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

/// One settings square's write.
///
/// A value rather than a setter per key, for the same reason `Placement` is
/// one: the square knows what it means, this module knows how to say it in
/// TOML, and neither has to learn the other's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// Width, height and label strip together. They are one size, and writing
    /// them one at a time would leave the file briefly describing a tile
    /// nobody asked for - which the watcher would pick up and lay out.
    Tiles { width: f32, height: f32, label: f32 },
    ShowDetail(bool),
    MaxColumns(usize),
    Browser(bool),
    /// The centre block's shape, in tiles a half. 0 turns it off: how much
    /// centre you want and whether you want any are the same question, so they
    /// are one square. Both numbers together, for the same reason `Tiles` is
    /// one change - a file that briefly says 3 x 1 is a layout the watcher
    /// would draw.
    CenterSize { columns: usize, rows: usize },
    /// Which lists the block holds, and whether they are kept apart.
    CenterContents(Contents),
}

/// Apply one settings change. Returns whether the file changed.
pub fn set(change: Change) -> bool {
    Config::path().is_some_and(|path| set_in(&path, change))
}

fn set_in(path: &Path, change: Change) -> bool {
    let Some(mut doc) = read(path) else { return false };
    match change {
        Change::Tiles { width, height, label } => {
            set_key(&mut doc, "grid", "tile_width", f64::from(width).into());
            set_key(&mut doc, "grid", "tile_height", f64::from(height).into());
            set_key(&mut doc, "grid", "label_height", f64::from(label).into());
        }
        Change::ShowDetail(on) => set_key(&mut doc, "grid", "show_detail", on.into()),
        Change::MaxColumns(n) => set_key(&mut doc, "grid", "max_columns", (n as i64).into()),
        Change::Browser(on) => set_key(&mut doc, "browser", "enabled", on.into()),
        Change::CenterSize { columns, rows } => {
            set_key(&mut doc, "favorites", "columns", (columns as i64).into());
            set_key(&mut doc, "favorites", "rows", (rows as i64).into());
        }
        Change::CenterContents(contents) => {
            set_key(&mut doc, "favorites", "contents", contents.key().into());
            // The key `contents` replaced. Left in, it would keep answering
            // the question this square just answered - see `Config::validated`.
            drop_key(&mut doc, "favorites", "split");
        }
    }
    write(path, &doc)
}

/// Replace one key's value, keeping whatever was written around it.
///
/// `doc[table][key] = value(v)` is the obvious spelling and it throws the old
/// value's decoration away with it - including the comment trailing the line.
/// A settings square eating the note the user wrote beside a setting is the
/// same failure as flattening the file, just one line at a time.
///
/// Missing tables and keys are created, so this works on a config that predates
/// the key entirely.
fn set_key(doc: &mut DocumentMut, table: &str, key: &str, v: Value) {
    let entry = &mut doc[table][key];
    let mut v = v;
    if let Some(old) = entry.as_value() {
        *v.decor_mut() = old.decor().clone();
    }
    *entry = TomlItem::Value(v);
}

/// Take a key out, leaving the table it was in even if that empties it.
///
/// Only for keys this app has replaced with another. A settings square must
/// never quietly delete something the user wrote.
fn drop_key(doc: &mut DocumentMut, table: &str, key: &str) {
    if let Some(table) = doc.get_mut(table).and_then(TomlItem::as_table_like_mut) {
        table.remove(key);
    }
}

/// Through `toml_edit` like every other write, so comments survive.
/// Turn the bridge on from the tray. Pairing needs a listening socket, so the
/// user should not have to hand-edit the config to reach the pairing flow.
pub fn set_browser_enabled(enabled: bool) -> bool {
    let Some(path) = Config::path() else { return false };
    let Some(mut doc) = read(&path) else { return false };
    set_key(&mut doc, "browser", "enabled", enabled.into());
    write(&path, &doc)
}

/// Empty out the pre-pairing keys once their contents have been carried into
/// the peer store. `allow` was the pairing list and `token` was a secret in a
/// world-readable file; neither should linger in the config saying something
/// that is no longer true.
pub fn clear_browser_legacy() -> bool {
    let Some(path) = Config::path() else { return false };
    let Some(mut doc) = read(&path) else { return false };
    doc["browser"]["allow"] = value(Array::new());
    doc["browser"]["token"] = value("");
    write(&path, &doc)
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
        let path = std::env::temp_dir().join(format!("bentopick-pins-test-{name}.toml"));
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

    fn three_sections(name: &str) -> PathBuf {
        let path = scratch(name);
        std::fs::write(
            &path,
            concat!(
                "hotkey = \"alt+`\"

",
                "[[sections]]
title = \"Browsing\"
source = \"tabs\"

",
                "[[sections]]
title = \"Active\"
source = \"windows\"

",
                "[[sections]]
title = \"Launch\"
source = \"taskbar\"
",
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn a_settings_square_writes_its_keys_and_leaves_the_rest_of_the_file_alone() {
        let path = scratch("settings");
        std::fs::write(
            &path,
            concat!(
                "# mine, keep it
",
                "hotkey = \"alt+`\"

",
                "[grid]
",
                "tile_width = 140.0
",
                "tile_height = 100.0
",
                "label_height = 24.0
",
                "show_detail = false  # the second line costs a row
",
            ),
        )
        .unwrap();

        assert!(set_in(&path, Change::Tiles { width: 180.0, height: 128.0, label: 28.0 }));
        assert!(set_in(&path, Change::ShowDetail(true)));
        assert!(set_in(&path, Change::MaxColumns(0)));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine, keep it"), "comment lost from {text}");
        assert!(text.contains("the second line costs a row"), "comment lost from {text}");

        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.grid.tile_width, 180.0);
        assert_eq!(parsed.grid.tile_height, 128.0);
        assert_eq!(parsed.grid.label_height, 28.0);
        assert!(parsed.grid.show_detail);
        assert_eq!(parsed.grid.max_columns, 0);
        // Untouched keys are still the user's.
        assert_eq!(parsed.hotkey, "alt+`");
    }

    // --- layout ---

    #[test]
    fn a_placement_is_written_as_three_keys() {
        let path = three_sections("placement");
        let placement = Placement { at: Some("left".into()), columns: 3, max_items: 12 };
        assert!(set_placement_in(&path, "Browsing", placement));

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        let browsing = parsed.sections.iter().find(|s| s.title == "Browsing").unwrap();
        assert_eq!(browsing.at.as_deref(), Some("left"));
        assert_eq!(browsing.columns, 3);
        assert_eq!(browsing.max_items, 12);
    }

    #[test]
    fn a_default_is_written_as_an_absent_key() {
        // Not `columns = 0`. A config that is meant to be hand-edited should
        // not accumulate keys that say "unset".
        let path = three_sections("defaults");
        set_placement_in(&path, "Active", Placement { at: Some("right/top".into()), columns: 4, max_items: 9 });
        assert!(set_placement_in(&path, "Active", Placement::default()));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("columns"), "{text}");
        assert!(!text.contains("max_items"), "{text}");
        assert!(!text.contains("at ="), "{text}");

        let parsed: Config = toml::from_str(&text).unwrap();
        let active = parsed.sections.iter().find(|s| s.title == "Active").unwrap();
        assert_eq!(active.at, None);
        assert_eq!(active.columns, 0);
        assert_eq!(active.max_items, 0);
    }

    #[test]
    fn a_layout_write_leaves_the_rest_of_the_file_alone() {
        let path = scratch("layout-comments");
        let original = concat!(
            "# mine\n",
            "hotkey = \"ctrl+alt+q\"\n\n",
            "[[sections]]\n",
            "title = \"Places\"\n",
            "source = \"manual\"\n",
            r"items = ['R:\dev']", "\n",
        );
        std::fs::write(&path, original).unwrap();

        set_placement_in(&path, "Places", Placement { at: Some("bottom".into()), columns: 5, max_items: 0 });
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains("ctrl+alt+q"), "{text}");
        assert!(text.contains(r"R:\dev"), "{text}");
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.sections[0].columns, 5);
        assert_eq!(parsed.sections[0].items.len(), 1);
    }

    #[test]
    fn an_unknown_section_writes_nothing() {
        let path = three_sections("unknown");
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(!set_placement_in(&path, "Nope", Placement::default()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn hand_written_comments_and_keys_survive() {
        let path = scratch("comments");
        let original = "# my bentopick config\nhotkey = \"ctrl+alt+q\"  # trailing note\n\n\
             [[sections]]\ntitle = \"Windows\"\nsource = \"windows\"\n\n\
             # things I open a lot\n[[sections]]\ntitle = \"Places\"\nsource = \"manual\"\nitems = []\n";
        std::fs::write(&path, original).unwrap();

        add_to(&path, None, "ms-settings:display").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(text.contains("# my bentopick config"));
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
            .find(|s| s.source.contains(crate::config::Source::Manual))
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

    /// A merged section is one header over two lists. Pins go in `items` and
    /// taskbar order goes in `order`, and both have to find it — this file
    /// reads the raw TOML, where `source` is a list rather than a string.
    #[test]
    fn a_merged_section_takes_both_a_pin_and_a_taskbar_order() {
        let path = scratch("merged");
        std::fs::write(
            &path,
            "[[sections]]\ntitle = \"Launch\"\nsource = [\"taskbar\", \"manual\"]\n\
             items = [\"R:\\\\dev\"]\n",
        )
        .unwrap();

        assert_eq!(add_to(&path, None, r"D:\x").as_deref(), Some("Launch"));
        assert_eq!(manual(&path, "Launch"), [r"R:\dev", r"D:\x"]);

        assert!(set_order_in(&path, "Launch", &["Firefox".into(), "Steam".into()]));
        let parsed: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let launch = parsed.sections.iter().find(|s| s.title == "Launch").unwrap();
        assert_eq!(launch.order, ["Firefox", "Steam"]);
        // The pin list survived the order write.
        assert_eq!(launch.items.len(), 2);
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

    // --- the centre block ---

    /// A config with a section but nothing said about `[favorites]`, which is
    /// every config written before the centre existed.
    fn no_centre(name: &str) -> PathBuf {
        let path = scratch(name);
        std::fs::write(
            &path,
            "hotkey = \"alt+`\"\n\n[[sections]]\ntitle = \"Launch\"\nsource = \"taskbar\"\n",
        )
        .unwrap();
        path
    }

    fn favorites(path: &PathBuf) -> Config {
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_favorite_creates_the_table_a_config_never_had() {
        let path = no_centre("fav-new");
        assert!(add_favorite_in(&path, Half::Apps, r"C:\Windows\notepad.exe"));

        let parsed = favorites(&path);
        assert_eq!(parsed.favorites.apps.len(), 1);
        assert_eq!(parsed.favorites.apps[0].target(), r"C:\Windows\notepad.exe");
        assert!(parsed.favorites.sites.is_empty());
        // And nothing else moved.
        assert_eq!(parsed.sections.len(), 1);
        assert_eq!(parsed.sections[0].title, "Launch");
    }

    #[test]
    fn the_two_halves_are_written_apart() {
        let path = no_centre("fav-halves");
        assert!(add_favorite_in(&path, Half::Apps, r"C:\Windows\notepad.exe"));
        assert!(add_favorite_in(&path, Half::Sites, "https://example.com"));

        let parsed = favorites(&path);
        assert_eq!(parsed.favorites.apps.len(), 1);
        assert_eq!(parsed.favorites.sites.len(), 1);
        assert_eq!(parsed.favorites.sites[0].target(), "https://example.com");
    }

    #[test]
    fn favoriting_the_same_thing_twice_is_a_no_op() {
        let path = no_centre("fav-twice");
        assert!(add_favorite_in(&path, Half::Apps, "notepad"));
        assert!(!add_favorite_in(&path, Half::Apps, "notepad"));
        assert_eq!(favorites(&path).favorites.apps.len(), 1);
    }

    #[test]
    fn removing_takes_out_one_favorite_and_leaves_the_rest() {
        let path = no_centre("fav-remove");
        for target in ["a", "b", "c"] {
            assert!(add_favorite_in(&path, Half::Apps, target));
        }
        assert!(remove_favorite_in(&path, Half::Apps, "b"));

        let kept: Vec<String> = favorites(&path)
            .favorites
            .apps
            .iter()
            .map(|entry| entry.target().to_owned())
            .collect();
        assert_eq!(kept, vec!["a".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn removing_something_that_is_not_a_favorite_writes_nothing() {
        let path = no_centre("fav-absent");
        assert!(!remove_favorite_in(&path, Half::Apps, "nothing"));
    }

    #[test]
    fn dragging_rewrites_one_half_in_the_order_it_was_dropped_in() {
        let path = no_centre("fav-order");
        for target in ["a", "b", "c"] {
            assert!(add_favorite_in(&path, Half::Apps, target));
        }
        let wanted = ["c".to_owned(), "a".to_owned(), "b".to_owned()];
        assert!(order_favorites_in(&path, Half::Apps, &wanted));

        let got: Vec<String> = favorites(&path)
            .favorites
            .apps
            .iter()
            .map(|entry| entry.target().to_owned())
            .collect();
        assert_eq!(got, wanted);
    }

    #[test]
    fn an_order_that_names_too_few_keeps_the_rest_rather_than_dropping_them() {
        // A stale list must never lose a favorite: the same rule the manual
        // sections follow, and for the same reason.
        let path = no_centre("fav-partial");
        for target in ["a", "b", "c"] {
            assert!(add_favorite_in(&path, Half::Apps, target));
        }
        assert!(order_favorites_in(&path, Half::Apps, &["c".to_owned()]));

        let got: Vec<String> = favorites(&path)
            .favorites
            .apps
            .iter()
            .map(|entry| entry.target().to_owned())
            .collect();
        assert_eq!(got, vec!["c".to_owned(), "a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn a_hand_written_title_survives_being_reordered() {
        let path = scratch("fav-titled");
        std::fs::write(
            &path,
            concat!(
                "[[sections]]\ntitle = \"Launch\"\nsource = \"taskbar\"\n\n",
                "[favorites]\n",
                "sites = [{ title = \"Docs\", target = \"https://docs.example\" }, \"https://b\"]\n",
            ),
        )
        .unwrap();

        assert!(order_favorites_in(
            &path,
            Half::Sites,
            &["https://b".to_owned(), "https://docs.example".to_owned()],
        ));
        let sites = favorites(&path).favorites.sites;
        assert_eq!(sites[1].title(), Some("Docs"));
    }

    #[test]
    fn a_centre_square_writes_its_key_and_leaves_the_comment_beside_it() {
        let path = scratch("fav-setting");
        std::fs::write(
            &path,
            concat!(
                "[[sections]]\ntitle = \"Launch\"\nsource = \"taskbar\"\n\n",
                "[favorites]\nrows = 2 # how tall I like it\nsplit = true\n",
            ),
        )
        .unwrap();

        assert!(set_in(&path, Change::CenterSize { columns: 0, rows: 0 }));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# how tall I like it"), "comment eaten: {text}");
        assert_eq!(favorites(&path).favorites.rows, 0);
        // Untouched by a size write: one square, one question.
        assert_eq!(favorites(&path).favorites.split, Some(true));
    }

    #[test]
    fn writing_what_the_centre_holds_takes_the_key_it_replaced_out() {
        // Both keys left in the file would be two answers to one question, and
        // `Config::validated` has to pick the older one - so the square that
        // answers it clears the older one on its way past.
        let path = scratch("fav-contents");
        std::fs::write(
            &path,
            concat!(
                "[[sections]]\ntitle = \"Launch\"\nsource = \"taskbar\"\n\n",
                "[favorites]\nrows = 2\nsplit = true\n",
            ),
        )
        .unwrap();

        assert!(set_in(&path, Change::CenterContents(Contents::Apps)));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("split"), "the old key is still there: {text}");
        let read = favorites(&path).validated();
        assert_eq!(read.favorites.contents, Contents::Apps);
    }
}
