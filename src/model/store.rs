//! The live item list, kept current by `SetWinEventHook` so that showing the
//! panel is a read, never a scan.
//!
//! Windows are held in MRU order: the foreground hook moves the newly focused
//! window to the front, which is the order a switcher wants. Taskbar pins and
//! manual entries are resolved once at startup, because neither changes without
//! a restart and both touch the disk.

use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    OBJID_WINDOW, PostMessageW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
};

use crate::config::{Contents, Center, ManualItem, SectionConfig, Source, Sources};
// Imported by name rather than as a module: `windows` would otherwise shadow the
// `windows` crate throughout this file.
use crate::model::taskbar;
use crate::model::windows::{WindowInfo, enumerate, refresh_title, still_switchable};
use crate::model::{Handle, Item, ItemId, Kind, Mode, Section, Target};
use crate::{log_info, log_warn};

/// Posted to the panel when the item list changed. Only acted on while visible.
pub const WM_MODEL_CHANGED: u32 = WM_APP + 1;

/// One configured section, with whatever could be resolved up front.
struct Group {
    title: String,
    color: Option<String>,
    edge: Option<String>,
    /// Where this sits in the config file. Not where it sits in the panel: an
    /// empty section never reaches the grid, so the two drift apart. What is
    /// dealt out per section - a ring colour - has to key off this one, or a
    /// browser connecting would recolour every box below it.
    slot: usize,
    sources: Sources,
    /// Lowercased process names, one entry per source. Empty means catch-all.
    matches: Vec<Vec<String>>,
    /// Pre-resolved per source, keyed by its index in `sources`: taskbar pins
    /// and manual entries both touch the disk, so they are read once. Windows
    /// and tabs are absent — they are read at show time.
    fixed: Vec<(usize, Vec<Item>)>,
    /// 0 means no cap. Applied after every group has contributed, so the cut
    /// falls at the end of the section rather than inside one group.
    max_items: usize,
}

/// Open windows of this window's app, itself included. `1` makes a window tile
/// redundant: the app tile already reaches the only window there is.
///
/// No readable executable counts as a crowd, so such a window is never hidden.
/// Nothing can match it to an app tile either, so hiding it would strand it.
fn siblings(windows: &[WindowInfo], window: &WindowInfo) -> usize {
    let Some(app) = window.app() else {
        return usize::MAX;
    };
    windows
        .iter()
        .filter(|other| other.app().as_deref() == Some(app.as_str()))
        .count()
}

fn claims(matches: &[String], window: &WindowInfo) -> bool {
    if matches.is_empty() {
        return true;
    }
    let Some(exe) = window.exe.as_ref().and_then(|p| p.file_name()) else {
        return false;
    };
    matches.contains(&exe.to_string_lossy().to_lowercase())
}

/// The centre block, with its entries resolved.
///
/// Resolved up front for the same reason manual entries are: working out what a
/// parsing name is takes the disk, and show time is meant to be a read.
struct Block {
    shape: Center,
    /// Apps then sites. Two lists even when the block is not split, because
    /// they are still two lists; the block just draws them end to end.
    halves: [Vec<Item>; 2],
}

impl Block {
    /// Everything the block holds, as the strings that identify the same thing
    /// elsewhere on the panel.
    ///
    /// What it *shows*, not what it stores. A half the block is not drawing -
    /// sites, while it is set to apps only - is still written down and still
    /// comes back when the setting does, but taking those out of the lists they
    /// came from would be a tile disappearing off the panel entirely.
    fn held(&self) -> (Vec<String>, Vec<String>) {
        let mut targets = Vec::new();
        let mut apps = Vec::new();
        let shown = self
            .halves
            .iter()
            .enumerate()
            .filter(|(half, _)| self.shape.contents.shows(*half))
            .flat_map(|(_, items)| items);
        for item in shown {
            if let Some(name) = item.shell_target() {
                targets.push(name.to_lowercase());
            }
            if let Some(app) = &item.app {
                apps.push(app.to_lowercase());
            }
        }
        (targets, apps)
    }

    /// Whether the block has anything to draw.
    ///
    /// A half that is not being shown does not count, the same way `held` does
    /// not count it: `contents = "apps"` with sites full and apps empty is a
    /// block with nothing in it, however much is written down.
    fn draws_anything(&self) -> bool {
        self.halves
            .iter()
            .enumerate()
            .any(|(half, items)| self.shape.contents.shows(half) && !items.is_empty())
    }
}

struct Store {
    windows: Vec<WindowInfo>,
    groups: Vec<Group>,
    /// A `modes` box is configured somewhere.
    ///
    /// What decides whether the six moves are a bar that is always there or a
    /// bar that move mode brings out. Without a `modes` box there would be
    /// nothing left to turn the mode on with, so the old bar stays.
    has_modes: bool,
    center: Block,
    /// bentolaunch's own panel, which must never appear in its own grid.
    exclude: Handle,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
static HOOKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static NOTIFY: AtomicIsize = AtomicIsize::new(0);

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            windows: Vec::new(),
            groups: Vec::new(),
            has_modes: false,
            center: build_block(&Center::default()),
            exclude: Handle::new(HWND(std::ptr::null_mut())),
        })
    })
}

/// Resolve the parts of a section that do not change without a config edit:
/// taskbar pins and manual entries both touch the disk, so they are read once.
fn build_groups(sections: &[SectionConfig]) -> Vec<Group> {
    sections
        .iter()
        .enumerate()
        .map(|(slot, section)| Group {
            title: section.title.clone(),
            color: section.color.clone(),
            edge: section.edge.clone(),
            slot,
            sources: section.source.clone(),
            max_items: section.max_items,
            // A group's own `match` wins; without one it falls back to the
            // section's, which is what an unmerged section still writes.
            matches: section
                .source
                .iter()
                .map(|spec| {
                    spec.matches()
                        .unwrap_or(&section.matches)
                        .iter()
                        .map(|m| m.to_lowercase())
                        .collect()
                })
                .collect(),
            fixed: section
                .source
                .iter()
                .enumerate()
                .filter_map(|(index, spec)| match spec.source() {
                    Source::Taskbar => Some((index, taskbar::pins_in_order(&section.order))),
                    Source::Manual => {
                        Some((index, section.items.iter().filter_map(manual_item).collect()))
                    }
                    Source::Windows | Source::Extra | Source::Running | Source::Tabs
                    | Source::Bookmarks
                    | Source::Moves
                    | Source::Modes
                    // Read on a worker, or asked of a browser. Neither is fixed
                    // at config time.
                    | Source::AllApps
                    | Source::AllBookmarks
                    // Not fixed per section: the centre resolves them once and
                    // every list of them comes off that.
                    | Source::Center => None,
                })
                .collect(),
        })
        .collect()
}

