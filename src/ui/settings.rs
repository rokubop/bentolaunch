//! The settings surface: the config's clickable knobs, as squares.
//!
//! Same squares as the edit options and the big menu, for the same reason. A
//! settings dialog is checkboxes, dropdowns and a slider or two, and every one
//! of those is a small target that has to be hit precisely. This app is aimed
//! at with a gaze pointer, so every setting here is a full tile and every click
//! means the same thing: step to the next value.
//!
//! Only what a click can say lives here. The hotkey, the theme colours and the
//! sections themselves stay in the file: they need typing, and a square that
//! opened a text field would be a worse text editor than the one the user
//! already has. `Open the file` is one of the squares for exactly that reason -
//! this surface is the common half, not a replacement.

use crate::config::{Config, Contents, Favorites};
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
    (5, "5 across"),
    (7, "7 across"),
    (9, "9 across"),
    (12, "12 across"),
    (0, "As many as fit"),
];

/// The centre block's shape, in tiles a half: across, then down.
///
/// Off is the first step rather than a square of its own. "Do I want a centre
/// block" and "how big" are the same question asked twice, and answering it
/// with two squares means two places for them to disagree. Both numbers on the
/// one square for the same reason: a block is a shape, and a height and a width
/// set apart is a file that briefly says 3 x 1 - which the watcher lays out.
///
/// Stops at four each way, which is where `Config::validated` stops: a block
/// wider than that leaves the grid around it nowhere to wrap to.
const CENTER_SIZES: [(usize, usize, &str); 6] = [
    (0, 0, "Center \u{00B7} off"),
    (2, 1, "Center \u{00B7} 2 \u{00D7} 1"),
    (2, 2, "Center \u{00B7} 2 \u{00D7} 2"),
    (3, 2, "Center \u{00B7} 3 \u{00D7} 2"),
    (3, 3, "Center \u{00B7} 3 \u{00D7} 3"),
    (4, 4, "Center \u{00B7} 4 \u{00D7} 4"),
];

