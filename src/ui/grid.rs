//! Grid layout. Pure arithmetic, no Windows types, so the sizing rules are
//! testable without a monitor.
//!
//! The rule (DESIGN.md "Resolved"): tile size is fixed and never changes. The
//! panel grows outward from the center of the work area as items are added,
//! until it reaches `max_screen_fraction` of that work area. Past that it stops
//! widening, and further rows scroll.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub tile_w: f32,
    pub tile_h: f32,
    pub gap: f32,
    pub padding: f32,
    pub max_fraction: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub cols: usize,
    pub rows: usize,
    /// Panel rect in screen coordinates.
    pub panel: Rect,
    /// Height the tiles want, which may exceed `panel.h`.
    pub content_h: f32,
    /// 0.0 when everything fits.
    pub max_scroll: f32,
    metrics: Metrics,
}

impl Layout {
    pub fn compute(count: usize, m: Metrics, work_area: Rect) -> Layout {
        let max_w = (work_area.w * m.max_fraction).max(m.tile_w + 2.0 * m.padding);
        let max_h = (work_area.h * m.max_fraction).max(m.tile_h + 2.0 * m.padding);

        // How many tiles fit across the widest panel we allow.
        let usable = (max_w - 2.0 * m.padding + m.gap).max(m.tile_w);
        let max_cols = (usable / (m.tile_w + m.gap)).floor().max(1.0) as usize;

        let cols = count.clamp(1, max_cols);
        let rows = count.div_ceil(cols).max(1);

        let panel_w = cols as f32 * m.tile_w + (cols - 1) as f32 * m.gap + 2.0 * m.padding;
        let content_h = rows as f32 * m.tile_h + (rows - 1) as f32 * m.gap + 2.0 * m.padding;
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
            rows,
            panel,
            content_h,
            max_scroll: (content_h - panel_h).max(0.0),
            metrics: m,
        }
    }

    /// Tile rect in panel-local coordinates, with `scroll` already applied.
    /// May be partly or wholly outside the panel when scrolled.
    pub fn tile_rect(&self, index: usize, scroll: f32) -> Rect {
        let m = self.metrics;
        let col = index % self.cols;
        let row = index / self.cols;
        Rect {
            x: m.padding + col as f32 * (m.tile_w + m.gap),
            y: m.padding + row as f32 * (m.tile_h + m.gap) - scroll,
            w: m.tile_w,
            h: m.tile_h,
        }
    }

    /// Panel-local point -> item index. Gaps and padding are misses, so a click
    /// between tiles never activates a neighbour.
    pub fn hit_test(&self, x: f32, y: f32, scroll: f32, count: usize) -> Option<usize> {
        if x < 0.0 || y < 0.0 || x >= self.panel.w || y >= self.panel.h {
            return None;
        }
        let m = self.metrics;
        let col_span = m.tile_w + m.gap;
        let row_span = m.tile_h + m.gap;

        let local_x = x - m.padding;
        let local_y = y - m.padding + scroll;
        if local_x < 0.0 || local_y < 0.0 {
            return None;
        }

        let col = (local_x / col_span).floor() as usize;
        let row = (local_y / row_span).floor() as usize;
        if col >= self.cols {
            return None;
        }
        // Reject the gap between this tile and the next.
        if local_x - col as f32 * col_span >= m.tile_w {
            return None;
        }
        if local_y - row as f32 * row_span >= m.tile_h {
            return None;
        }

        let index = row * self.cols + col;
        (index < count).then_some(index)
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
        Metrics { tile_w: 200.0, tile_h: 140.0, gap: 10.0, padding: 20.0, max_fraction: 0.8 }
    }

    #[test]
    fn few_items_form_a_single_row_that_hugs_them() {
        let l = Layout::compute(3, metrics(), WORK);
        assert_eq!((l.cols, l.rows), (3, 1));
        assert_eq!(l.panel.w, 3.0 * 200.0 + 2.0 * 10.0 + 40.0);
        assert_eq!(l.max_scroll, 0.0);
    }

    #[test]
    fn panel_is_centered_on_the_work_area() {
        let l = Layout::compute(7, metrics(), WORK);
        let center_x = l.panel.x + l.panel.w / 2.0;
        let center_y = l.panel.y + l.panel.h / 2.0;
        assert!((center_x - 1280.0).abs() <= 1.0);
        assert!((center_y - 700.0).abs() <= 1.0);
    }

    #[test]
    fn width_stops_growing_at_the_fraction_cap() {
        let wide = Layout::compute(200, metrics(), WORK);
        assert!(
            wide.panel.w <= WORK.w * 0.8,
            "panel {} exceeded the 80% cap of {}",
            wide.panel.w,
            WORK.w * 0.8
        );
        // ...and having hit the cap, more items must not widen it further.
        assert_eq!(wide.cols, Layout::compute(500, metrics(), WORK).cols);
    }

    #[test]
    fn overflow_scrolls_instead_of_growing_past_the_height_cap() {
        let l = Layout::compute(500, metrics(), WORK);
        assert!(l.panel.h <= WORK.h * 0.8 + 1.0);
        assert!(l.max_scroll > 0.0, "500 tiles must scroll");
        assert!(l.content_h > l.panel.h);
        assert_eq!(l.clamp_scroll(f32::MAX), l.max_scroll);
        assert_eq!(l.clamp_scroll(-50.0), 0.0);
    }

    #[test]
    fn tile_size_is_unchanged_by_item_count() {
        let small = Layout::compute(2, metrics(), WORK).tile_rect(0, 0.0);
        let huge = Layout::compute(500, metrics(), WORK).tile_rect(0, 0.0);
        assert_eq!((small.w, small.h), (huge.w, huge.h));
    }

    #[test]
    fn hit_test_finds_each_tile_by_its_own_rect() {
        let l = Layout::compute(12, metrics(), WORK);
        for i in 0..12 {
            let r = l.tile_rect(i, 0.0);
            assert_eq!(l.hit_test(r.x + 1.0, r.y + 1.0, 0.0, 12), Some(i));
            assert_eq!(l.hit_test(r.x + r.w - 1.0, r.y + r.h - 1.0, 0.0, 12), Some(i));
        }
    }

    #[test]
    fn gaps_padding_and_empty_cells_are_misses() {
        let l = Layout::compute(11, metrics(), WORK);
        let m = metrics();
        // Inside the gap after column 0.
        assert_eq!(l.hit_test(m.padding + m.tile_w + 2.0, m.padding + 5.0, 0.0, 11), None);
        // Inside the padding.
        assert_eq!(l.hit_test(2.0, 2.0, 0.0, 11), None);
        // The 12th cell exists in the grid but has no item behind it.
        let ghost = l.tile_rect(11, 0.0);
        assert_eq!(l.hit_test(ghost.x + 1.0, ghost.y + 1.0, 0.0, 11), None);
    }

    #[test]
    fn hit_test_follows_the_scroll_offset() {
        let l = Layout::compute(500, metrics(), WORK);
        let scroll = 200.0;
        let r = l.tile_rect(l.cols * 3, scroll);
        assert_eq!(l.hit_test(r.x + 1.0, r.y + 1.0, scroll, 500), Some(l.cols * 3));
    }

    #[test]
    fn zero_items_still_produces_a_sane_panel() {
        let l = Layout::compute(0, metrics(), WORK);
        assert_eq!(l.cols, 1);
        assert!(l.panel.w > 0.0 && l.panel.h > 0.0);
        assert_eq!(l.hit_test(30.0, 30.0, 0.0, 0), None);
    }

    #[test]
    fn absurdly_large_tiles_still_yield_one_column() {
        let m = Metrics { tile_w: 5000.0, tile_h: 4000.0, ..metrics() };
        let l = Layout::compute(4, m, WORK);
        assert_eq!(l.cols, 1);
        assert_eq!(l.rows, 4);
    }
}
