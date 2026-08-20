//! Grid layout. Pure arithmetic, no Windows types, so the sizing rules are
//! testable without a monitor.
//!
//! The rule (DESIGN.md "Resolved"): tile size is fixed and never changes. The
//! panel grows outward from the center of the work area as items are added,
//! until it reaches `max_screen_fraction` of that work area. Past that it stops
//! widening, and further rows scroll.
//!
//! Sections stack top to bottom, each under its own header, and all of them
//! share one column count so tiles line up down the whole panel.
//!
//! Tile rectangles are computed once, up front, in *content space* (the full
//! scrollable height). Everything else — drawing, hit-testing, scrolling — is
//! that list minus the scroll offset. Cheaper to reason about than recovering a
//! row and column from a point across variable-height sections.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn shifted(self, dy: f32) -> Rect {
        Rect { y: self.y - dy, ..self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub tile_w: f32,
    pub tile_h: f32,
    pub gap: f32,
    pub padding: f32,
    pub max_fraction: f32,
    /// Hard cap on columns. 0 means "whatever fits".
    pub max_cols: usize,
    /// Exact column count, 0 to derive it. Only filtering sets it, so the
    /// panel cannot change width per keystroke. Still bounded by the screen.
    pub fixed_cols: usize,
    pub header_h: f32,
    pub section_gap: f32,
    /// Filter strip above the grid, 0 when not filtering. Does not scroll: it
    /// is what explains why most of the grid is missing.
    pub search_h: f32,
}

/// What the layout needs to know about a section: its label and how many tiles.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionShape {
    pub title: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub title: String,
    /// Content space.
    pub rect: Rect,
}

/// One rendered section's slice of the grid.
///
/// Bands tile the panel with no gaps: each one runs from where the previous
/// ended down to its own last row, so every point in the panel belongs to
/// exactly one section. Dropping needs that — something landing in the gap above
/// a section should still mean *that* section, not nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    /// Index into the sections handed to `compute`.
    pub section: usize,
    /// Flat index of this section's first tile.
    pub first: usize,
    pub count: usize,
    /// Content space, spanning the header and every row of tiles.
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub cols: usize,
    /// Panel rect in screen coordinates.
    pub panel: Rect,
    /// Height the content wants, which may exceed `panel.h`.
    pub content_h: f32,
    /// 0.0 when everything fits.
    pub max_scroll: f32,
    /// One per item, flattened across sections in order. Content space.
    tiles: Vec<Rect>,
    headers: Vec<Header>,
    bands: Vec<Band>,
    metrics: Metrics,
}