/// Resolve the centre block's two lists, the same way manual entries are
/// resolved: once, because both touch the disk.
fn build_block(center: &Center) -> Block {
    let resolve = |list: &[ManualItem]| -> Vec<Item> {
        list.iter()
            .filter_map(manual_item)
            .map(|item| Item { origin: Source::Center, ..item })
            .collect()
    };
    Block {
        halves: [resolve(&center.apps), resolve(&center.sites)],
        shape: center.clone(),
    }
}

/// Rebuild sections after a config edit. Windows are left alone: the hooks have
/// been keeping that list current and it does not depend on config.
pub fn reconfigure(sections: &[SectionConfig], center: &Center) {
    let groups = build_groups(sections);
    let center = build_block(center);
    let has_modes = groups
        .iter()
        .any(|group| group.sources.iter().any(|spec| spec.source() == Source::Modes));
    if let Ok(mut s) = store().lock() {
        s.groups = groups;
        s.has_modes = has_modes;
        s.center = center;
    }
}

/// First and only full enumeration. Everything after this is incremental.
pub fn init(exclude: HWND, sections: &[SectionConfig], center: &Center) {
    let groups = build_groups(sections);
    let center = build_block(center);
    let has_modes = groups
        .iter()
        .any(|group| group.sources.iter().any(|spec| spec.source() == Source::Modes));
    let found = enumerate(exclude);
    log_info!("initial scan: {} windows", found.len());
    for (n, w) in found.iter().enumerate() {
        log_info!(
            "  [{n:>2}] {:<48} {} [{}]",
            truncate(&w.title, 48),
            w.exe
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "?".into()),
            w.class
        );
    }
    for group in &groups {
        let shape = group
            .sources
            .iter()
            .zip(&group.matches)
            .map(|(spec, matches)| match matches.len() {
                0 => format!("{:?}", spec.source()).to_lowercase(),
                n => format!("{:?}+{n}", spec.source()).to_lowercase(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        log_info!(
            "section \"{}\" [{shape}]: {} fixed item(s)",
            group.title,
            group.fixed.iter().map(|(_, items)| items.len()).sum::<usize>()
        );
    }

    log_info!(
        "centre block: {} app(s), {} site(s), {}x{} slots a side",
        center.halves[0].len(),
        center.halves[1].len(),
        center.shape.columns,
        center.shape.rows
    );

    if let Ok(mut s) = store().lock() {
        s.exclude = Handle::new(exclude);
        s.windows = found;
        s.groups = groups;
        s.has_modes = has_modes;
        s.center = center;
    }
}

/// Build a tile from a manual config entry.
///
/// Everything here is a shell parsing name, so nothing needs to exist on disk —
/// `ms-settings:display` is as valid a target as `R:\dev`. Existence only
/// affects which title and icon bentolaunch can infer.
fn manual_item(entry: &ManualItem) -> Option<Item> {
    let target = entry.target().trim();
    if target.is_empty() {
        return None;
    }
    let kind = derive_kind(target);
    let title = entry
        .title()
        .map(str::to_owned)
        .unwrap_or_else(|| derive_title(target));

    Some(Item {
        id: ItemId::Shell(target.to_owned()),
        kind,
        title,
        detail: shorten_detail(target),
        target: Target::Shell(target.to_owned()),
        app: crate::shell::link::app_stem(target),
        icon_source: Some(target.to_owned()),
        origin: Source::Manual,
        link: None,
        running: None,
        group: 0,
    })
}

/// A URI scheme is letters followed by `:`. A drive letter is a single
/// character, so `C:\x` is a path and `ms-settings:display` is a link.
fn is_uri(spec: &str) -> bool {
    match spec.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        None => false,
    }
}

fn derive_kind(spec: &str) -> Kind {
    let path = Path::new(spec);
    if path.is_dir() {
        Kind::Folder
    } else if path.is_file() {
        Kind::App
    } else if is_uri(spec) {
        Kind::Link
    } else {
        log_warn!("manual entry does not exist and is not a URI: {spec}");
        Kind::App
    }
}

fn derive_title(spec: &str) -> String {
    let path = Path::new(spec);
    if path.is_dir() {
        return path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.to_owned());
    }
    if path.is_file() {
        return path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.to_owned());
    }
    if let Some((scheme, rest)) = spec.split_once(':')
        && is_uri(spec)
    {
        let rest = rest.trim_start_matches('/').trim_end_matches('/');
        return if rest.is_empty() { scheme.to_owned() } else { rest.to_owned() };
    }
    spec.to_owned()
}

/// Long paths are unreadable at tile width; keep the tail, which is the part
/// that identifies the target.
fn shorten_detail(spec: &str) -> String {
    const MAX: usize = 40;
    if spec.chars().count() <= MAX {
        return spec.to_owned();
    }
    let tail: String = spec
        .chars()
        .skip(spec.chars().count().saturating_sub(MAX - 1))
        .collect();
    format!("…{tail}")
}

