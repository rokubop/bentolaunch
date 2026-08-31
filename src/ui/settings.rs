//! Settings as squares, because a dialog is small targets and this app is
//! aimed at with a gaze pointer. Every click steps to the next value.
//!
//! Only what a click can say. Anything needing typing stays in the file, which
//! `Open the file` is here to reach.

use crate::config::{Config, Contents, Center};
use crate::pins::Change;

/// Tile sizes, smallest first: width, height, the strip the label gets, and
/// what the square says while it is on that one.
///
/// Presets rather than a slider. Four steps cover the range anyone actually
/// wants, and each is one click from the next. The label strip grows with the
/// tile because a fixed strip on a large tile strands the title in white space.
const TILE_SIZES: [(f32, f32, f32, &str); 4] = [
    (100.0, 76.0, 20.0, "Tiles \u{00B7} small"),
    (140.0, 100.0, 24.0, "Tiles \u{00B7} medium"),
    (180.0, 128.0, 28.0, "Tiles \u{00B7} large"),
    (220.0, 156.0, 32.0, "Tiles \u{00B7} huge"),
];

/// Column caps. 0 is the config's "no cap beyond what fits the screen".
const COLUMNS: [(usize, &str); 5] = [
    (5, "Columns \u{00B7} 5"),
    (7, "Columns \u{00B7} 7"),
    (9, "Columns \u{00B7} 9"),
    (12, "Columns \u{00B7} 12"),
    (0, "Columns \u{00B7} as many as fit"),
];