impl Layout {
    pub fn compute(sections: &[SectionShape], m: Metrics, work_area: Rect) -> Layout {
        let max_w = (work_area.w * m.max_fraction).max(m.tile_w + 2.0 * m.padding);
        let max_h = (work_area.h * m.max_fraction).max(m.tile_h + 2.0 * m.padding);

        // How many tiles fit across the widest panel we allow.
        let usable = (max_w - 2.0 * m.padding + m.gap).max(m.tile_w);
        let fits = (usable / (m.tile_w + m.gap)).floor().max(1.0) as usize;
        // A row longer than this stops being scannable in one look, however wide
        // the monitor is.
        let capped = if m.max_cols == 0 { fits } else { fits.min(m.max_cols) };

        // One column count for the whole panel, driven by the busiest section.
        let widest = sections.iter().map(|s| s.count).max().unwrap_or(0);
        let cols = if m.fixed_cols > 0 {
            m.fixed_cols.clamp(1, capped)
        } else {
            widest.clamp(1, capped)
        };

        let panel_w = cols as f32 * m.tile_w + (cols - 1) as f32 * m.gap + 2.0 * m.padding;

        let mut tiles = Vec::new();
        let mut headers = Vec::new();
        let mut bands: Vec<Band> = Vec::new();
        let mut y = m.search_h + m.padding;

        for (index, section) in sections.iter().enumerate() {
            if section.count == 0 {
                continue;
            }
            let band_top = y;
            let first_tile = tiles.len();
            if index > 0 && !tiles.is_empty() {
                y += m.section_gap;
            }
            if !section.title.is_empty() && m.header_h > 0.0 {
                headers.push(Header {
                    title: section.title.clone(),
                    rect: Rect {
                        x: m.padding,
                        y,
                        w: panel_w - 2.0 * m.padding,
                        h: m.header_h,
                    },
                });
                y += m.header_h;
            }

            let rows = section.count.div_ceil(cols);
            for slot in 0..section.count {
                let col = slot % cols;
                let row = slot / cols;
                tiles.push(Rect {
                    x: m.padding + col as f32 * (m.tile_w + m.gap),
                    y: y + row as f32 * (m.tile_h + m.gap),
                    w: m.tile_w,
                    h: m.tile_h,
                });
            }
            y += rows as f32 * m.tile_h + (rows.saturating_sub(1)) as f32 * m.gap;

            bands.push(Band {
                section: index,
                first: first_tile,
                count: section.count,
                // Width and the final height are filled in below, once the panel
                // width and the following band's top are both known.
                rect: Rect { x: 0.0, y: band_top, w: panel_w, h: y - band_top },
            });
        }

        let content_h = y + m.padding;
        let panel_h = content_h.min(max_h);

        // Stretch bands over the padding and gaps so they cover the panel with
        // nothing in between. The strip is chrome, so a drop on it hits nobody.
        for i in 0..bands.len() {
            let top = if i == 0 { m.search_h } else { bands[i].rect.y };
            let bottom = bands.get(i + 1).map_or(content_h, |next| next.rect.y);
            bands[i].rect.y = top;
            bands[i].rect.h = bottom - top;
        }

        // Centered on the work area, snapped to whole pixels so tiles stay crisp.
        let panel = Rect {
            x: (work_area.x + (work_area.w - panel_w) / 2.0).round(),
            y: (work_area.y + (work_area.h - panel_h) / 2.0).round(),
            w: panel_w.round(),
            h: panel_h.round(),
        };

        Layout {
            cols,
            panel,
            content_h,
            max_scroll: (content_h - panel_h).max(0.0),
            tiles,
            headers,
            bands,
            metrics: m,
        }
    }

    #[cfg(test)]
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Tile rect in panel-local coordinates, with `scroll` applied. May be
    /// partly or wholly outside the panel when scrolled.
    pub fn tile_rect(&self, index: usize, scroll: f32) -> Rect {
        self.tiles
            .get(index)
            .copied()
            .unwrap_or(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 })
            .shifted(scroll)
    }

    pub fn headers(&self, scroll: f32) -> impl Iterator<Item = (&str, Rect)> {
        self.headers
            .iter()
            .map(move |h| (h.title.as_str(), h.rect.shifted(scroll)))
    }

    /// Panel-local point -> item index. Gaps, padding and headers are misses, so
    /// a click between tiles never activates a neighbour.
    pub fn hit_test(&self, x: f32, y: f32, scroll: f32) -> Option<usize> {
        if x < 0.0 || y < 0.0 || x >= self.panel.w || y >= self.panel.h {
            return None;
        }
        let content_y = y + scroll;
        self.tiles
            .iter()
            .position(|tile| tile.contains(x, content_y))
    }

    pub fn clamp_scroll(&self, scroll: f32) -> f32 {
        scroll.clamp(0.0, self.max_scroll)
    }

    pub fn bands(&self) -> &[Band] {
        &self.bands
    }

    /// The keep-open button, panel-local.
    ///
    /// Chrome, not content: it does not scroll, so it cannot be carried off the
    /// top of a long grid. It sits on the first header's row, where headers
    /// leave the right-hand end empty, and falls back to the top padding strip
    /// when headers are turned off.
    /// Panel-local, fixed to the panel rather than the content. Empty when
    /// `search_h` is 0.
    pub fn search_rect(&self) -> Rect {
        let m = self.metrics;
        Rect {
            x: m.padding,
            y: 0.0,
            w: (self.panel.w - 2.0 * m.padding).max(0.0),
            h: m.search_h.min(self.panel.h),
        }
    }

    /// Which band owns a flat tile index.
    pub fn band_of(&self, tile: usize) -> Option<usize> {
        self.bands
            .iter()
            .position(|band| tile >= band.first && tile < band.first + band.count)
    }

    /// Where a dragged tile would land in `band`, as an insertion index in
    /// `0..=count`. Measured against tile centers, so the drop goes where the
    /// gap the cursor is nearest to is, not where the tile under it starts.
    pub fn insert_slot(&self, band: usize, x: f32, y: f32, scroll: f32) -> usize {
        let Some(band) = self.bands.get(band) else {
            return 0;
        };
        let Some(origin) = self.tiles.get(band.first) else {
            return 0;
        };
        let m = self.metrics;

        let row = ((y + scroll - origin.y) / (m.tile_h + m.gap)).floor().max(0.0) as usize;
        let column = ((x - origin.x) / (m.tile_w + m.gap) + 0.5)
            .floor()
            .clamp(0.0, self.cols as f32) as usize;

        (row * self.cols + column).min(band.count)
    }
}