/// The grid, in config order. Empty sections are dropped so a header never
/// appears over nothing.
///
/// Each window is claimed by the first section whose `match` accepts it, so a
/// filtered section listed above the catch-all is what pulls the browsers, or
/// Explorer, out into their own group. No window appears twice.
/// Who gets foreground rights before a browser raises itself. Not the socket's
/// peer: Chrome opens sockets from its network process, which owns no windows.
pub fn browser_pids() -> Vec<u32> {
    let Ok(s) = store().lock() else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = s
        .windows
        .iter()
        .filter(|w| {
            w.exe
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_lowercase())
                .is_some_and(|exe| crate::config::BROWSERS.contains(&exe.as_str()))
        })
        .map(|w| w.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// "New tab", when there is a browser to ask. Only ever the lowest connection:
/// with two browsers paired this would otherwise be two buttons that look the
/// same, or one that changes which browser it means.
fn new_tab_item() -> Option<Item> {
    let connection = *crate::browser::server::connections().first()?;
    Some(Item {
        id: ItemId::Action("newtab"),
        kind: Kind::Action,
        title: "New tab".into(),
        detail: String::new(),
        target: Target::NewTab { connection },
        icon_source: None,
        app: None,
        origin: Source::Tabs,
        link: None,
        running: None,
        group: 0,
    })
}

fn tab_items() -> Vec<Item> {
    new_tab_item()
        .into_iter()
        .chain(
            crate::browser::server::tabs()
        .into_iter()
        .map(|owned| Item {
            id: ItemId::Tab(owned.connection, owned.tab.id),
            kind: Kind::Tab,
            // Filtering searches this line, so a generic title is still
            // findable by host.
            //
            // Truncated like a window title, and for a stronger reason: this
            // one arrives over a socket, and laying out a megabyte of text
            // would happen on the UI thread.
            detail: truncate(owned.tab.host(), 48),
            title: if owned.tab.title.is_empty() {
                truncate(owned.tab.host(), 48)
            } else {
                truncate(&owned.tab.title, 48)
            },
            target: Target::Tab {
                connection: owned.connection,
                tab_id: owned.tab.id,
                window_id: owned.tab.window_id,
            },
            icon_source: owned
                .tab
                .icon
                .as_ref()
                .map(|key| format!("{}{key}", crate::shell::icons::FAVICON)),
            app: None,
            origin: Source::Tabs,
            link: Some(owned.tab.url.clone()),
            running: None,
            group: 0,
        }),
        )
        .collect()
}

fn bookmark_items() -> Vec<Item> {
    crate::browser::server::bookmarks()
        .into_iter()
        .map(|bookmark| Item {
            id: ItemId::Shell(bookmark.url.clone()),
            kind: Kind::Link,
            detail: truncate(bookmark.host(), 48),
            title: if bookmark.title.is_empty() {
                truncate(bookmark.host(), 48)
            } else {
                truncate(&bookmark.title, 48)
            },
            // A URL the shell already opens, so this needs nothing from the
            // socket. The browser can be closed and the tile still works.
            target: Target::Shell(bookmark.url.clone()),
            icon_source: bookmark
                .icon
                .as_ref()
                .map(|key| format!("{}{key}", crate::shell::icons::FAVICON)),
            app: None,
            origin: Source::Bookmarks,
            link: Some(bookmark.url.clone()),
            running: None,
            group: 0,
        })
        .collect()
}

/// Every bookmark there is, folder path and all.
///
/// Empty until a browser has been asked and has answered, and empty for good
/// against an extension that predates the question - which is a box that draws
/// nothing rather than a panel that breaks.
///
/// No favicon came with these. One is filed by origin, so anything sharing a
/// site with an open tab or a bar entry is already wearing the right picture;
/// the rest fall back to the shell, exactly as a hand-written site favorite
/// does.
fn all_bookmark_items() -> Vec<Item> {
    crate::browser::server::tree()
        .into_iter()
        .map(|bookmark| {
            let item = Item {
                id: ItemId::Shell(bookmark.url.clone()),
                kind: Kind::Link,
                // The folder, not the host: in an archive of thousands, which
                // folder something is filed under is what tells two bookmarks
                // of the same site apart. The host is in the title already
                // whenever there is nothing else to call it.
                detail: match bookmark.folder.is_empty() {
                    true => truncate(bookmark.host(), 48),
                    false => truncate(&bookmark.folder, 48),
                },
                title: if bookmark.title.is_empty() {
                    truncate(bookmark.host(), 48)
                } else {
                    truncate(&bookmark.title, 48)
                },
                target: Target::Shell(bookmark.url.clone()),
                icon_source: None,
                app: None,
                origin: Source::AllBookmarks,
                link: Some(bookmark.url.clone()),
                running: None,
                group: 0,
            };
            wearing_favicon(&item)
        })
        .collect()
}

/// The modes, one tile each.
///
/// A fixed set in a fixed order, so the four squares are in the same four
/// places every summon. `Move` leads because it is the one that used to be
/// seven tiles of its own.
pub const MODES: [Mode; 4] = [Mode::Move, Mode::Center, Mode::Close, Mode::Layout];

fn mode_items() -> Vec<Item> {
    MODES
        .iter()
        .map(|mode| Item {
            id: ItemId::Action(mode.label()),
            kind: Kind::Action,
            title: mode.label().to_owned(),
            detail: String::new(),
            target: Target::Mode(*mode),
            // Drawn, not fetched: the mark on these is a picture of what the
            // mode does to the panel.
            icon_source: None,
            app: None,
            origin: Source::Modes,
            link: None,
            running: None,
            group: 0,
        })
        .collect()
}

/// Every installed app, alphabetical.
///
/// Empty while the reader is still walking `shell:AppsFolder`, which takes a
/// moment on a machine with a lot installed. An empty box draws nothing and the
/// panel is told to rebuild when the list lands, so the wait shows as the box
/// filling in rather than as the panel being stuck.
fn all_app_items() -> Vec<Item> {
    let Some(apps) = crate::shell::apps::request() else {
        return Vec::new();
    };
    apps.iter()
        .map(|app| Item {
            id: ItemId::Shell(app.target.clone()),
            kind: Kind::App,
            title: app.title.clone(),
            detail: String::new(),
            target: Target::Shell(app.target.clone()),
            icon_source: Some(app.target.clone()),
            app: None,
            origin: Source::AllApps,
            link: None,
            running: None,
            group: 0,
        })
        .collect()
}

/// The square that opens the rest of them, at the end of the box showing a few.
///
/// The taskbar is a shortlist somebody curated and so is the bookmarks bar, so
/// anything not on either had no way onto the panel at all. Last in its box,
/// because a box's whole worth is that its tiles are where they were last time.
fn all_tile(mode: Mode, id: &'static str) -> Item {
    Item {
        id: ItemId::Action(id),
        kind: Kind::Action,
        title: mode.label().to_owned(),
        detail: String::new(),
        target: Target::Mode(mode),
        // Drawn, not fetched: nine squares where the box shows a handful.
        icon_source: None,
        app: None,
        origin: Source::Modes,
        link: None,
        running: None,
        group: 0,
    }
}

/// The tile naming the window being moved, then the six moves. Fixed, so this
/// reads nothing and never comes back empty.
fn move_items() -> Vec<Item> {
    let action = |id, title: &str, target| Item {
        id: ItemId::Action(id),
        kind: Kind::Action,
        title: title.to_owned(),
        detail: String::new(),
        target,
        // Drawn, not fetched: the mark on these is the shape they make.
        icon_source: None,
        app: None,
        origin: Source::Moves,
        link: None,
        running: None,
        group: 0,
    };

    // Stay first: it names what the rest act on, so it reads before them.
    std::iter::once(action("stay", "Stay open", Target::Stay))
        .chain(
            crate::shell::arrange::MOVES
                .iter()
                .map(|mv| action(mv.key(), mv.label(), Target::Arrange(*mv))),
        )
        .collect()
}

/// One empty slot in the centre block.
///
/// Drawn rather than left out. The block earns its place by being the same
/// shape in the same spot every summon, and a block that shrank as it emptied
/// would be a set of moving targets - which is the one thing a gaze pointer
/// cannot use.
fn empty_slot(n: usize) -> Item {
    Item {
        id: ItemId::Slot(n),
        kind: Kind::Action,
        // No words. Filtering searches the title, and an empty square that
        // survived a query would be a match that means nothing.
        title: String::new(),
        detail: String::new(),
        target: Target::Slot,
        icon_source: None,
        app: None,
        origin: Source::Center,
        link: None,
        running: None,
        // A group of its own, which is what stops a drag inside the block from
        // running off the end of what is filled and into the empties. It also
        // gives the empty squares the alternating fill, so they read as places
        // rather than as tiles.
        group: 1,
    }
}

/// The centre block, as sections.
///
/// Last in the list on purpose. Every other box works out where its tiles are
/// from what is configured ahead of it, and the centre is not configured as a
/// section at all, so it goes on the end where it disturbs nothing.
///
/// Untitled, because the block never draws a header: it is the most valuable
/// space on the panel and a title would spend a row of it saying what eight
/// icons already say.
/// The origin of a URL, spelled the way the extension spells it: scheme, `://`,
/// host and port, lowercased. That string is the key every favicon is filed
/// under, because one bitmap serves every page on a site.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{}", host.to_lowercase()))
}

