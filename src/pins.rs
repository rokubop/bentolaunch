//! Adding a tile to `bentolaunch.toml` without flattening the file.
//!
//! `toml_edit` rather than re-serialising through serde: the config is meant to
//! be hand-edited, and round-tripping it through `Config` would silently discard
//! every comment, blank line and key ordering the user put there. A tool that
//! eats your comments is a tool you stop hand-editing.

use std::path::Path;

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item as TomlItem, Table, Value, value};

use crate::config::{Config, Contents, Center};
use crate::ui::grid::Lane;
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
    /// Which band across the panel. `None` removes the key, putting the box
    /// back to the default lane.
    pub side: Option<String>,
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
    /// The key under `[center]`.
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
pub fn add_to_center(half: Half, target: &str) -> bool {
    Config::path().is_some_and(|path| add_to_center_in(&path, half, target))
}

/// Take one target out of a half of the centre block.
pub fn remove_from_center(half: Half, target: &str) -> bool {
    Config::path().is_some_and(|path| remove_from_center_in(&path, half, target))
}

fn add_to_center_in(path: &Path, half: Half, target: &str) -> bool {
    edit_center(path, half, |items| {
        if items.iter().any(|entry| target_of(entry) == Some(target)) {
            log_info!("already a favorite, skipping: {target}");
            return false;
        }
        items.push(target);
        true
    })
}