/// The order slots end up in when the tile at `from` is dropped at insertion
/// point `to`. Each entry is the slot an item came from.
///
/// Split out from the panel because it is pure index arithmetic, and because
/// off-by-one here silently scrambles a user's pinned layout.
pub fn reordered(count: usize, from: usize, to: usize) -> Vec<usize> {
    let mut slots: Vec<usize> = (0..count).filter(|&slot| slot != from).collect();
    // `to` counts positions in the *original* list, so an insertion after the
    // dragged tile shifts down by one once that tile is lifted out.
    let at = if to > from { to - 1 } else { to };
    slots.insert(at.min(slots.len()), from);
    slots
}

/// The stretch of tiles a drag may move within: the neighbours inside
/// `(band_first, band_count)` that share the dragged tile's origin.
///
/// A merged section holds tiles from more than one source, and no config can
/// express a taskbar pin sitting between two manual ones — those two orders are
/// separate lists. So a drag rearranges its own run and stops at the seam.
///
/// Here for the same reason as `reordered`: pure index arithmetic, and an
/// off-by-one silently scrambles a user's pinned layout.
pub fn origin_run<T: PartialEq>(
    origins: &[T],
    band_first: usize,
    band_count: usize,
    tile: usize,
) -> (usize, usize) {
    let Some(origin) = origins.get(tile) else {
        return (tile, 0);
    };
    let same = |index: usize| origins.get(index) == Some(origin);

    let mut first = tile;
    while first > band_first && same(first - 1) {
        first -= 1;
    }
    let end = (band_first + band_count).min(origins.len());
    (first, (first..end).take_while(|index| same(*index)).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect { x: 0.0, y: 0.0, w: 2560.0, h: 1400.0 };

    fn metrics() -> Metrics {
        Metrics {
            tile_w: 200.0,
            tile_h: 140.0,
            gap: 10.0,
            padding: 20.0,
            max_fraction: 0.8,
            max_cols: 0,
            fixed_cols: 0,
            header_h: 28.0,
            section_gap: 14.0,
            search_h: 0.0,
        }
    }

    /// Which band a panel-local point falls in. The panel itself no longer asks,
    /// but the bands still have to cover it with nothing in between.
    fn band_at(l: &Layout, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || y < 0.0 || x >= l.panel.w || y >= l.panel.h {
            return None;
        }
        l.bands().iter().position(|band| band.rect.contains(x, y))
    }

    fn shape(title: &str, count: usize) -> SectionShape {
        SectionShape { title: title.into(), count }
    }

    fn one(count: usize) -> Vec<SectionShape> {
        vec![shape("", count)]
    }

    #[test]
    fn few_items_form_a_single_row_that_hugs_them() {
        let l = Layout::compute(&one(3), metrics(), WORK);
        assert_eq!(l.cols, 3);
        assert_eq!(l.panel.w, 3.0 * 200.0 + 2.0 * 10.0 + 40.0);
        assert_eq!(l.max_scroll, 0.0);
    }

    #[test]
    fn panel_is_centered_on_the_work_area() {
        let l = Layout::compute(&one(7), metrics(), WORK);
        let center_x = l.panel.x + l.panel.w / 2.0;
        let center_y = l.panel.y + l.panel.h / 2.0;
        assert!((center_x - 1280.0).abs() <= 1.0);
        assert!((center_y - 700.0).abs() <= 1.0);
    }

    #[test]
    fn width_stops_growing_at_the_fraction_cap() {
        let wide = Layout::compute(&one(200), metrics(), WORK);
        assert!(
            wide.panel.w <= WORK.w * 0.8,
            "panel {} exceeded the 80% cap of {}",
            wide.panel.w,
            WORK.w * 0.8
        );
        assert_eq!(wide.cols, Layout::compute(&one(500), metrics(), WORK).cols);
    }

    #[test]
    fn overflow_scrolls_instead_of_growing_past_the_height_cap() {
        let l = Layout::compute(&one(500), metrics(), WORK);
        assert!(l.panel.h <= WORK.h * 0.8 + 1.0);
        assert!(l.max_scroll > 0.0, "500 tiles must scroll");
        assert!(l.content_h > l.panel.h);
        assert_eq!(l.clamp_scroll(f32::MAX), l.max_scroll);
        assert_eq!(l.clamp_scroll(-50.0), 0.0);
    }

    #[test]
    fn tile_size_is_unchanged_by_item_count() {
        let small = Layout::compute(&one(2), metrics(), WORK).tile_rect(0, 0.0);
        let huge = Layout::compute(&one(500), metrics(), WORK).tile_rect(0, 0.0);
        assert_eq!((small.w, small.h), (huge.w, huge.h));
    }

    #[test]
    fn hit_test_finds_each_tile_by_its_own_rect() {
        let l = Layout::compute(&one(12), metrics(), WORK);
        for i in 0..12 {
            let r = l.tile_rect(i, 0.0);
            assert_eq!(l.hit_test(r.x + 1.0, r.y + 1.0, 0.0), Some(i));
            assert_eq!(l.hit_test(r.x + r.w - 1.0, r.y + r.h - 1.0, 0.0), Some(i));
        }
    }

    #[test]
    fn gaps_and_padding_are_misses() {
        let l = Layout::compute(&one(11), metrics(), WORK);
        let m = metrics();
        assert_eq!(l.hit_test(m.padding + m.tile_w + 2.0, m.padding + 5.0, 0.0), None);
        assert_eq!(l.hit_test(2.0, 2.0, 0.0), None);
    }

    #[test]
    fn hit_test_follows_the_scroll_offset() {
        let l = Layout::compute(&one(500), metrics(), WORK);
        let scroll = 200.0;
        let index = l.cols * 3;
        let r = l.tile_rect(index, scroll);
        assert_eq!(l.hit_test(r.x + 1.0, r.y + 1.0, scroll), Some(index));
    }

    #[test]
    fn zero_items_still_produces_a_sane_panel() {
        let l = Layout::compute(&[], metrics(), WORK);
        assert_eq!(l.cols, 1);
        assert!(l.panel.w > 0.0 && l.panel.h > 0.0);
        assert_eq!(l.hit_test(30.0, 30.0, 0.0), None);
    }

    #[test]
    fn max_cols_caps_a_row_the_screen_would_otherwise_allow() {
        // Ultrawide, where the fraction cap alone still permits a very long row.
        const WIDE: Rect = Rect { x: 0.0, y: 0.0, w: 5120.0, h: 1440.0 };

        let uncapped = Layout::compute(&one(40), metrics(), WIDE);
        assert!(uncapped.cols > 9, "fixture must allow more than 9 columns");

        let m = Metrics { max_cols: 9, ..metrics() };
        let capped = Layout::compute(&one(40), m, WIDE);
        assert_eq!(capped.cols, 9);
        assert!(capped.panel.w < uncapped.panel.w);
    }

    #[test]
    fn max_cols_does_not_pad_out_a_short_row() {
        let m = Metrics { max_cols: 9, ..metrics() };
        let l = Layout::compute(&one(3), m, WORK);
        assert_eq!(l.cols, 3, "three items must not stretch to nine columns");
    }

    #[test]
    fn a_zero_cap_means_whatever_fits() {
        const WIDE: Rect = Rect { x: 0.0, y: 0.0, w: 5120.0, h: 1440.0 };
        let capped = Metrics { max_cols: 9, ..metrics() };
        let uncapped = Metrics { max_cols: 0, ..metrics() };
        assert!(
            Layout::compute(&one(40), uncapped, WIDE).cols
                > Layout::compute(&one(40), capped, WIDE).cols
        );
    }

    #[test]
    fn absurdly_large_tiles_still_yield_one_column() {
        let m = Metrics { tile_w: 5000.0, tile_h: 4000.0, ..metrics() };
        let l = Layout::compute(&one(4), m, WORK);
        assert_eq!(l.cols, 1);
    }

    // --- sections ---

    #[test]
    fn sections_stack_and_indices_run_straight_through() {
        let sections = vec![shape("Pinned", 3), shape("Windows", 4)];
        let l = Layout::compute(&sections, metrics(), WORK);

        assert_eq!(l.tile_count(), 7);
        // Column count comes from the busiest section, not the total.
        assert_eq!(l.cols, 4);

        // Section 2's tiles sit strictly below section 1's.
        let last_of_first = l.tile_rect(2, 0.0);
        let first_of_second = l.tile_rect(3, 0.0);
        assert!(first_of_second.y > last_of_first.y);

        // And every tile is still hit-testable at its own index.
        for i in 0..7 {
            let r = l.tile_rect(i, 0.0);
            assert_eq!(l.hit_test(r.x + 2.0, r.y + 2.0, 0.0), Some(i));
        }
    }

    #[test]
    fn each_titled_section_gets_one_header_above_its_tiles() {
        let sections = vec![shape("Pinned", 2), shape("Windows", 2)];
        let l = Layout::compute(&sections, metrics(), WORK);

        let headers: Vec<_> = l.headers(0.0).collect();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "Pinned");
        assert_eq!(headers[1].0, "Windows");

        assert!(headers[0].1.y < l.tile_rect(0, 0.0).y);
        assert!(headers[1].1.y > l.tile_rect(1, 0.0).y);
        assert!(headers[1].1.y < l.tile_rect(2, 0.0).y);
    }

    #[test]
    fn a_header_is_not_a_tile() {
        let l = Layout::compute(&[shape("Pinned", 2)], metrics(), WORK);
        let header = l.headers(0.0).next().unwrap().1;
        assert_eq!(l.hit_test(header.x + 5.0, header.y + 5.0, 0.0), None);
    }

    #[test]
    fn empty_sections_contribute_nothing() {
        let with_empty = vec![shape("Pinned", 0), shape("Windows", 3)];
        let without = vec![shape("Windows", 3)];
        let a = Layout::compute(&with_empty, metrics(), WORK);
        let b = Layout::compute(&without, metrics(), WORK);

        assert_eq!(a.tile_count(), 3);
        assert_eq!(a.headers(0.0).count(), 1);
        assert_eq!(a.content_h, b.content_h);
        assert_eq!(a.tile_rect(0, 0.0), b.tile_rect(0, 0.0));
    }

    #[test]
    fn untitled_sections_still_group_without_a_header() {
        let l = Layout::compute(&[shape("", 2), shape("", 2)], metrics(), WORK);
        assert_eq!(l.headers(0.0).count(), 0);
        assert_eq!(l.tile_count(), 4);
        // Still two groups: the second starts a new row despite cols == 2.
        assert!(l.tile_rect(2, 0.0).y > l.tile_rect(0, 0.0).y);
    }

    // --- filtering ---

    #[test]
    fn a_fixed_column_count_holds_the_panel_still_as_matches_fall_away() {
        let m = Metrics { fixed_cols: 9, ..metrics() };
        let wide = Layout::compute(&one(40), m, WORK);
        let narrow = Layout::compute(&one(2), m, WORK);

        assert_eq!(narrow.cols, 9);
        assert_eq!(narrow.panel.w, wide.panel.w);
        assert_eq!(narrow.panel.x, wide.panel.x);
        // Only the height gives way, and the first tile stays where it was.
        assert!(narrow.panel.h < wide.panel.h);
        assert_eq!(narrow.tile_rect(0, 0.0).x, wide.tile_rect(0, 0.0).x);
    }

    #[test]
    fn a_fixed_count_still_yields_to_the_screen() {
        // A width frozen on an ultrawide, then applied on a laptop panel.
        const SMALL: Rect = Rect { x: 0.0, y: 0.0, w: 1280.0, h: 800.0 };
        let m = Metrics { fixed_cols: 40, ..metrics() };
        let l = Layout::compute(&one(40), m, SMALL);
        assert!(l.panel.w <= SMALL.w * 0.8, "panel {} overflowed", l.panel.w);
        assert_eq!(l.cols, Layout::compute(&one(40), metrics(), SMALL).cols);
    }

    #[test]
    fn zero_matches_still_leave_a_panel_to_say_so_in() {
        let m = Metrics { fixed_cols: 6, search_h: 30.0, ..metrics() };
        let l = Layout::compute(&[], m, WORK);
        assert_eq!(l.cols, 6, "the strip must not collapse to one column");
        assert!(l.panel.h >= 30.0);
        let strip = l.search_rect();
        assert!(strip.w > 0.0 && strip.h > 0.0);
    }

    #[test]
    fn the_search_strip_sits_above_every_tile_and_header() {
        let m = Metrics { search_h: 30.0, ..metrics() };
        let l = Layout::compute(&[shape("Pinned", 4)], m, WORK);
        let strip = l.search_rect();

        assert_eq!(strip.y, 0.0);
        assert_eq!(strip.h, 30.0);
        assert!(strip.y + strip.h <= l.headers(0.0).next().unwrap().1.y);
        assert!(strip.y + strip.h <= l.tile_rect(0, 0.0).y);
        // And it costs exactly its own height.
        let without = Layout::compute(&[shape("Pinned", 4)], metrics(), WORK);
        assert_eq!(l.content_h, without.content_h + 30.0);
    }

    #[test]
    fn the_search_strip_is_chrome_not_a_section() {
        let m = Metrics { search_h: 30.0, ..metrics() };
        let l = Layout::compute(&[shape("Pinned", 4)], m, WORK);

        assert_eq!(band_at(&l, 5.0, 5.0), None, "a drop on the strip belongs to nobody");
        assert_eq!(l.hit_test(5.0, 5.0, 0.0), None);
        // Everything below it is still covered.
        for y in 30..l.panel.h as i32 {
            assert!(band_at(&l, 5.0, y as f32).is_some(), "no band at y={y}");
        }
    }

    #[test]
    fn the_strip_does_not_scroll_with_the_grid() {
        let m = Metrics { search_h: 30.0, ..metrics() };
        let l = Layout::compute(&one(500), m, WORK);
        assert!(l.max_scroll > 0.0);

        // The grid slides under a strip that takes no scroll offset at all.
        let strip = l.search_rect();
        assert!(l.tile_rect(0, l.max_scroll).y < l.tile_rect(0, 0.0).y);
        assert_eq!(strip.y, 0.0);
        assert_eq!(strip.h, 30.0);
    }

    // --- bands and drop slots ---

    #[test]
    fn bands_cover_the_whole_panel_with_no_dead_space() {
        let sections = vec![shape("Pinned", 3), shape("Windows", 5)];
        let l = Layout::compute(&sections, metrics(), WORK);

        assert_eq!(l.bands().len(), 2);
        assert_eq!(l.bands()[0].rect.y, 0.0);
        let last = l.bands().last().unwrap();
        assert_eq!(last.rect.y + last.rect.h, l.content_h);
        assert_eq!(l.bands()[0].rect.h, l.bands()[1].rect.y);

        // Every row of pixels down the panel belongs to some band.
        for y in 0..l.panel.h as i32 {
            assert!(band_at(&l, 5.0, y as f32).is_some(), "no band at y={y}");
        }
    }

    #[test]
    fn empty_sections_do_not_get_a_band() {
        let l = Layout::compute(&[shape("Pinned", 0), shape("Windows", 2)], metrics(), WORK);
        assert_eq!(l.bands().len(), 1);
        // The band still names the section it came from, not its position.
        assert_eq!(l.bands()[0].section, 1);
    }

    #[test]
    fn a_tile_belongs_to_the_band_it_was_laid_out_in() {
        let l = Layout::compute(&[shape("A", 3), shape("B", 4)], metrics(), WORK);
        assert_eq!(l.band_of(0), Some(0));
        assert_eq!(l.band_of(2), Some(0));
        assert_eq!(l.band_of(3), Some(1));
        assert_eq!(l.band_of(6), Some(1));
        assert_eq!(l.band_of(7), None);

        // And a point inside a tile agrees with the tile's own band.
        let r = l.tile_rect(4, 0.0);
        assert_eq!(band_at(&l, r.x + 1.0, r.y + 1.0), l.band_of(4));
    }

    #[test]
    fn a_drop_lands_on_the_nearest_gap_between_tiles() {
        let l = Layout::compute(&one(5), metrics(), WORK);
        let first = l.tile_rect(0, 0.0);
        let third = l.tile_rect(2, 0.0);

        // Left of the first tile, and on its left half: before everything.
        assert_eq!(l.insert_slot(0, 1.0, first.y + 5.0, 0.0), 0);
        assert_eq!(l.insert_slot(0, first.x + 5.0, first.y + 5.0, 0.0), 0);
        // Right half of a tile means after it.
        assert_eq!(l.insert_slot(0, first.x + first.w - 5.0, first.y + 5.0, 0.0), 1);
        assert_eq!(l.insert_slot(0, third.x + third.w - 5.0, third.y + 5.0, 0.0), 3);
        // Past the last tile, clamped to the end.
        assert_eq!(l.insert_slot(0, l.panel.w - 1.0, l.panel.h - 1.0, 0.0), 5);
    }

    #[test]
    fn drop_slots_are_measured_within_the_section_not_the_panel() {
        let l = Layout::compute(&[shape("A", 4), shape("B", 4)], metrics(), WORK);
        let second = l.bands()[1].clone();
        let first_of_b = l.tile_rect(second.first, 0.0);

        assert_eq!(l.insert_slot(1, first_of_b.x + 5.0, first_of_b.y + 5.0, 0.0), 0);
        assert_eq!(
            l.insert_slot(1, first_of_b.x + first_of_b.w - 5.0, first_of_b.y + 5.0, 0.0),
            1
        );
    }

    #[test]
    fn drop_slots_follow_the_scroll_offset() {
        let l = Layout::compute(&one(500), metrics(), WORK);
        let scroll = 200.0;
        let index = l.cols * 3;
        let r = l.tile_rect(index, scroll);
        assert_eq!(l.insert_slot(0, r.x + 5.0, r.y + 5.0, scroll), index);
    }

    #[test]
    fn moving_a_tile_shifts_only_what_it_passes() {
        // 0 1 2 3 4, drag 0 to the end.
        assert_eq!(reordered(5, 0, 5), vec![1, 2, 3, 4, 0]);
        // Drag the last tile to the front.
        assert_eq!(reordered(5, 4, 0), vec![4, 0, 1, 2, 3]);
        // One step right: the insertion point is past the tile's own slot.
        assert_eq!(reordered(5, 1, 3), vec![0, 2, 1, 3, 4]);
        // One step left.
        assert_eq!(reordered(5, 3, 1), vec![0, 3, 1, 2, 4]);
    }

    #[test]
    fn dropping_a_tile_back_where_it_started_changes_nothing() {
        for slot in 0..5 {
            assert_eq!(reordered(5, slot, slot), vec![0, 1, 2, 3, 4]);
            assert_eq!(reordered(5, slot, slot + 1), vec![0, 1, 2, 3, 4]);
        }
    }

    /// A merged section: 3 taskbar pins then 2 manual ones, one band of 5.
    const MERGED: [char; 5] = ['t', 't', 't', 'm', 'm'];

    #[test]
    fn a_run_covers_only_the_neighbours_from_the_same_source() {
        for tile in 0..3 {
            assert_eq!(origin_run(&MERGED, 0, 5, tile), (0, 3), "taskbar tile {tile}");
        }
        for tile in 3..5 {
            assert_eq!(origin_run(&MERGED, 0, 5, tile), (3, 2), "manual tile {tile}");
        }
    }

    #[test]
    fn a_run_never_leaves_its_band() {
        // Same origins either side of the seam at 3: the band is the wall.
        let origins = ['m', 'm', 'm', 'm', 'm'];
        assert_eq!(origin_run(&origins, 3, 2, 4), (3, 2));
        assert_eq!(origin_run(&origins, 0, 3, 1), (0, 3));
    }

    #[test]
    fn a_single_source_section_is_one_whole_run() {
        let origins = ['w'; 6];
        assert_eq!(origin_run(&origins, 0, 6, 3), (0, 6));
    }

    #[test]
    fn a_run_stops_at_the_end_of_the_list() {
        // A band claiming more tiles than exist must not walk off the end.
        assert_eq!(origin_run(&MERGED, 0, 99, 0), (0, 3));
        assert_eq!(origin_run(&MERGED, 0, 5, 99), (99, 0));
    }

    /// The whole point of the seam: reordering inside one run leaves every tile
    /// belonging to the other source exactly where it was.
    #[test]
    fn reordering_a_run_cannot_disturb_the_other_source() {
        let (first, count) = origin_run(&MERGED, 0, 5, 3);
        let moved: Vec<char> = reordered(count, 3 - first, 2)
            .iter()
            .map(|slot| MERGED[first + slot])
            .collect();
        assert_eq!(moved, ['m', 'm']);
        assert_eq!(&MERGED[..first], ['t', 't', 't']);
    }

    #[test]
    fn sections_scroll_together_as_one_surface() {
        let sections = vec![shape("Pinned", 20), shape("Windows", 60)];
        let l = Layout::compute(&sections, metrics(), WORK);
        assert!(l.max_scroll > 0.0);

        let scroll = l.max_scroll;
        for (index, unscrolled) in (0..l.tile_count()).map(|i| (i, l.tile_rect(i, 0.0))) {
            let scrolled = l.tile_rect(index, scroll);
            assert!((unscrolled.y - scrolled.y - scroll).abs() < 0.01);
        }
        for ((_, a), (_, b)) in l.headers(0.0).zip(l.headers(scroll)) {
            assert!((a.y - b.y - scroll).abs() < 0.01);
        }
    }
}