/// Give a site favorite the browser's own favicon when there is one to give.
///
/// Without this every site in the block comes back wearing the default
/// browser's logo, because that is genuinely what the shell knows about a URL -
/// and four identical logos in the middle of the screen is the block failing at
/// the only thing it is for. Asked afresh every time the grid is built, so a
/// favicon that turns up after a browser connects is picked up on the next
/// summon rather than waiting for a config edit.
fn wearing_favicon(item: &Item) -> Item {
    let Some(origin) = item.shell_target().and_then(origin_of) else {
        return item.clone();
    };
    if !crate::shell::icons::has_favicon(&origin) {
        return item.clone();
    }
    Item {
        icon_source: Some(format!("{}{origin}", crate::shell::icons::FAVICON)),
        ..item.clone()
    }
}

fn center_sections(center: &Block, mode: Mode) -> Vec<Section> {
    // Off, the block holds nothing and so is not a box on the panel at all -
    // and a square that switched it off would then have nowhere to be clicked
    // to switch it back on. Edit mode keeps one empty slot in the middle for
    // it, which is a box, which is somewhere to click.
    //
    // Center mode keeps it for the same reason from the other end: off is
    // the shipped state now, so the first favorite anyone ever adds is added to
    // a block that is not there, and the mode has to show where it will land.
    //
    // Empty is the same as off here, whatever shape the file asks for. Nine
    // squares a half holding nothing is the middle of the screen spent on
    // nothing, and `rows` says how big the block gets, not that it has to be
    // there before there is anything to put in it. Once it holds something it
    // draws its whole shape, empty slots and all - those are where the next one
    // lands, and they stay put as things come and go.
    if !center.shape.on() || !center.draws_anything() {
        return match mode {
            Mode::Layout | Mode::Center => vec![Section {
                title: String::new(),
                items: vec![empty_slot(0)],
                color: center.shape.color.clone(),
                edge: None,
                slot: usize::MAX,
                total: 0,
                center: Some(0),
                columns: 1,
            }],
            _ => Vec::new(),
        };
    }
    let slots = center.shape.slots();
    let halves: Vec<Vec<Item>> = match center.shape.contents {
        Contents::Split => center.halves.to_vec(),
        // One block: the two lists end to end, in the same order they would
        // have sat side by side.
        Contents::One => vec![center.halves.iter().flatten().cloned().collect()],
        // One list only. The other is still written and still read; it just has
        // nowhere to be drawn, which is what "apps only" means.
        Contents::Apps => vec![center.halves[0].clone()],
        Contents::Sites => vec![center.halves[1].clone()],
    };

    halves
        .into_iter()
        .enumerate()
        .map(|(half, held)| {
            let total = held.len();
            let mut items: Vec<Item> =
                held.iter().take(slots).map(wearing_favicon).collect();
            // Numbered across the whole block, so two empty slots in different
            // halves are still two different tiles to hover.
            for n in items.len()..slots {
                items.push(empty_slot(half * slots + n));
            }
            Section {
                title: String::new(),
                items,
                color: center.shape.color.clone(),
                // The block wears its own frame, in the accent. A palette
                // colour on top of that would be a second line saying the
                // same thing in a different hue, and no ring is drawn round a
                // centre half at all - so the slot is never asked for.
                edge: None,
                slot: usize::MAX,
                total,
                center: Some(half),
                columns: center.shape.columns,
            }
        })
        .collect()
}