/// What the block holds, and whether the two lists are kept apart. One square
/// stepping through all four, because they are four answers to one question.
/// The settings square says the whole thing; the edit-layout square has the
/// block right there beside it and only needs the answer.
const CENTER_CONTENTS: [(Contents, &str, &str); 4] = [
    (Contents::Split, "Center holds \u{00B7} apps + sites", "Apps + sites"),
    (Contents::One, "Center holds \u{00B7} one block", "One block"),
    (Contents::Apps, "Center holds \u{00B7} apps only", "Apps only"),
    (Contents::Sites, "Center holds \u{00B7} sites only", "Sites only"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Tiles,
    Labels,
    Columns,
    /// Which lists the block holds: apps and sites apart, both in one block,
    /// or one of them alone.
    CenterHolds,
    Browser,
    /// The escape hatch: everything this surface does not cover.
    OpenFile,
    /// Layout back to stock. Asks first, on a surface of its own.
    Reset,
    /// The two answers to that question. Their own squares on their own
    /// surface, not a label the Reset square swaps in: a square that turned
    /// into a confirm under the pointer is one nobody notices has changed, and
    /// a second dwell in the same place would answer it.
    ResetNo,
    ResetYes,
    Done,
}

/// What Reset opens: the eight squares gone, two answers in their place. An
/// in-place confirm changed one word and nobody saw it. Keeping it comes first,
/// and neither answer stands where Reset did, so a stray second click misses.
pub const CONFIRM_RESET: [Setting; 2] = [Setting::ResetNo, Setting::ResetYes];

/// Two rows of four. The block's size is not here: it needs two directions,
/// and this surface covers the thing being sized. Reset sits one square from
/// Done, the two worst to confuse.
pub const SETTINGS: [Setting; 8] = [
    Setting::Tiles,
    Setting::Labels,
    Setting::Columns,
    Setting::Browser,
    Setting::CenterHolds,
    Setting::Reset,
    Setting::OpenFile,
    Setting::Done,
];

impl Setting {
    /// The mark on the square. Static: the value is said in words underneath.
    ///
    /// Icon font, never the UI face - almost none of these exist in it, so
    /// Unicode shapes silently mixed two typefaces at two weights.
    pub fn glyph(self) -> &'static str {
        match self {
            // Few big cells against many small ones: the two shape settings,
            // side by side and telling each other apart.
            Setting::Tiles => "\u{E8A9}",
            Setting::Labels => "\u{E8FD}",
            Setting::Columns => "\u{E9A6}",
            Setting::Browser => "\u{E774}",
            // A box split across the middle: the block, and the two lists in
            // it. The same mark the edit square for this wears.
            Setting::CenterHolds => "\u{E745}",
            Setting::OpenFile => "\u{E8E5}",
            // Undo, not refresh. Anticlockwise is back to where this started;
            // the clockwise one is what every app on the machine reloads with.
            Setting::Reset => "\u{E7A7}",
            Setting::ResetYes => "\u{E7A7}",
            Setting::ResetNo => "\u{E711}",
            Setting::Done => "\u{E73E}",
        }
    }

    /// The setting and the value it is on now, in one line.
    ///
    /// A gaze pointer settles long enough to read a line, and the value has to
    /// be on the square itself: there is nowhere else to show it, and a knob
    /// that does not say where it is takes a click to find out.
    pub fn label(self, config: &Config) -> &'static str {
        match self {
            Setting::Tiles => TILE_SIZES[tiles_now(config)].3,
            Setting::Labels => {
                if config.grid.show_detail {
                    "Labels \u{00B7} name + detail"
                } else {
                    "Labels \u{00B7} name only"
                }
            }
            Setting::Columns => match columns_now(config) {
                Some(index) => COLUMNS[index].1,
                // A width hand-edited to something off the list. Named rather
                // than snapped, so opening this surface never silently moves a
                // setting the user chose deliberately.
                None => "Columns \u{00B7} as set",
            },
            Setting::CenterHolds => CENTER_CONTENTS[holds_now(config)].1,
            Setting::Browser => {
                if config.browser.enabled {
                    "Browser \u{00B7} on"
                } else {
                    "Browser \u{00B7} off"
                }
            }
            Setting::OpenFile => "Open the file",
            // The question is asked on the square, not in a dialog. A
            // message box is two small buttons handed the focus, which is
            // the one shape nothing pointing with gaze can use.
            Setting::Reset => "Reset layout",
            Setting::ResetYes => "Reset the layout",
            Setting::ResetNo => "Keep my layout",
            Setting::Done => "Done",
        }
    }

    /// What one click writes. `None` for the two squares that are not values.
    pub fn next(self, config: &Config) -> Option<Change> {
        match self {
            Setting::Tiles => {
                let (width, height, label, _) =
                    TILE_SIZES[(tiles_now(config) + 1) % TILE_SIZES.len()];
                Some(Change::Tiles { width, height, label })
            }
            Setting::Labels => Some(Change::ShowDetail(!config.grid.show_detail)),
            Setting::Columns => {
                // Off the list starts at the top of it, rather than guessing
                // which neighbour the user meant.
                let index = columns_now(config).map_or(0, |i| (i + 1) % COLUMNS.len());
                Some(Change::MaxColumns(COLUMNS[index].0))
            }
            Setting::CenterHolds => {
                let index = (holds_now(config) + 1) % CENTER_CONTENTS.len();
                Some(Change::CenterContents(CENTER_CONTENTS[index].0))
            }
            Setting::Browser => Some(Change::Browser(!config.browser.enabled)),
            Setting::OpenFile | Setting::Reset | Setting::Done => None,
            Setting::ResetNo | Setting::ResetYes => None,
        }
    }

    /// Whether this square does anything where the config currently stands.
    ///
    /// Greyed rather than removed, the same rule the edit options follow: the
    /// squares must never reshuffle under the pointer.
    pub fn applies(self, config: &Config) -> bool {
        match self {
            // Nothing to fill when there is no block.
            Setting::CenterHolds => config.center.on(),
            // Nothing to put back when nothing has been moved.
            Setting::Reset => !layout_is_stock(config),
            _ => true,
        }
    }
}

/// Whether resetting would change anything: exactly the keys
/// `pins::reset_layout` writes, and nothing else.
///
/// What a box holds is not compared, and neither is a box the user wrote - a
/// reset keeps both. Comparing the whole list said "not stock" forever.
pub fn layout_is_stock(config: &Config) -> bool {
    let stock = Config::default();
    let block = |c: &Config| (c.center.rows, c.center.columns, c.center.contents);
    if config.grid != stock.grid || block(config) != block(&stock) {
        return false;
    }
    let (mine, theirs) = (boxes(config), boxes(&stock));
    let Some((head, extra)) = mine.split_at_checked(theirs.len()) else {
        return false;
    };
    // The stock boxes first, exactly. Then anything else, which a reset leaves
    // where it is - unless it answers to a stock title, which a reset would
    // fold into the stock box of that name.
    head == theirs && extra.iter().all(|kept| !theirs.iter().any(|s| s.title == kept.title))
}

