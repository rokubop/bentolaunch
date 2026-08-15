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
    pub header_h: f32,
    pub section_gap: f32,
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
        let cols = widest.clamp(1, capped);

        let panel_w = cols as f32 * m.tile_w + (cols - 1) as f32 * m.gap + 2.0 * m.padding;

        let mut tiles = Vec::new();
        let mut headers = Vec::new();
        let mut y = m.padding;

        for (index, section) in sections.iter().enumerate() {
            if section.count == 0 {
                continue;
            }
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
        }

        let content_h = y + m.padding;
        let panel_h = content_h.min(max_h);

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
            header_h: 28.0,
            section_gap: 14.0,
        }
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