/// The grid, as the panel should show it right now.
///
/// `mode` is the only thing here that is not a fact about the machine: the six
/// moves are a box that fills only while move mode is on. An empty section
/// draws nothing, so a `move` box that has nothing to say costs no row - which
/// is what lets a `modes` bar of four squares stand in for a bar of seven.
pub fn sections(mode: Mode) -> Vec<Section> {
    // One box holding all of something, in place of the grid rather than beside
    // it: three hundred tiles are not a box that fits next to anything. The
    // corner button is the way out, as it is out of every mode.
    if let Mode::AllApps | Mode::AllBookmarks = mode {
        let items = match mode {
            Mode::AllBookmarks => all_bookmark_items(),
            _ => all_app_items(),
        };
        let total = items.len();
        return vec![Section {
            title: mode.label().to_owned(),
            items,
            color: None,
            edge: None,
            slot: usize::MAX,
            total,
            center: None,
            columns: 0,
        }];
    }

    let Ok(s) = store().lock() else {
        log_warn!("item store is poisoned; showing an empty grid");
        return Vec::new();
    };

    // Anything the centre is holding is left out of the list it came from. One
    // thing appearing twice on one panel costs its slot the only property that
    // makes a fixed position worth having.
    // Launchable tiles only. A window is a different question from the app it
    // belongs to: favoriting Chrome says where to start one, and says nothing
    // about the four Chrome windows already open.
    let (held_targets, held_apps) = s.center.held();
    let doubled = |item: &Item| match item.origin {
        Source::Taskbar | Source::Manual | Source::Running | Source::Bookmarks => {
            item.shell_target()
                .is_some_and(|name| held_targets.contains(&name.to_lowercase()))
                || item
                    .app
                    .as_ref()
                    .is_some_and(|app| held_apps.contains(&app.to_lowercase()))
        }
        Source::Windows | Source::Extra | Source::Tabs | Source::Moves | Source::Modes
        // Lists to launch from rather than ones the centre is drawn out of.
        | Source::AllApps
        | Source::AllBookmarks
        // The list itself, wherever it is shown.
        | Source::Center => false,
    };

    let mut claimed = vec![false; s.windows.len()];
    let mut out = Vec::with_capacity(s.groups.len());

    for group in &s.groups {
        // Groups contribute in the order they are listed, so a merged section
        // still has a fixed shape: the tabs never land among the windows.
        let mut items = Vec::new();
        for (index, spec) in group.sources.iter().enumerate() {
            let start = items.len();
            match spec.source() {
                Source::Windows => {
                    let matches = group.matches.get(index).map_or(&[][..], Vec::as_slice);
                    for (n, window) in s.windows.iter().enumerate() {
                        if claimed[n] || !claims(matches, window) {
                            continue;
                        }
                        claimed[n] = true;
                        items.push(window.to_item());
                    }
                }
                Source::Extra => {
                    let matches = group.matches.get(index).map_or(&[][..], Vec::as_slice);
                    for (n, window) in s.windows.iter().enumerate() {
                        if claimed[n] || !claims(matches, window) {
                            continue;
                        }
                        // One window: the app tile reaches it. Two or more:
                        // only titles say which one you want.
                        if siblings(&s.windows, window) < 2 {
                            continue;
                        }
                        claimed[n] = true;
                        items.push(window.to_item());
                    }
                }
                Source::Running => {
                    // Against what the section already holds, or a pin marked
                    // running gets doubled by the window that marked it.
                    let mut seen: Vec<String> =
                        items.iter().filter_map(|item| item.app.clone()).collect();
                    for window in &s.windows {
                        let (Some(app), Some(exe)) = (window.app(), window.exe.as_ref()) else {
                            continue;
                        };
                        if seen.contains(&app) {
                            continue;
                        }
                        let name = exe.to_string_lossy().into_owned();
                        items.push(Item {
                            id: ItemId::Shell(name.clone()),
                            kind: Kind::App,
                            // Executable name, never the window title. One tile
                            // per app, and a tile that renames itself as you
                            // browse has no learnable position.
                            title: exe
                                .file_stem()
                                .map_or_else(|| app.clone(), |s| s.to_string_lossy().into_owned()),
                            detail: "running".into(),
                            target: Target::Window(window.handle),
                            icon_source: Some(name),
                            app: Some(app.clone()),
                            origin: Source::Running,
                            link: None,
                            running: Some(window.handle),
                            group: 0,
                        });
                        seen.push(app);
                    }
                }
                // Read at show time, not resolved up front: they change as fast
                // as the browser does.
                Source::Tabs => items.extend(tab_items()),
                Source::Bookmarks => items.extend(bookmark_items()),
                // Only while they apply. A `move` box listed alongside no
                // `modes` box anywhere is the old always-on bar and stays on.
                Source::Moves => {
                    if mode == Mode::Move || !s.has_modes {
                        items.extend(move_items());
                    }
                }
                Source::Modes => items.extend(mode_items()),
                // The centre's own lists, for anyone who would rather have
                // them as an ordinary box than in the middle of the panel.
                Source::Center => {
                    items.extend(s.center.halves.iter().flatten().cloned())
                }
                Source::Taskbar | Source::Manual => {
                    if let Some((_, fixed)) = group.fixed.iter().find(|(n, _)| *n == index) {
                        items.extend(fixed.iter().cloned());
                    }
                }
                Source::AllApps => items.extend(all_app_items()),
                Source::AllBookmarks => items.extend(all_bookmark_items()),
            }
            for item in &mut items[start..] {
                item.group = index;
            }
        }

        items.retain(|item| !doubled(item));

        mark_running(&mut items, &s.windows);

        let total = items.len();
        if group.max_items > 0 {
            items.truncate(group.max_items);
        }

        // The square that opens all of them, last in the box showing a few.
        // After the cut, because it is the way to what `max_items` just took
        // away. A group of its own, so a drag among the tiles above cannot run
        // into it.
        let opens = group.sources.iter().find_map(|spec| match spec.source() {
            Source::Taskbar => Some((Mode::AllApps, "all-apps")),
            // Only while a browser is connected. Nothing else can answer for
            // the tree, and a square that opens an empty box reads as broken.
            // The bar being empty is not the test: a browser with nothing on
            // its bar still has an archive behind it.
            Source::Bookmarks if !crate::browser::server::connections().is_empty() => {
                Some((Mode::AllBookmarks, "all-bookmarks"))
            }
            _ => None,
        });
        if let Some((mode, id)) = opens {
            let mut tile = all_tile(mode, id);
            tile.group = group.sources.iter().count();
            items.push(tile);
        }

        if !items.is_empty() {
            out.push(Section {
                title: group.title.clone(),
                color: group.color.clone(),
                edge: group.edge.clone(),
                slot: group.slot,
                items,
                total,
                center: None,
                columns: 0,
            });
        }
    }

    // The centre last, so the flat run of every other box is where it was.
    for mut section in center_sections(&s.center, mode) {
        mark_running(&mut section.items, &s.windows);
        out.push(section);
    }
    out
}

