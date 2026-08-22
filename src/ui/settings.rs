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

use crate::config::Config;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Tiles,
    Labels,
    Columns,
    Browser,
    /// The escape hatch: everything this surface does not cover.
    OpenFile,
    Done,
}

pub const SETTINGS: [Setting; 6] = [
    Setting::Tiles,
    Setting::Labels,
    Setting::Columns,
    Setting::Browser,
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
            Setting::Browser => Some(Change::Browser(!config.browser.enabled)),
            Setting::OpenFile | Setting::Done => None,
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