/// What the block holds, and whether the two lists are kept apart. One square
/// stepping through all four, because they are four answers to one question.
/// The settings square says the whole thing; the edit-layout square has the
/// block right there beside it and only needs the answer.
const CENTER_CONTENTS: [(Contents, &str, &str); 4] = [
    (Contents::Split, "Center \u{00B7} apps + sites", "Apps + sites"),
    (Contents::One, "Center \u{00B7} one block", "One block"),
    (Contents::Apps, "Center \u{00B7} apps only", "Apps only"),
    (Contents::Sites, "Center \u{00B7} sites only", "Sites only"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Tiles,
    Labels,
    Columns,
    /// The centre block's shape, off included.
    Center,
    /// Which lists the block holds: apps and sites apart, both in one block,
    /// or one of them alone.
    CenterHolds,
    Browser,
    /// The escape hatch: everything this surface does not cover.
    OpenFile,
    Done,
}

/// Eight, which is two full rows of four. The shape settings first, then the
/// centre, then the one switch, then the two squares that are not values.
pub const SETTINGS: [Setting; 8] = [
    Setting::Tiles,
    Setting::Labels,
    Setting::Columns,
    Setting::Browser,
    Setting::Center,
    Setting::CenterHolds,
    Setting::OpenFile,
    Setting::Done,
];

impl Setting {
    /// The big mark on the square.
    ///
    /// Static, unlike the edit options': these say what the square is, and the
    /// value is said in words underneath. A glyph that changed with the value
    /// would be two things to read instead of one.
    ///
    /// Geometric shapes rather than emoji. These are drawn in the UI font like
    /// every other option, and anything with an emoji presentation comes back
    /// in colour out of a different font - one bright pictogram among five line
    /// drawings reads as the odd one out rather than as a set.
    pub fn glyph(self) -> &'static str {
        match self {
            // A square inside a square: the tile, and how much of it there is.
            Setting::Tiles => "\u{25A3}",
            // Lines of text, then columns of them. A pair on purpose: they are
            // the two shape settings and they sit next to each other.
            Setting::Labels => "\u{25A4}",
            Setting::Columns => "\u{25A5}",
            // A square with its middle filled: the block, and the panel around
            // it. Then the same square cut down the middle, which is what the
            // split does to it.
            Setting::Center => "\u{25FB}",
            Setting::CenterHolds => "\u{25EB}",
            Setting::Browser => "\u{25EF}",
            // The same mark the menu puts on "Add file". It is the same kind of
            // thing: a file, opened.
            Setting::OpenFile => "\u{1F5CE}",
            Setting::Done => "\u{2713}",
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
            Setting::Center => match center_now(config) {
                Some(index) => CENTER_SIZES[index].2,
                None => "Center \u{00B7} as set",
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
            Setting::Center => {
                let index = center_now(config).map_or(0, |i| (i + 1) % CENTER_SIZES.len());
                let (columns, rows, _) = CENTER_SIZES[index];
                Some(Change::CenterSize { columns, rows })
            }
            Setting::CenterHolds => {
                let index = (holds_now(config) + 1) % CENTER_CONTENTS.len();
                Some(Change::CenterContents(CENTER_CONTENTS[index].0))
            }
            Setting::Browser => Some(Change::Browser(!config.browser.enabled)),
            Setting::OpenFile | Setting::Done => None,
        }
    }

    /// Whether this square does anything where the config currently stands.
    ///
    /// Greyed rather than removed, the same rule the edit options follow: the
    /// squares must never reshuffle under the pointer.
    pub fn applies(self, config: &Config) -> bool {
        match self {
            // Nothing to fill when there is no block.
            Setting::CenterHolds => config.favorites.on(),
            _ => true,
        }
    }
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

/// Which shape the block is on, or `None` for one typed into the file that is
/// not on the list.
///
/// Off is off however it was written: `rows = 0` with a width still set is the
/// same block as no block, and a square that called that "as set" would take
/// two clicks to turn anything on.
/// The most tiles the block may take either way. `Config::validated` stops
/// here: wider than this and the grid around it has nowhere to wrap to.
pub const CENTER_MOST: usize = 4;

/// Step the block's shape one tile in one direction.
///
/// The two directions apart, because a block is a shape: three columns of
/// favorites with one row of them is a real answer, and a single list of
/// presets cannot give it. Clamped rather than wrapped - a Wider that came
/// back round to one column is a button that undoes itself.
pub fn center_resize(config: &Config, across: isize, down: isize) -> Option<Change> {
    let f = &config.favorites;
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
    let f = &config.favorites;
    match f.on() {
        true => Change::CenterSize { columns: f.columns, rows: 0 },
        false => Change::CenterSize {
            columns: f.columns.clamp(1, CENTER_MOST),
            rows: Favorites::default().rows,
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

fn center_now(config: &Config) -> Option<usize> {
    let f = &config.favorites;
    if !f.on() {
        return Some(0);
    }
    CENTER_SIZES
        .iter()
        .position(|(cols, rows, _)| *cols == f.columns && *rows == f.rows)
}

fn holds_now(config: &Config) -> usize {
    CENTER_CONTENTS
        .iter()
        .position(|(c, ..)| *c == config.favorites.contents)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::default()
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
    fn the_centre_square_steps_through_its_shapes_and_wraps_to_off() {
        let mut c = config();
        c.favorites.columns = 4;
        c.favorites.rows = 4;
        assert_eq!(Setting::Center.label(&c), "Center \u{00B7} 4 \u{00D7} 4");
        assert_eq!(
            Setting::Center.next(&c),
            Some(Change::CenterSize { columns: 0, rows: 0 })
        );

        c.favorites.columns = 0;
        c.favorites.rows = 0;
        assert_eq!(Setting::Center.label(&c), "Center \u{00B7} off");
        assert_eq!(
            Setting::Center.next(&c),
            Some(Change::CenterSize { columns: 2, rows: 1 })
        );
    }

    #[test]
    fn the_default_block_is_a_shape_the_square_can_name() {
        assert_eq!(Setting::Center.label(&config()), "Center \u{00B7} 3 \u{00D7} 3");
    }

    #[test]
    fn a_block_switched_off_with_a_width_still_set_reads_as_off() {
        // How the square leaves it: turning the block off writes both numbers,
        // and a hand-edited `rows = 0` beside a width has to mean the same
        // thing or the square takes two clicks to turn anything back on.
        let mut c = config();
        c.favorites.rows = 0;
        assert_eq!(Setting::Center.label(&c), "Center \u{00B7} off");
        assert_eq!(
            Setting::Center.next(&c),
            Some(Change::CenterSize { columns: 2, rows: 1 })
        );
    }

    #[test]
    fn a_centre_shape_off_the_list_is_named_not_snapped() {
        let mut c = config();
        c.favorites.columns = 1;
        c.favorites.rows = 4;
        assert_eq!(Setting::Center.label(&c), "Center \u{00B7} as set");
        assert_eq!(
            Setting::Center.next(&c),
            Some(Change::CenterSize { columns: 0, rows: 0 })
        );
    }

    #[test]
    fn what_the_centre_holds_steps_through_all_four_and_wraps() {
        let mut c = config();
        c.favorites.contents = Contents::Split;
        assert_eq!(Setting::CenterHolds.label(&c), "Center \u{00B7} apps + sites");
        assert_eq!(
            Setting::CenterHolds.next(&c),
            Some(Change::CenterContents(Contents::One))
        );

        c.favorites.contents = Contents::Sites;
        assert_eq!(Setting::CenterHolds.label(&c), "Center \u{00B7} sites only");
        assert_eq!(
            Setting::CenterHolds.next(&c),
            Some(Change::CenterContents(Contents::Split))
        );
    }

    #[test]
    fn the_contents_square_greys_out_when_there_is_no_block_to_fill() {
        let mut c = config();
        c.favorites.rows = 0;
        assert!(!Setting::CenterHolds.applies(&c));
        // And every other square still applies, so nothing else went grey with it.
        for setting in SETTINGS.iter().filter(|s| **s != Setting::CenterHolds) {
            assert!(setting.applies(&c), "{setting:?} went grey");
        }
    }

    #[test]
    fn every_centre_shape_survives_the_configs_own_range_check() {
        for (columns, rows, label) in CENTER_SIZES {
            let mut c = config();
            c.favorites.columns = columns;
            c.favorites.rows = rows;
            let checked = c.validated();
            assert_eq!(checked.favorites.rows, rows, "{label} lost its height");
            assert_eq!(checked.favorites.columns, columns, "{label} lost its width");
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