/// One box, as the layout sees it: what it is called, and where it goes.
struct Placed<'a> {
    title: &'a str,
    side: Option<&'a str>,
    columns: usize,
    max_items: usize,
}

impl PartialEq for Placed<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
            && self.side == other.side
            && self.columns == other.columns
            && self.max_items == other.max_items
    }
}

/// Every box, in file order - which is the order down a lane, so the list
/// itself is part of the layout.
fn boxes(config: &Config) -> Vec<Placed<'_>> {
    config
        .sections
        .iter()
        .map(|s| Placed {
            title: s.title.as_str(),
            side: s.side.as_deref(),
            columns: s.columns,
            max_items: s.max_items,
        })
        .collect()
}

/// Nearest preset by width. Nearest rather than exact: a size typed into the
/// file sits between two of these, and the square still has to say something.
fn tiles_now(config: &Config) -> usize {
    let w = config.grid.tile_width;
    TILE_SIZES
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (a.0 - w).abs().total_cmp(&(b.0 - w).abs()))
        .map_or(1, |(index, _)| index)
}

fn columns_now(config: &Config) -> Option<usize> {
    COLUMNS
        .iter()
        .position(|(n, _)| *n == config.grid.max_columns)
}

/// The most tiles the block may take either way. One number, in `config`,
/// where `validated` clamps to it and `shape_for` grows up to it.
pub const CENTER_MOST: usize = Center::MOST;

/// Step the block's shape one tile in one direction.
///
/// The two directions apart, because a block is a shape: three columns of
/// center with one row of them is a real answer, and a single list of
/// presets cannot give it. Clamped rather than wrapped - a Wider that came
/// back round to one column is a button that undoes itself.
pub fn center_resize(config: &Config, across: isize, down: isize) -> Option<Change> {
    let f = &config.center;
    let step = |now: usize, by: isize| {
        now.checked_add_signed(by).filter(|n| (1..=CENTER_MOST).contains(n))
    };
    let columns = step(f.columns, across)?;
    let rows = step(f.rows, down)?;
    (columns != f.columns || rows != f.rows).then_some(Change::CenterSize { columns, rows })
}

/// Switch the block off, or back on at the shape it last had.
///
/// Off is `rows = 0`, and the columns are left alone so turning it back on
/// gives back the block that was there rather than a default one.
pub fn center_toggle(config: &Config) -> Change {
    let f = &config.center;
    match f.on() {
        true => Change::CenterSize { columns: f.columns, rows: 0 },
        false => Change::CenterSize {
            columns: f.columns.clamp(1, CENTER_MOST),
            rows: Center::default().rows,
        },
    }
}

/// Step what the block holds, wrapping. Four answers to one question.
pub fn center_holds_next(config: &Config) -> Change {
    let index = (holds_now(config) + 1) % CENTER_CONTENTS.len();
    Change::CenterContents(CENTER_CONTENTS[index].0)
}

/// What the block is holding, said short. The block is right beside the square
/// that says it, so it does not need to name itself again.
pub fn center_holds_said(config: &Config) -> &'static str {
    CENTER_CONTENTS[holds_now(config)].2
}