fn remove_from_center_in(path: &Path, half: Half, target: &str) -> bool {
    edit_center(path, half, |items| {
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
pub fn forget_in_center(target: &str) -> bool {
    let apps = remove_from_center(Half::Apps, target);
    let sites = remove_from_center(Half::Sites, target);
    apps || sites
}

/// Rewrite one half in the given order. Entries not named keep following, so a
/// stale list never loses a favorite.
pub fn order_center(half: Half, targets: &[String]) -> bool {
    Config::path().is_some_and(|path| order_center_in(&path, half, targets))
}

fn order_center_in(path: &Path, half: Half, targets: &[String]) -> bool {
    edit_center(path, half, |items| {
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

/// Edit one of the two lists under `[center]`, creating the table and the
/// key if the config predates them.
fn edit_center(path: &Path, half: Half, edit: impl FnOnce(&mut Array) -> bool) -> bool {
    let Some(mut doc) = read(path) else { return false };
    {
        let entry = &mut doc["center"][half.key()];
        if entry.is_none() {
            *entry = value(Array::new());
        }
        let Some(items) = entry.as_array_mut() else {
            log_warn!("center.{} is not a list; leaving it alone", half.key());
            return false;
        };
        if !edit(items) {
            return false;
        }
        stack(items);
    }
    // In the same write as the list it is sizing for. Two writes would leave
    // the file briefly holding more center than the block draws, and the
    // watcher lays out whatever it finds.
    grow_to_fit(&mut doc);
    write(path, &doc)
}

/// Grow the block to hold what the file says it holds.
///
/// Only up. Removing a favorite leaves the shape where it is, so the square you
/// learned the position of is still that square - a block that shrank as it
/// emptied would be moving targets. Coming back down is an edit-mode click.
///
/// A block that is off is a block of no slots, so this is also what the first
/// favorite turns it on with.
fn grow_to_fit(doc: &mut DocumentMut) {
    let count = |key: &str| {
        doc.get("center")
            .and_then(|f| f.get(key))
            .and_then(TomlItem::as_array)
            .map_or(0, Array::len)
    };
    let number = |key: &str| {
        doc.get("center")
            .and_then(|f| f.get(key))
            .and_then(TomlItem::as_integer)
            .and_then(|n| usize::try_from(n).ok())
    };

    let (apps, sites) = (count("apps"), count("sites"));
    // Which lists share a half decides how many slots a half has to hold. Read
    // off the document rather than off a parsed `Config`: this runs mid-write,
    // and the file on disk is a write behind.
    let contents = doc
        .get("center")
        .and_then(|f| f.get("contents"))
        .and_then(TomlItem::as_str)
        .unwrap_or(Contents::default().key());
    let held = match contents {
        "one" => apps + sites,
        "apps" => apps,
        "sites" => sites,
        _ => apps.max(sites),
    };

    // An absent key is its default, and the default is off. Reading it as zero
    // is what makes the first favorite on a fresh config turn the block on.
    let now = number("rows").unwrap_or(0) * number("columns").unwrap_or(0);
    if held == 0 || now >= held {
        return;
    }
    let (columns, rows) = Center::shape_for(held);
    set_key(doc, "center", "columns", (columns as i64).into());
    set_key(doc, "center", "rows", (rows as i64).into());
    log_info!("centre block grown to {columns} x {rows} a half for {held} favorite(s)");
}

/// Move a section `delta` places down its own lane.
///
/// Order in the file is order down a lane, so this is how that order is
/// changed. Past the boxes in other lanes, not through them: swapping with a
/// box in the other column is a write that changes nothing on screen.
pub fn move_section(section: &str, delta: isize) -> bool {
    Config::path().is_some_and(|path| move_section_in(&path, section, delta))
}

/// Which band across the panel a section's table asks for. A section that says
/// nothing takes the default.
fn lane_of(table: &Table) -> Lane {
    table
        .get("side")
        .and_then(|v| v.as_str())
        .and_then(Lane::parse)
        .unwrap_or_default()
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
    // Its neighbours down its own lane. `from` is one of them, so this always
    // finds it.
    let lanes: Vec<Lane> = sections.iter().map(lane_of).collect();
    let mine = lanes[from];
    let lane: Vec<usize> = (0..lanes.len()).filter(|&index| lanes[index] == mine).collect();
    let Some(place) = lane.iter().position(|&index| index == from) else {
        return false;
    };
    let Some(to) = place
        .checked_add_signed(delta)
        .and_then(|next| lane.get(next))
        .copied()
    else {
        return false;
    };
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
    match &placement.side {
        Some(side) => table["side"] = value(side.as_str()),
        None => {
            table.remove("side");
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

/// Which box `add` would put something in, without putting it there.
///
/// So a menu can say where a tile is going. "Add to Launch" is worth a read
/// where "Pin" is not: the useful half of the label is the destination, and
/// this is the only thing that knows it.
pub fn destination() -> Option<String> {
    let path = Config::path()?;
    let mut doc = read(&path)?;
    let sections = sections_mut(&mut doc)?;
    match first_manual(sections) {
        Some(index) => sections.get(index).map(title_of),
        // Nowhere to put one yet, so `add` would make this.
        None => Some(FALLBACK_TITLE.to_string()),
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
            set_key(&mut doc, "center", "columns", (columns as i64).into());
            set_key(&mut doc, "center", "rows", (rows as i64).into());
        }
        Change::CenterContents(contents) => {
            set_key(&mut doc, "center", "contents", contents.key().into());
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


/// Through `toml_edit` like every other write, so comments survive.
/// Turn the bridge on from the tray. Pairing needs a listening socket, so the
/// user should not have to hand-edit the config to reach the pairing flow.
pub fn set_browser_enabled(enabled: bool) -> bool {
    let Some(path) = Config::path() else { return false };
    let Some(mut doc) = read(&path) else { return false };
    set_key(&mut doc, "browser", "enabled", enabled.into());
    write(&path, &doc)
}


/// Put the layout back to stock, keeping everything the user put there by hand.
///
/// The one write that cannot be a key at a time. A box that was deleted and a
/// box that was added are both layout, and neither is reachable by setting a
/// value, so the section list is rebuilt from the defaults outright.
///
/// What is not layout is not touched: the hotkey, the theme, the browser
/// switch, each section's `items` and `order`, and the block's two lists all
/// come through, comments and all. Paired browsers were never in here - they
/// live in `peers.json` - so a reset cannot unpair anything.
///
/// The old file is copied next to the log first. A reset that took something
/// wanted is then one file copy from undone, which is the only reason it is
/// safe to offer as a single click.
pub fn reset_layout() -> bool {
    Config::path().is_some_and(|path| reset_layout_in(&path))
}

/// What a section carries that belongs to the user rather than to the layout.
/// `items` is what they added by hand; `order` is the order they dragged the
/// taskbar pins into.
const KEPT: [&str; 2] = ["items", "order"];

/// The block's shape, which is layout. Its two lists are not, and are absent
/// from here on purpose.
const FAVORITE_KEYS: [&str; 3] = ["rows", "columns", "contents"];

fn reset_layout_in(path: &Path) -> bool {
    let Some(mut doc) = read(path) else { return false };
    let Some(stock) = stock_doc() else {
        log_warn!("could not build the default config; layout not reset");
        return false;
    };
    let Some(fresh) = stock.get("sections").and_then(TomlItem::as_array_of_tables) else {
        return false;
    };
    back_up(path);

    let was: Vec<Table> = doc
        .get("sections")
        .and_then(TomlItem::as_array_of_tables)
        .map(|sections| sections.iter().cloned().collect())
        .unwrap_or_default();

    let mut rebuilt = ArrayOfTables::new();
    for table in fresh.iter() {
        let mut table = table.clone();
        if let Some(before) = was.iter().find(|old| title_of(old) == title_of(&table)) {
            for key in KEPT {
                if let Some(item) = before.get(key) {
                    table[key] = item.clone();
                }
            }
        }
        tidy(&mut table);
        rebuilt.push(table);
    }

    // A box the user wrote themselves is not the layout's to delete. It comes
    // through whole, after the stock ones, rather than being reset into a lane
    // it never asked for.
    let stock_titles: Vec<String> = fresh.iter().map(title_of).collect();
    for table in was {
        if !stock_titles.contains(&title_of(&table)) {
            rebuilt.push(table);
        }
    }

    // toml_edit renders tables in recorded-position order and a table with no
    // position inherits the last one seen. Clearing all of them is what keeps
    // the rebuilt list together and in list order: positions carried over from
    // the old file would deal the new sections back out into the old slots,
    // which is a different order once the count has changed.
    for table in rebuilt.iter_mut() {
        table.set_position(None);
    }
    doc["sections"] = TomlItem::ArrayOfTables(rebuilt);

    // Every key of `[grid]`, one at a time rather than replacing the table, so
    // a note written beside a setting survives being reset.
    if let Some(grid) = stock.get("grid").and_then(TomlItem::as_table) {
        for (key, item) in grid.iter() {
            if let Some(v) = item.as_value() {
                set_key(&mut doc, "grid", key, v.clone());
            }
        }
    }
    if let Some(center) = stock.get("center").and_then(TomlItem::as_table) {
        for key in FAVORITE_KEYS {
            if let Some(v) = center.get(key).and_then(TomlItem::as_value) {
                set_key(&mut doc, "center", key, v.clone());
            }
        }
    }
    // Stock is a block that is off, and a reset must not be what hides a
    // favorite. Sized back up to what the file still holds, which is the same
    // rule adding one follows - so the stock shape is "as big as it needs".
    grow_to_fit(&mut doc);

    write(path, &doc) && {
        log_info!("layout reset to defaults");
        true
    }
}

/// Drop the keys serde wrote out at their empty value.
///
/// A default is an absent key, not a zero - the same rule `set_placement_in`
/// follows. This file is meant to be hand-edited, and `columns = 0` sitting
/// under every box says less than nothing to whoever opens it.
///
/// `title` is left even when it is empty: the two untitled boxes are untitled
/// on purpose, and the key is how that is said.
fn tidy(table: &mut Table) {
    let empty: Vec<String> = table
        .iter()
        .filter(|(key, item)| *key != "title" && is_empty(item))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in empty {
        table.remove(&key);
    }
}

fn is_empty(item: &TomlItem) -> bool {
    match item.as_value() {
        Some(Value::Array(list)) => list.is_empty(),
        Some(Value::Integer(n)) => *n.value() == 0,
        _ => false,
    }
}

/// The default config as a document, to copy stock values out of.
fn stock_doc() -> Option<DocumentMut> {
    toml::to_string_pretty(&Config::default()).ok()?.parse::<DocumentMut>().ok()
}

/// Copy the config beside the log before a reset overwrites it.
///
/// In the cache directory rather than beside the exe: that directory is already
/// this app's, and a portable build dropped in `Program Files` cannot write to
/// its own folder anyway. One file, overwritten each time - a reset is an undo
/// of the last one, not a history.
///
/// A failed copy does not stop the reset. It is a courtesy, and refusing to
/// reset because the backup could not be written would be the tool arguing with
/// a click the user already made.
fn back_up(path: &Path) {
    let Some(dir) = backup_dir() else { return };
    let backup = dir.join("bentolaunch.toml.bak");
    match std::fs::copy(path, &backup) {
        Ok(_) => log_info!("previous config saved to {}", backup.display()),
        Err(e) => log_warn!("could not back up the config: {e}"),
    }
}

fn backup_dir() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = test_backup_dir().get() {
        return Some(dir.clone());
    }
    crate::log::cache_dir()
}

/// A reset test writes a real backup, and the real place for it is a real
/// install's directory. Point it somewhere disposable so a test run never
/// touches the config someone is using.
#[cfg(test)]
fn test_backup_dir() -> &'static std::sync::OnceLock<std::path::PathBuf> {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    &DIR
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
        let path = std::env::temp_dir().join(format!("bentolaunch-pins-test-{name}.toml"));
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
        let placement = Placement { side: Some("left".into()), columns: 3, max_items: 12 };
        assert!(set_placement_in(&path, "Browsing", placement));

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        let browsing = parsed.sections.iter().find(|s| s.title == "Browsing").unwrap();
        assert_eq!(browsing.side.as_deref(), Some("left"));
        assert_eq!(browsing.columns, 3);
        assert_eq!(browsing.max_items, 12);
    }

    #[test]
    fn a_default_is_written_as_an_absent_key() {
        // Not `columns = 0`. A config that is meant to be hand-edited should
        // not accumulate keys that say "unset".
        let path = three_sections("defaults");
        set_placement_in(&path, "Active", Placement { side: Some("right".into()), columns: 4, max_items: 9 });
        assert!(set_placement_in(&path, "Active", Placement::default()));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("columns"), "{text}");
        assert!(!text.contains("max_items"), "{text}");
        assert!(!text.contains("side ="), "{text}");

        let parsed: Config = toml::from_str(&text).unwrap();
        let active = parsed.sections.iter().find(|s| s.title == "Active").unwrap();
        assert_eq!(active.side, None);
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

        set_placement_in(&path, "Places", Placement { side: Some("full".into()), columns: 5, max_items: 0 });
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
        let original = "# my bentolaunch config\nhotkey = \"ctrl+alt+q\"  # trailing note\n\n\
             [[sections]]\ntitle = \"Windows\"\nsource = \"windows\"\n\n\
             # things I open a lot\n[[sections]]\ntitle = \"Places\"\nsource = \"manual\"\nitems = []\n";
        std::fs::write(&path, original).unwrap();

        add_to(&path, None, "ms-settings:display").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        assert!(text.contains("# my bentolaunch config"));
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

    /// A config with a section but nothing said about `[center]`, which is
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

    fn center(path: &PathBuf) -> Config {
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_favorite_creates_the_table_a_config_never_had() {
        let path = no_centre("fav-new");
        assert!(add_to_center_in(&path, Half::Apps, r"C:\Windows\notepad.exe"));

        let parsed = center(&path);
        assert_eq!(parsed.center.apps.len(), 1);
        assert_eq!(parsed.center.apps[0].target(), r"C:\Windows\notepad.exe");
        assert!(parsed.center.sites.is_empty());
        // And nothing else moved.
        assert_eq!(parsed.sections.len(), 1);
        assert_eq!(parsed.sections[0].title, "Launch");
    }

    #[test]
    fn the_two_halves_are_written_apart() {
        let path = no_centre("fav-halves");
        assert!(add_to_center_in(&path, Half::Apps, r"C:\Windows\notepad.exe"));
        assert!(add_to_center_in(&path, Half::Sites, "https://example.com"));

        let parsed = center(&path);
        assert_eq!(parsed.center.apps.len(), 1);
        assert_eq!(parsed.center.sites.len(), 1);
        assert_eq!(parsed.center.sites[0].target(), "https://example.com");
    }

    #[test]
    fn favoriting_the_same_thing_twice_is_a_no_op() {
        let path = no_centre("fav-twice");
        assert!(add_to_center_in(&path, Half::Apps, "notepad"));
        assert!(!add_to_center_in(&path, Half::Apps, "notepad"));
        assert_eq!(center(&path).center.apps.len(), 1);
    }

    #[test]
    fn removing_takes_out_one_favorite_and_leaves_the_rest() {
        let path = no_centre("fav-remove");
        for target in ["a", "b", "c"] {
            assert!(add_to_center_in(&path, Half::Apps, target));
        }
        assert!(remove_from_center_in(&path, Half::Apps, "b"));

        let kept: Vec<String> = center(&path)
            .center
            .apps
            .iter()
            .map(|entry| entry.target().to_owned())
            .collect();
        assert_eq!(kept, vec!["a".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn removing_something_that_is_not_a_favorite_writes_nothing() {
        let path = no_centre("fav-absent");
        assert!(!remove_from_center_in(&path, Half::Apps, "nothing"));
    }

    #[test]
    fn dragging_rewrites_one_half_in_the_order_it_was_dropped_in() {
        let path = no_centre("fav-order");
        for target in ["a", "b", "c"] {
            assert!(add_to_center_in(&path, Half::Apps, target));
        }
        let wanted = ["c".to_owned(), "a".to_owned(), "b".to_owned()];
        assert!(order_center_in(&path, Half::Apps, &wanted));

        let got: Vec<String> = center(&path)
            .center
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
            assert!(add_to_center_in(&path, Half::Apps, target));
        }
        assert!(order_center_in(&path, Half::Apps, &["c".to_owned()]));

        let got: Vec<String> = center(&path)
            .center
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
                "[center]\n",
                "sites = [{ title = \"Docs\", target = \"https://docs.example\" }, \"https://b\"]\n",
            ),
        )
        .unwrap();

        assert!(order_center_in(
            &path,
            Half::Sites,
            &["https://b".to_owned(), "https://docs.example".to_owned()],
        ));
        let sites = center(&path).center.sites;
        assert_eq!(sites[1].title(), Some("Docs"));
    }

    #[test]
    fn a_centre_square_writes_its_key_and_leaves_the_comment_beside_it() {
        let path = scratch("fav-setting");
        std::fs::write(
            &path,
            concat!(
                "[[sections]]\ntitle = \"Launch\"\nsource = \"taskbar\"\n\n",
                "[center]\nrows = 2 # how tall I like it\n",
            ),
        )
        .unwrap();

        assert!(set_in(&path, Change::CenterSize { columns: 0, rows: 0 }));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# how tall I like it"), "comment eaten: {text}");
        assert_eq!(center(&path).center.rows, 0);
    }


    // --- the block growing to fit ---

    fn block(path: &Path) -> (usize, usize) {
        let f = reread(path).center;
        (f.columns, f.rows)
    }

    #[test]
    fn the_first_favorite_turns_the_block_on() {
        let path = scratch("grow-first");
        std::fs::write(&path, "hotkey = 'alt+`'
").unwrap();
        assert!(!reread(&path).center.on(), "the block should ship off");

        assert!(add_to_center_in(&path, Half::Apps, "notepad.exe"));
        assert_eq!(block(&path), Center::shape_for(1));
        assert!(reread(&path).center.on());
    }

    #[test]
    fn the_block_grows_when_a_favorite_would_not_be_drawn() {
        let path = scratch("grow-past");
        std::fs::write(&path, "[center]
rows = 1
columns = 2
").unwrap();

        // Two fit the 2 x 1 it is on, so nothing moves for either.
        for app in ["a.exe", "b.exe"] {
            assert!(add_to_center_in(&path, Half::Apps, app));
        }
        assert_eq!(block(&path), (2, 1), "the block grew before it had to");

        // The third would be written and never drawn, which is the click with
        // no visible result this exists to stop.
        assert!(add_to_center_in(&path, Half::Apps, "c.exe"));
        assert_eq!(block(&path), Center::shape_for(3));
    }

    #[test]
    fn the_block_does_not_shrink_when_a_favorite_leaves() {
        let path = scratch("grow-not-back");
        std::fs::write(&path, "hotkey = 'alt+`'
").unwrap();
        for app in ["a.exe", "b.exe", "c.exe"] {
            assert!(add_to_center_in(&path, Half::Apps, app));
        }
        let grown = block(&path);

        assert!(remove_from_center_in(&path, Half::Apps, "c.exe"));
        assert!(remove_from_center_in(&path, Half::Apps, "b.exe"));
        assert_eq!(block(&path), grown, "the squares moved out from under the pointer");
    }

    #[test]
    fn one_block_counts_both_lists_against_the_same_slots() {
        // Split gives each list its own half, so a half holds the longer one.
        // One draws them end to end in a single half, which has to hold both.
        let path = scratch("grow-one-block");
        std::fs::write(&path, "[center]
contents = 'one'
").unwrap();
        assert!(add_to_center_in(&path, Half::Apps, "a.exe"));
        assert!(add_to_center_in(&path, Half::Sites, "https://example.com"));
        assert!(add_to_center_in(&path, Half::Sites, "https://example.org"));
        assert_eq!(block(&path), Center::shape_for(3));
    }

    #[test]
    fn a_hand_grown_block_is_left_alone() {
        // Bigger than it needs is a choice, and adding to it is not a reason to
        // take that choice back.
        let path = scratch("grow-hand-set");
        std::fs::write(&path, "[center]
rows = 4
columns = 4
").unwrap();
        assert!(add_to_center_in(&path, Half::Apps, "a.exe"));
        assert_eq!(block(&path), (4, 4));
    }

    // --- resetting the layout ---

    /// One backup file, deliberately: a reset undoes the last reset, not a
    /// history of them. That makes the reset tests share a file, so they take
    /// turns. Poisoning is another test's failure, which this one survives.
    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        TURN.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Somewhere disposable for the backup. The real place for it is a real
    /// install's directory, and a test run must never touch a config somebody
    /// is using. Set once; the ignored result is a later call finding it set.
    fn scratch_backups() -> PathBuf {
        let dir = std::env::temp_dir().join("bentolaunch-pins-test-backups");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = test_backup_dir().set(dir);
        test_backup_dir().get().unwrap().clone()
    }

    /// A config somebody has been living in: boxes moved out of their lanes,
    /// tiles resized, an app added by hand, the block filled and reshaped, a
    /// hotkey and a colour of their own, and a comment beside two of them.
    ///
    /// TOML literal strings for the paths, so a Windows path is a path rather
    /// than a row of escapes.
    const LIVED_IN: &str = r#"
# my hotkey, do not touch
hotkey = 'ctrl+alt+space'

[grid]
tile_width = 220.0
tile_height = 156.0
max_columns = 5

[theme]
# the one colour I got right
panel = '#F0112233'

[browser]
enabled = true

[center]
rows = 1
columns = 4
apps = ['notepad.exe']
sites = ['https://example.com']

[[sections]]
title = 'Launch'
source = ['taskbar', 'manual']
side = 'right'
max_items = 3
items = ['R:\dev']

[[sections]]
title = 'Active'
source = 'windows'
side = 'right'
"#;

    fn a_lived_in_config(name: &str) -> PathBuf {
        let path = scratch(name);
        std::fs::write(&path, LIVED_IN).unwrap();
        path
    }

    fn reread(path: &Path) -> Config {
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Title, lane and cap, in file order. The whole of what a reset promises
    /// to put back about the boxes.
    fn shape(config: &Config) -> Vec<(&str, Option<&str>, usize)> {
        config
            .sections
            .iter()
            .map(|s| (s.title.as_str(), s.side.as_deref(), s.max_items))
            .collect()
    }

    #[test]
    fn a_reset_puts_the_boxes_and_the_grid_back() {
        let _turn = one_at_a_time();
        scratch_backups();
        let path = a_lived_in_config("reset-layout");
        assert!(reset_layout_in(&path));

        let after = reread(&path);
        let stock = Config::default();
        assert_eq!(after.grid, stock.grid, "the grid was not put back");
        assert_eq!(shape(&after), shape(&stock), "the boxes are not in their stock lanes and order");

        // The stock block is off, but a reset must not be what hides a
        // favorite. The fixture holds one a side, so it comes back at the
        // smallest shape that draws them.
        assert_eq!((after.center.columns, after.center.rows), Center::shape_for(1));
    }

    #[test]
    fn a_reset_keeps_everything_that_is_not_layout() {
        let _turn = one_at_a_time();
        scratch_backups();
        let path = a_lived_in_config("reset-keeps");
        assert!(reset_layout_in(&path));

        let after = reread(&path);
        assert_eq!(after.hotkey, "ctrl+alt+space", "the hotkey was reset");
        assert_eq!(after.theme.panel, "#F0112233", "the theme was reset");
        assert!(after.browser.enabled, "the browser switch was reset");

        let launch = after.sections.iter().find(|s| s.title == "Launch").unwrap();
        assert_eq!(launch.items.len(), 1, "a hand-added item was dropped");
        assert_eq!(launch.items[0].target(), r"R:\dev");
        assert_eq!(after.center.apps.len(), 1, "the block's apps were dropped");
        assert_eq!(after.center.sites.len(), 1, "the block's sites were dropped");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my hotkey, do not touch"), "a comment was eaten:\n{text}");
        assert!(text.contains("# the one colour I got right"), "a comment was eaten:\n{text}");
    }

    #[test]
    fn a_reset_switches_the_block_off_when_nothing_is_in_it() {
        let _turn = one_at_a_time();
        scratch_backups();
        let path = scratch("reset-empty-block");
        std::fs::write(&path, "[center]
rows = 4
columns = 4
").unwrap();
        assert!(reset_layout_in(&path));
        assert!(!reread(&path).center.on(), "sixteen empty squares survived a reset");
    }

    #[test]
    fn a_reset_brings_back_a_box_that_was_deleted() {
        let _turn = one_at_a_time();
        scratch_backups();
        let path = scratch("reset-deleted");
        std::fs::write(&path, "[[sections]]\ntitle = 'Launch'\nsource = 'taskbar'\n").unwrap();
        assert!(reset_layout_in(&path));

        let after = reread(&path);
        assert_eq!(shape(&after), shape(&Config::default()), "the stock boxes did not come back");
    }

    #[test]
    fn a_reset_keeps_a_box_the_user_wrote_themselves() {
        let _turn = one_at_a_time();
        scratch_backups();
        let path = scratch("reset-own-box");
        let own = r"
[[sections]]
title = 'Places'
source = 'manual'
side = 'left'
items = ['R:\dev']
";
        std::fs::write(&path, own).unwrap();
        assert!(reset_layout_in(&path));

        let after = reread(&path);
        let places = after.sections.iter().find(|s| s.title == "Places").expect("Places was deleted");
        assert_eq!(places.items.len(), 1, "the box came back empty");
        assert_eq!(places.side.as_deref(), Some("left"), "the box was moved out of its lane");
    }

    #[test]
    fn a_reset_leaves_the_old_file_where_it_can_be_got_back() {
        let _turn = one_at_a_time();
        let backup = scratch_backups().join("bentolaunch.toml.bak");
        let _ = std::fs::remove_file(&backup);

        let path = a_lived_in_config("reset-backup");
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(reset_layout_in(&path));

        let saved = std::fs::read_to_string(&backup).expect("no backup was written");
        assert_eq!(saved, before, "the backup is not the file that was reset");
    }

    #[test]
    fn a_config_with_a_box_of_its_own_reads_as_stock_once_reset() {
        // The square greys itself off `layout_is_stock`, and a kept box must
        // not keep answering "still not stock" after the reset that kept it.
        let _turn = one_at_a_time();
        scratch_backups();
        let path = scratch("reset-stock-after");
        let own = r"
[[sections]]
title = 'Places'
source = 'manual'
side = 'left'
items = ['R:\dev']
";
        std::fs::write(&path, own).unwrap();
        assert!(!crate::ui::settings::layout_is_stock(&reread(&path)));

        assert!(reset_layout_in(&path));
        assert!(
            crate::ui::settings::layout_is_stock(&reread(&path)),
            "the square would stay live after a reset that changed everything it could",
        );
    }

    #[test]
    fn a_reset_reads_back_as_the_layout_it_wrote() {
        let _turn = one_at_a_time();
        scratch_backups();
        let path = a_lived_in_config("reset-round-trip");
        assert!(reset_layout_in(&path));
        let once = std::fs::read_to_string(&path).unwrap();

        // Twice: the second reset reads what the first wrote, so anything the
        // writer emits that the reader trips over turns up here.
        assert!(reset_layout_in(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), once, "a reset is not settled");
        assert_eq!(shape(&reread(&path)), shape(&Config::default()));
    }
}