/// Mark the pin rather than grow a second tile, as the taskbar does.
///
/// Pins only: a window tile is already the window. `windows` is MRU first, so
/// the first match is the one to switch to.
fn mark_running(items: &mut [Item], windows: &[WindowInfo]) {
    for item in items {
        if item.shell_target().is_none() {
            continue;
        }
        let Some(app) = item.app.clone() else { continue };
        item.running = windows
            .iter()
            .find(|window| window.app().as_deref() == Some(app.as_str()))
            .map(|window| window.handle);
    }
}

/// Character-aware truncation; window titles are full of non-ASCII.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// `notify` receives `WM_MODEL_CHANGED` whenever the list changes.
pub fn install_hooks(notify: HWND) {
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);

    // Grouped into contiguous ranges; one hook per range is cheaper than one per
    // event. WINEVENT_SKIPOWNPROCESS keeps bentolaunch from reacting to itself.
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];

    let mut handles = Vec::new();
    for (first, last) in ranges {
        // SAFETY: out-of-context hooks deliver on this thread's message loop, so
        // `on_event` never runs concurrently with the UI thread.
        let hook = unsafe {
            SetWinEventHook(
                first,
                last,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.is_invalid() {
            log_warn!("SetWinEventHook failed for range {first:#x}..{last:#x}");
        } else {
            handles.push(hook.0 as isize);
        }
    }
    log_info!("installed {} window event hooks", handles.len());
    if let Ok(mut h) = HOOKS.lock() {
        *h = handles;
    }
}

pub fn uninstall_hooks() {
    let Ok(mut handles) = HOOKS.lock() else { return };
    for raw in handles.drain(..) {
        // SAFETY: each handle came from a successful SetWinEventHook and is
        // unhooked exactly once.
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(raw as *mut core::ffi::c_void));
        }
    }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole top-level windows. Without this we would also see every button
    // and menu item in every app on the machine.
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }

    let changed = match event {
        EVENT_SYSTEM_FOREGROUND => on_foreground(hwnd),
        EVENT_OBJECT_CREATE | EVENT_OBJECT_UNCLOAKED => on_appear(hwnd),
        EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => on_vanish(hwnd),
        EVENT_OBJECT_NAMECHANGE => on_rename(hwnd),
        _ => false,
    };

    if changed {
        let notify = NOTIFY.load(Ordering::SeqCst);
        if notify != 0 {
            // SAFETY: posting is asynchronous and safe even if the target is
            // mid-teardown; a failed post is not worth reacting to.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(notify as *mut core::ffi::c_void)),
                    WM_MODEL_CHANGED,
                    Default::default(),
                    Default::default(),
                );
            }
        }
    }
}

fn on_foreground(hwnd: HWND) -> bool {
    let handle = Handle::new(hwnd);
    let Ok(mut s) = store().lock() else { return false };
    if let Some(pos) = s.windows.iter().position(|w| w.handle == handle) {
        if pos == 0 {
            return false;
        }
        let entry = s.windows.remove(pos);
        s.windows.insert(0, entry);
        return true;
    }
    drop(s);
    // Foreground for something we have not seen: it just became eligible.
    on_appear(hwnd)
}

fn on_appear(hwnd: HWND) -> bool {
    let handle = Handle::new(hwnd);
    let exclude = {
        let Ok(s) = store().lock() else { return false };
        if handle == s.exclude || s.windows.iter().any(|w| w.handle == handle) {
            return false;
        }
        s.exclude
    };
    if !still_switchable(hwnd) {
        return false;
    }
    // A full pass, but only on a create/uncloak event, not on the hotkey. The
    // owner-chain test in `is_switchable` needs the surrounding windows to
    // decide whether this one is the switchable member of its group.
    let Some(info) = enumerate(exclude.hwnd())
        .into_iter()
        .find(|w| w.handle == handle)
    else {
        return false;
    };
    let Ok(mut s) = store().lock() else { return false };
    if s.windows.iter().any(|w| w.handle == handle) {
        return false;
    }
    s.windows.insert(0, info);
    true
}

fn on_vanish(hwnd: HWND) -> bool {
    let handle = Handle::new(hwnd);
    let Ok(mut s) = store().lock() else { return false };
    let before = s.windows.len();
    s.windows.retain(|w| w.handle != handle);
    s.windows.len() != before
}