fn holds_now(config: &Config) -> usize {
    CENTER_CONTENTS
        .iter()
        .position(|(c, ..)| *c == config.center.contents)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn every_mark_comes_from_the_icon_font() {
        // Reaching for a Unicode shape silently mixed two typefaces at two
        // weights: eight of the marks were in the UI face and the rest fell
        // back to Segoe UI Symbol, where the folder and the file are the same
        // blank rectangle. The private use area is the icon set and nothing
        // else, so this is the whole rule.
        for setting in SETTINGS.iter().chain(CONFIRM_RESET.iter()) {
            let mark = setting.glyph();
            assert!(!mark.is_empty(), "{setting:?} has no mark");
            for c in mark.chars() {
                assert!(
                    ('\u{E000}'..='\u{F8FF}').contains(&c),
                    "{setting:?} reaches outside the icon font: {mark:?}",
                );
            }
        }
    }

    #[test]
    fn no_two_squares_wear_the_same_mark() {
        // "Add file" and "Open the file" were the same rectangle, on two
        // surfaces one click apart.
        let mut seen: Vec<&str> = Vec::new();
        for setting in SETTINGS {
            let mark = setting.glyph();
            assert!(!seen.contains(&mark), "{setting:?} wears a mark already taken");
            seen.push(mark);
        }
    }

    #[test]
    fn every_value_square_names_its_own_setting() {
        // "5 across" said nothing about which square it was, and two squares
        // both opening "Center \" said different things - the shape and what
        // it holds. The prefix has to name the setting, and name only it.
        let c = config();
        let mut seen: Vec<&str> = Vec::new();
        for setting in SETTINGS {
            let label = setting.label(&c);
            let Some(_) = setting.next(&c) else {
                assert!(
                    !label.contains('\u{00B7}'),
                    "{setting:?} does nothing on a click, so it is not a value: {label}",
                );
                continue;
            };
            let name = label
                .split_once(" \u{00B7} ")
                .unwrap_or_else(|| panic!("{setting:?} does not say its setting: {label}"))
                .0;
            assert!(!seen.contains(&name), "two squares both call themselves {name:?}");
            seen.push(name);
        }
    }

    #[test]
    fn the_reset_square_says_one_thing_whether_it_applies_or_not() {
        // It is an action, not a value. Saying "Layout \ stock" when it would
        // do nothing borrowed the value squares' grammar for the one square
        // that has no value - and greying already says "this would do nothing"
        // everywhere else on the surface.
        let mut moved = config();
        moved.grid.max_columns = 12;
        let stock = Config::default();
        assert!(!layout_is_stock(&moved));
        assert!(layout_is_stock(&stock));

        assert_eq!(Setting::Reset.label(&moved), Setting::Reset.label(&stock));
        assert!(Setting::Reset.applies(&moved));
        assert!(!Setting::Reset.applies(&stock));
    }

    #[test]
    fn reset_does_not_sit_beside_done() {
        // The two worst squares to confuse: throw my layout away, and I am
        // finished. A mis-aim between them costs a trip through the question.
        let at = |want| SETTINGS.iter().position(|s| *s == want).unwrap();
        assert!(at(Setting::Done) - at(Setting::Reset) > 1);
    }

    #[test]
    fn the_default_tile_size_reads_as_the_preset_it_is() {
        assert_eq!(Setting::Tiles.label(&config()), "Tiles \u{00B7} medium");
    }

    #[test]
    fn a_hand_typed_size_lands_on_the_nearest_preset_rather_than_the_first() {
        let mut c = config();
        c.grid.tile_width = 175.0;
        assert_eq!(Setting::Tiles.label(&c), "Tiles \u{00B7} large");
    }

    #[test]
    fn clicking_tiles_steps_up_and_wraps_at_the_top() {
        let mut c = config();
        c.grid.tile_width = 220.0;
        assert_eq!(
            Setting::Tiles.next(&c),
            Some(Change::Tiles { width: 100.0, height: 76.0, label: 20.0 })
        );
    }

    #[test]
    fn every_preset_survives_the_configs_own_range_check() {
        for (width, height, label, _) in TILE_SIZES {
            let mut c = config();
            c.grid.tile_width = width;
            c.grid.tile_height = height;
            c.grid.label_height = label;
            let checked = c.validated();
            assert_eq!(checked.grid.tile_width, width);
            assert_eq!(checked.grid.tile_height, height);
            assert_eq!(checked.grid.label_height, label);
        }
    }

    #[test]
    fn a_column_count_off_the_list_is_named_not_snapped() {
        let mut c = config();
        c.grid.max_columns = 6;
        assert_eq!(Setting::Columns.label(&c), "Columns \u{00B7} as set");
        assert_eq!(Setting::Columns.next(&c), Some(Change::MaxColumns(5)));
    }

    #[test]
    fn the_two_squares_that_are_not_values_write_nothing() {
        assert_eq!(Setting::OpenFile.next(&config()), None);
        assert_eq!(Setting::Done.next(&config()), None);
    }





    #[test]
    fn what_the_centre_holds_steps_through_all_four_and_wraps() {
        let mut c = config();
        c.center.contents = Contents::Split;
        assert_eq!(Setting::CenterHolds.label(&c), "Center holds \u{00B7} apps + sites");
        assert_eq!(
            Setting::CenterHolds.next(&c),
            Some(Change::CenterContents(Contents::One))
        );

        c.center.contents = Contents::Sites;
        assert_eq!(Setting::CenterHolds.label(&c), "Center holds \u{00B7} sites only");
        assert_eq!(
            Setting::CenterHolds.next(&c),
            Some(Change::CenterContents(Contents::Split))
        );
    }

    #[test]
    fn the_contents_square_greys_out_when_there_is_no_block_to_fill() {
        let mut c = config();
        c.center.rows = 0;
        assert!(!Setting::CenterHolds.applies(&c));
        // And every other square still applies, so nothing else went grey with
        // it. Reset is not one of them: it greys on a stock layout, which this
        // config is, and that is its own rule rather than the block's.
        let others = SETTINGS
            .iter()
            .filter(|s| ![Setting::CenterHolds, Setting::Reset].contains(s));
        for setting in others {
            assert!(setting.applies(&c), "{setting:?} went grey");
        }
    }

    #[test]
    fn the_question_is_a_surface_of_its_own() {
        assert_eq!(CONFIRM_RESET, [Setting::ResetNo, Setting::ResetYes]);
        // Answers live only on the question. On the settings surface they would
        // be two squares answering a question nobody asked.
        for answer in CONFIRM_RESET {
            assert!(!SETTINGS.contains(&answer), "{answer:?} is on the settings surface");
            assert!(answer.next(&config()).is_none(), "{answer:?} writes a setting");
            assert!(!answer.glyph().is_empty() && !answer.label(&config()).is_empty());
        }
        // Neither is ever dim. A greyed answer is a question with no way out.
        let mut stock = config();
        stock.center.rows = 0;
        for on in [config(), stock] {
            for answer in CONFIRM_RESET {
                assert!(answer.applies(&on), "{answer:?} went grey");
            }
        }
    }

    #[test]
    fn no_answer_sits_where_the_reset_square_was() {
        // The whole reason the question is its own surface: a confirm under the
        // pointer is one a second click - or a second dwell, which is how this
        // is aimed - answers by accident.
        use crate::ui::grid::{Rect, centred_grid};
        let panel = Rect { x: 0.0, y: 0.0, w: 1376.0, h: 632.0 };
        let (w, h, gap) = (140.0, 100.0, 10.0);

        let squares = centred_grid(panel, SETTINGS.len(), w, h, gap);
        let reset = squares[SETTINGS.iter().position(|s| *s == Setting::Reset).unwrap()];
        let (x, y) = (reset.x + reset.w / 2.0, reset.y + reset.h / 2.0);

        for answer in centred_grid(panel, CONFIRM_RESET.len(), w, h, gap) {
            let hit = x >= answer.x
                && x < answer.x + answer.w
                && y >= answer.y
                && y < answer.y + answer.h;
            assert!(!hit, "an answer covers the square that asked");
        }
    }



    #[test]
    fn each_toggle_says_the_state_it_is_in_and_writes_the_other_one() {
        let mut c = config();
        c.grid.show_detail = false;
        assert_eq!(Setting::Labels.label(&c), "Labels \u{00B7} name only");
        assert_eq!(Setting::Labels.next(&c), Some(Change::ShowDetail(true)));

        c.browser.enabled = false;
        assert_eq!(Setting::Browser.label(&c), "Browser \u{00B7} off");
        assert_eq!(Setting::Browser.next(&c), Some(Change::Browser(true)));
    }
}