fn on_rename(hwnd: HWND) -> bool {
    let Some(title) = refresh_title(hwnd) else {
        return false;
    };
    let handle = Handle::new(hwnd);
    let Ok(mut s) = store().lock() else { return false };
    match s.windows.iter_mut().find(|w| w.handle == handle) {
        Some(w) if w.title != title => {
            w.title = title;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_are_paths_and_schemes_are_uris() {
        assert!(!is_uri(r"C:\Windows\notepad.exe"));
        assert!(!is_uri(r"R:\dev"));
        assert!(is_uri("ms-settings:display"));
        assert!(is_uri("https://example.com"));
        assert!(!is_uri("plain-text"));
    }

    #[test]
    fn uri_titles_drop_the_scheme() {
        assert_eq!(derive_title("ms-settings:display"), "display");
        assert_eq!(derive_title("https://example.com/"), "example.com");
    }

    #[test]
    fn a_named_entry_keeps_its_title() {
        let entry = ManualItem::Named {
            title: "Display".into(),
            target: "ms-settings:display".into(),
        };
        let item = manual_item(&entry).unwrap();
        assert_eq!(item.title, "Display");
        assert_eq!(item.kind, Kind::Link);
        assert_eq!(item.target, Target::Shell("ms-settings:display".into()));
    }

    #[test]
    fn blank_entries_are_dropped() {
        assert!(manual_item(&ManualItem::Plain("   ".into())).is_none());
    }

    fn window(exe: Option<&str>, title: &str) -> WindowInfo {
        WindowInfo {
            handle: Handle::new(HWND(std::ptr::null_mut())),
            title: title.into(),
            class: "Test".into(),
            exe: exe.map(std::path::PathBuf::from),
            pid: 0,
        }
    }

    /// The rule behind `source = "extra"`: one window is already reached by the
    /// app tile, so listing it again by title says nothing.
    #[test]
    fn an_apps_only_window_has_no_siblings() {
        let windows = vec![
            window(Some(r"C:\Windows\explorer.exe"), "Downloads"),
            window(Some(r"C:\chrome.exe"), "A tab"),
        ];
        assert_eq!(siblings(&windows, &windows[0]), 1);
    }

    #[test]
    fn windows_of_one_app_count_each_other() {
        let windows = vec![
            window(Some(r"C:\Windows\explorer.exe"), "Downloads"),
            window(Some(r"C:\Windows\explorer.exe"), "R:\\dev"),
            window(Some(r"C:\chrome.exe"), "A tab"),
        ];
        assert_eq!(siblings(&windows, &windows[0]), 2);
        assert_eq!(siblings(&windows, &windows[2]), 1);
    }

    /// Case and directory differ, the app does not.
    #[test]
    fn the_same_app_from_two_paths_still_counts_as_one() {
        let windows = vec![
            window(Some(r"C:\Program Files\App\Thing.exe"), "one"),
            window(Some(r"D:\other\thing.EXE"), "two"),
        ];
        assert_eq!(siblings(&windows, &windows[0]), 2);
    }

    /// Nothing pairs this with an app tile, so it must never be hidden.
    #[test]
    fn a_window_with_no_executable_is_never_hidden() {
        let windows = vec![window(None, "mystery")];
        assert!(siblings(&windows, &windows[0]) >= 2);
    }

    #[test]
    fn long_targets_keep_their_tail() {
        let long = format!("C:\\{}\\thing.exe", "x".repeat(80));
        let detail = shorten_detail(&long);
        assert!(detail.chars().count() <= 40);
        assert!(detail.ends_with("thing.exe"));
    }

    // --- the centre block ---

    /// A two-by-two block, whatever the default happens to be. The shape is
    /// what most of these are about, so it is stated rather than inherited:
    /// changing what a new install starts with must not rewrite the arithmetic
    /// these tests are checking.
    fn center(apps: &[&str], sites: &[&str]) -> Center {
        Center {
            rows: 2,
            columns: 2,
            apps: apps.iter().map(|s| ManualItem::Plain((*s).into())).collect(),
            sites: sites.iter().map(|s| ManualItem::Plain((*s).into())).collect(),
            ..Center::default()
        }
    }

    #[test]
    fn a_half_is_padded_out_to_its_slots_so_the_block_keeps_its_shape() {
        let center = build_block(&center(&["ms-settings:display"], &[]));
        let out = center_sections(&center, Mode::Grid);
        assert_eq!(out.len(), 2, "split means two halves");
        // Two rows of two: four slots, one filled and three waiting.
        assert_eq!(out[0].items.len(), 4);
        assert_eq!(out[1].items.len(), 4);
        assert_eq!(
            out[0].items.iter().filter(|i| i.target == Target::Slot).count(),
            3
        );
        assert!(out[1].items.iter().all(|i| i.target == Target::Slot));
    }

    #[test]
    fn the_halves_are_numbered_left_to_right() {
        // Something in it: an empty block is not drawn at all now.
        let center = build_block(&center(&["ms-settings:display"], &[]));
        let out = center_sections(&center, Mode::Grid);
        assert_eq!(out[0].center, Some(0));
        assert_eq!(out[1].center, Some(1));
    }

    #[test]
    fn an_empty_block_is_not_drawn_however_big_it_is_set() {
        // `rows` says how big the block gets, not that it has to be there
        // before there is anything to put in it.
        let center = build_block(&Center { rows: 3, columns: 3, ..center(&[], &[]) });
        assert!(center_sections(&center, Mode::Grid).is_empty());
    }

    #[test]
    fn an_empty_block_still_shows_where_a_favorite_would_land() {
        // Collapsed is not gone. Both modes that put something in it have to
        // show the square it goes to, or there is no way back to a block.
        let center = build_block(&Center { rows: 3, columns: 3, ..center(&[], &[]) });
        for mode in [Mode::Center, Mode::Layout] {
            let out = center_sections(&center, mode);
            assert_eq!(out.len(), 1, "no landing square in {mode:?}");
            assert!(out[0].items.iter().all(|i| i.target == Target::Slot));
        }
    }

    #[test]
    fn a_half_that_is_not_shown_does_not_keep_the_block_open() {
        // Apps only, with sites full and apps empty. The block draws nothing,
        // so it is empty however much is written down.
        let center = build_block(&Center {
            contents: Contents::Apps,
            ..center(&[], &["https://example.com"])
        });
        assert!(center_sections(&center, Mode::Grid).is_empty());
    }

    #[test]
    fn one_favorite_still_draws_the_whole_shape() {
        // Collapsing is about empty, not about fitting. The slots either side
        // of a favorite are where the next one lands and must not move.
        let center = build_block(&Center {
            rows: 3,
            columns: 3,
            ..center(&["ms-settings:display"], &[])
        });
        let out = center_sections(&center, Mode::Grid);
        assert_eq!(out[0].items.len(), 9, "the block shrank to fit its contents");
    }

    #[test]
    fn an_unsplit_block_is_one_box_holding_both_lists_in_turn() {
        let center = build_block(&Center {
            contents: Contents::One,
            ..center(&["ms-settings:display"], &["https://example.com"])
        });
        let out = center_sections(&center, Mode::Grid);
        assert_eq!(out.len(), 1);
        let filled: Vec<&str> = out[0]
            .items
            .iter()
            .filter_map(|item| item.shell_target())
            .collect();
        assert_eq!(filled, vec!["ms-settings:display", "https://example.com"]);
    }

    #[test]
    fn one_list_only_is_one_box_holding_that_list() {
        for (contents, held) in
            [(Contents::Apps, "ms-settings:display"), (Contents::Sites, "https://example.com")]
        {
            let center = build_block(&Center {
                contents,
                ..center(&["ms-settings:display"], &["https://example.com"])
            });
            let out = center_sections(&center, Mode::Grid);
            assert_eq!(out.len(), 1, "{contents:?} drew more than one box");
            let filled: Vec<&str> =
                out[0].items.iter().filter_map(|item| item.shell_target()).collect();
            assert_eq!(filled, vec![held], "{contents:?} holds the wrong list");
        }
    }

    /// It belongs to the box that mirrors the taskbar, not to the modes bar.
    /// The bar is four squares that are always the same four in the same
    /// places; a fifth that is about one box on the panel would be a fifth
    /// answer to a different question.
    #[test]
    fn the_all_of_it_squares_are_not_modes_bar_squares() {
        for (mode, id) in [(Mode::AllApps, "all-apps"), (Mode::AllBookmarks, "all-bookmarks")] {
            assert!(!MODES.contains(&mode), "{mode:?} took a place in the bar");
            let tile = all_tile(mode, id);
            assert_eq!(tile.target, Target::Mode(mode));
            assert!(!tile.title.is_empty(), "{mode:?}'s square has no words");
            assert!(mode.done().is_some(), "no way out of {mode:?}");
        }
    }

    #[test]
    fn a_list_the_block_is_not_showing_stays_in_the_list_it_came_from() {
        // Set to apps only, the sites are still written down and still come
        // back when the setting does. Taking them out of `Browsing` as well
        // would be a tile that is on no part of the panel at all.
        let center = build_block(&Center {
            contents: Contents::Apps,
            ..center(&["ms-settings:display"], &["https://example.com"])
        });
        let (targets, _) = center.held();
        assert!(targets.contains(&"ms-settings:display".to_owned()));
        assert!(!targets.contains(&"https://example.com".to_owned()));
    }

    #[test]
    fn a_block_turned_off_is_no_sections_at_all() {
        let center = build_block(&Center { rows: 0, ..center(&["x"], &["y"]) });
        assert!(center_sections(&center, Mode::Grid).is_empty());
    }

    #[test]
    fn more_center_items_than_slots_are_cut_rather_than_growing_the_block() {
        let many: Vec<&str> = vec!["a:1", "b:2", "c:3", "d:4", "e:5", "f:6"];
        let center = build_block(&center(&many, &[]));
        let out = center_sections(&center, Mode::Grid);
        assert_eq!(out[0].items.len(), 4);
        // And the section still says how many there really were, which is what
        // "more tiles" in edit mode has to stop at.
        assert_eq!(out[0].total, 6);
    }

    #[test]
    fn what_the_block_holds_is_named_by_target_and_by_app() {
        let center = build_block(&center(&["ms-settings:display"], &["https://example.com"]));
        let (targets, _) = center.held();
        assert!(targets.contains(&"ms-settings:display".to_owned()));
        assert!(targets.contains(&"https://example.com".to_owned()));
    }

    #[test]
    fn an_empty_slot_is_its_own_group_so_a_drag_cannot_run_into_one() {
        let out = center_sections(&build_block(&center(&["ms-settings:display"], &[])), Mode::Grid);
        let filled = &out[0].items[0];
        let empty = &out[0].items[1];
        assert_ne!(filled.group, empty.group);
    }

    #[test]
    fn a_urls_origin_is_spelled_the_way_the_extension_spells_it() {
        // The favicon key. One bitmap serves every page on a site, so a site
        // favorite and the tab it came from have to land on the same string.
        assert_eq!(
            origin_of("https://Docs.RS/serde/latest?x=1#frag").as_deref(),
            Some("https://docs.rs")
        );
        assert_eq!(
            origin_of("http://localhost:3000/").as_deref(),
            Some("http://localhost:3000")
        );
        // Not a page, so there is no favicon to look for.
        assert_eq!(origin_of("ms-settings:display"), None);
        assert_eq!(origin_of(r"C:\Windows\notepad.exe"), None);
        assert_eq!(origin_of("file:///R:/dev"), None);
        assert_eq!(origin_of("https://"), None);
    }

    #[test]
    fn a_site_favorite_keeps_the_shell_icon_until_a_favicon_turns_up() {
        // Nothing paired in a test, so this is the no-browser case: the URL
        // itself, which the shell answers with the default browser's logo.
        let center = build_block(&center(&[], &["https://example.com"]));
        let out = center_sections(&center, Mode::Grid);
        assert_eq!(out[1].items[0].icon_source.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn every_mode_gets_a_tile_that_says_what_it_is() {
        let tiles = mode_items();
        assert_eq!(tiles.len(), MODES.len());
        for (tile, mode) in tiles.iter().zip(MODES) {
            assert_eq!(tile.target, Target::Mode(mode));
            assert!(!tile.title.is_empty(), "{mode:?} has no words");
        }
        // Move leads: it is the one that used to be seven tiles of its own.
        assert_eq!(MODES[0], Mode::Move);
    }
}
