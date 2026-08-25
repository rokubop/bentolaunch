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

use crate::model::Mode;


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

    /// Overlap, edges excluded. Two tiles sharing an edge do not overlap, which
    /// is what makes a tile sitting exactly against the centre block count as
    /// clear of it.
    fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Content space to panel-local. Bands are handed out whole, so the panel
    /// needs the same shift the tiles get.
    pub fn shifted_by(self, scroll: f32) -> Rect {
        self.shifted(scroll)
    }
}

/// One cut in the panel: which side of it a box ends up on.
///
/// Left and right cut a rectangle down the middle; top and bottom cut it
/// across. Nothing else is needed to describe a bento, because every bento is
/// a rectangle cut in two, over and over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// Which way the cut runs.
    fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Across,
            Side::Top | Side::Bottom => Axis::Down,
        }
    }

    /// Whether this is the near half of its cut.
    fn first(self) -> bool {
        matches!(self, Side::Left | Side::Top)
    }

    pub fn parse(word: &str) -> Option<Side> {
        match word.trim().to_ascii_lowercase().as_str() {
            "left" | "l" => Some(Side::Left),
            "right" | "r" => Some(Side::Right),
            "top" | "t" | "up" => Some(Side::Top),
            "bottom" | "b" | "down" => Some(Side::Bottom),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
            Side::Top => "top",
            Side::Bottom => "bottom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// Side by side.
    Across,
    /// Stacked.
    Down,
}

/// Where a box sits, as a run of cuts from the whole panel inward.
///
/// `left` is the whole left half. `right/top` is the top of what is left over
/// after that cut. A share pins one side to a fraction of its parent; without
/// one the two halves are sized by what they hold.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct At {
    pub cuts: Vec<Side>,
    /// 0 to size by content. Applies to the last cut in `cuts`.
    pub share: f32,
}

impl At {
    /// `"right/top"`, or `"left@35"` to pin the left side to 35% of the width.
    ///
    /// Unreadable input is no placement rather than an error: a typo in a
    /// hand-edited config should cost one section its position, not the panel.
    pub fn parse(spec: &str) -> Option<At> {
        let (path, share) = match spec.split_once('@') {
            Some((path, percent)) => (path, percent.trim().parse::<f32>().ok()?.clamp(5.0, 95.0)),
            None => (spec, 0.0),
        };
        let cuts: Option<Vec<Side>> = path
            .split(['/', ' ', ','])
            .filter(|word| !word.trim().is_empty())
            .map(Side::parse)
            .collect();
        let cuts = cuts?;
        (!cuts.is_empty()).then_some(At { cuts, share: share / 100.0 })
    }

    /// Whether the cut this sits on divides width rather than height.
    ///
    /// What "bigger" means depends on it: a box on a left/right cut grows
    /// wider, one on a top/bottom cut grows taller. Saying "bigger" for both
    /// is how a button ends up pointing the wrong way.
    pub fn splits_width(&self) -> bool {
        self.cuts
            .last()
            .is_some_and(|side| side.axis() == Axis::Across)
    }

    /// Back to the config spelling.
    pub fn spell(&self) -> String {
        let path = self
            .cuts
            .iter()
            .map(|side| side.word())
            .collect::<Vec<_>>()
            .join("/");
        if self.share > 0.0 {
            format!("{path}@{:.0}", self.share * 100.0)
        } else {
            path
        }
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
    /// How far along the ring's top edge a box's title sits, from its left
    /// corner. Titles cost no layout, so this is not a gap in the grid.
    pub header_gap: f32,
    /// Clear rows between one box and the box stacked under it, in pixels and
    /// rounded to whole rows. Never a fraction of one: the panel is one lattice
    /// and a box cannot be moved off it.
    pub section_gap: f32,
    /// Filter strip above the grid, 0 when not filtering. Does not scroll: it
    /// is what explains why most of the grid is missing.
    pub search_h: f32,
}

/// What the layout needs to know about a section: its label, how many tiles,
/// and where its box sits in the bento.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionShape {
    pub title: String,
    pub count: usize,
    /// Where the box sits. `None` stacks it under whatever came before, which
    /// is what an unplaced section gets and what the plain default is made of.
    pub at: Option<At>,
    /// Tile columns inside this box. 0 fits as many as its rectangle takes.
    pub columns: usize,
    /// This box is held in the middle of the panel instead of taking a place in
    /// the tree. `Some(n)` is where it sits in the centre block, left to right.
    ///
    /// The centre of the screen is where a gaze pointer is most accurate, so
    /// one block is nailed there and the bento is laid out around it. The tree
    /// cannot say this: every cut runs edge to edge, so a centred box would
    /// drag its lines across the whole panel. Instead the centre claims a
    /// rectangle up front and every other box flows around it — the tree is
    /// planned as if the centre were not there, and the wrapping is what makes
    /// it true. See `Layout::compute`.
    ///
    /// A centre box needs `columns`: nothing can derive it, because the box is
    /// a fixed number of slots rather than a list that grew.
    pub center: Option<usize>,
}


/// The cells one box's ring encloses, on that box's own tile grid.
///
/// Not `Band::rect`. That one is stretched to tile the panel with no gaps,
/// because a drop landing beside a box has to mean *that* box, and it stays a
/// rectangle for exactly as long as dropping exists. This is what the box
/// actually occupies, and it is allowed to have a bite out of it where the
/// centre block stands - which is what turns a ring into an L or a C.
///
/// Ragged ends are filled in: a box whose last row is half full still gets a
/// squared-off ring. The shape follows the centre block, which never moves, and
/// not the item count, which changes every time a window opens.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cells {
    /// Top left of the first cell, content space.
    pub x: f32,
    pub y: f32,
    pub cols: usize,
    pub rows: usize,
    /// Row major, `rows * cols`. False only where the centre block took the
    /// cell out.
    pub filled: Vec<bool>,
}

impl Cells {
    /// Whether a cell is inside the ring. Out of range is outside, which is
    /// what makes the edge of the grid an edge of the shape.
    fn at(&self, row: usize, col: usize) -> bool {
        row < self.rows
            && col < self.cols
            && self.filled.get(row * self.cols + col).copied().unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub title: String,
    /// Content space.
    pub rect: Rect,
    /// The band this header sits above. Not the header's own index: an
    /// untitled section draws no header, so the two lists drift apart.
    pub band: usize,
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
    /// Tile columns inside this box. Boxes in one bento row each have their
    /// own, so a drop slot cannot be worked out from the panel's count.
    pub cols: usize,
    /// Half of the centre block rather than a box in the tree. Stretching,
    /// layout editing and drop targeting all have to leave these alone: the
    /// centre is not somewhere the cuts can reach.
    pub center: bool,
    /// What the box occupies, for the ring drawn round it. Kept apart from
    /// `rect` on purpose - see `Cells`.
    pub cells: Cells,
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

        let tree = plan(sections);
        let middle = center_boxes(sections);
        // What the block wants, before the panel has a width to give it.
        let asking: usize = center_widths(sections, &middle, usize::MAX).iter().sum();
        let wanted = tree.as_ref().map_or(1, |tree| tree.want(sections, capped).cols);
        // The centre wins over `max_columns`. That cap is a preference about
        // how long a row stays scannable; the block being whole is not a
        // preference, and a block hanging off the panel is a click that lands
        // on nothing. It still yields to what fits the screen, and never
        // narrows past one column a half, so it always fits the panel it is
        // drawn in.
        let room = asking.min(fits.max(middle.len()));
        let cols = if m.fixed_cols > 0 {
            m.fixed_cols.clamp(1, capped)
        } else {
            wanted.clamp(1, capped)
        }
        .max(room);

        // A block four columns wide in a panel nine columns wide leaves five
        // spare, which cannot be split evenly: the block ends up half a column
        // off centre, and off centre is the one thing it must not be. One
        // column more or less of panel fixes it, and keeping the block on the
        // same grid as everything around it is what keeps the wrap clean.
        //
        // Wider first, because narrower costs a column of grid on every row.
        // Not while a query is live: the width is frozen then, and a panel that
        // stepped sideways on a keystroke is the thing that rule exists to stop.
        let cols = if room > 0 && m.fixed_cols == 0 && (cols + room) % 2 == 1 {
            if cols < fits {
                cols + 1
            } else if cols > room {
                cols - 1
            } else {
                cols
            }
        } else {
            cols
        };

        let widths = center_widths(sections, &middle, cols);
        let center_cols: usize = widths.iter().sum();
        let center_rows = center_rows(sections, &middle, &widths);

        let panel_w = cols as f32 * m.tile_w + (cols - 1) as f32 * m.gap + 2.0 * m.padding;

        // Where the centre sits depends on how tall the panel is, and how tall
        // the panel is depends on what the centre pushed out of its way.
        //
        // There is no settling this. Wrapping is a step function - move the
        // hole down half a tile and a whole row of the grid comes free, which
        // shortens the panel, which moves the hole back up - so the two can and
        // do trade places forever. Repeated once to get the hole somewhere
        // sensible in the content, and then the panel is *positioned* to put it
        // on the middle of the screen rather than the hole being nudged to the
        // middle of the panel. The screen is what the centre is measured
        // against; the panel is only what happens to be drawn on it, and it is
        // free to sit wherever it has to.
        let mut reserve = None;
        let mut home = None;
        let mut align = None;
        for _ in 0..4 {
            let (pass, bottom) = lay_out(sections, &m, &tree, capped, cols, reserve, home);
            // Read off the pass with no hole in it, so the grid the block lines
            // up with is the grid the panel would have had without it.
            align = align.or_else(|| row_grid(&pass, &m));
            let reached = reaching(bottom, reserve);
            let content_h = reached + m.padding;
            let next =
                center_reserve((center_cols, center_rows), &m, cols, content_h.min(max_h), align);
            // The corner the app's own button holds, settled in the same loop
            // and for the same reason: what it takes out of the grid can be the
            // row that decides how tall the grid is.
            let next_home = home_reserve(cols, reached, &m);
            let done = settled(reserve, next) && settled(home, next_home);
            reserve = next;
            home = next_home;
            if done {
                break;
            }
        }

        let (mut out, bottom) = lay_out(sections, &m, &tree, capped, cols, reserve, home);
        let content_h = reaching(bottom, reserve) + m.padding;
        let panel_h = content_h.min(max_h);

        // The strip is chrome, so a drop on it hits nobody.
        //
        // Before the centre goes in: `stretch` walks the tree and indexes the
        // bands by leaf order, so the centre's bands have to arrive after it.
        if let Some(tree) = &tree {
            let whole = Rect {
                x: 0.0,
                y: m.search_h,
                w: panel_w,
                h: content_h - m.search_h,
            };
            stretch(tree, &mut out.bands, 0, whole);
        }
        if let Some(reserve) = reserve {
            place_center(sections, &m, &middle, &widths, reserve, &mut out);
        }

        // Centered on the work area, snapped to whole pixels so tiles stay crisp.
        let mut panel_y = work_area.y + (work_area.h - panel_h) / 2.0;
        // Except when there is a centre block, which is the thing that has to
        // land on the middle of the screen. The panel slides to put it there.
        // Clamped to the work area, so a panel taller than the screen still
        // starts at the top of it and scrolls, which beats hanging off the edge.
        if let Some(r) = reserve {
            let slack = (work_area.h - panel_h).max(0.0);
            panel_y = (work_area.y + work_area.h / 2.0 - (r.y + r.h / 2.0))
                .clamp(work_area.y, work_area.y + slack);
        }
        let panel = Rect {
            x: (work_area.x + (work_area.w - panel_w) / 2.0).round(),
            y: panel_y.round(),
            w: panel_w.round(),
            h: panel_h.round(),
        };

        Layout {
            cols,
            panel,
            content_h,
            max_scroll: (content_h - panel_h).max(0.0),
            tiles: out.tiles,
            headers: out.headers,
            bands: out.bands,
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

    /// Title, where to draw it, and which band it belongs to.
    pub fn headers(&self, scroll: f32) -> impl Iterator<Item = (&str, Rect, usize)> {
        self.headers
            .iter()
            .map(move |h| (h.title.as_str(), h.rect.shifted(scroll), h.band))
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

    /// The centre block as one rectangle, and the x of each seam between its
    /// halves. Content space. `None` when there is no block.
    ///
    /// The halves are separate boxes with separate tiles, and they have to read
    /// as one container with a line down it — a border round each half would say
    /// they were two things that happened to be next to each other, which is
    /// the opposite of what the block is.
    pub fn center_frame(&self) -> Option<(Rect, Vec<f32>)> {
        let halves: Vec<&Band> = self.bands.iter().filter(|band| band.center).collect();
        let mut rect = halves.first()?.rect;
        let mut seams = Vec::new();
        for band in halves.iter().skip(1) {
            // Midway across the gutter between them, so the line sits in the
            // space the tiles already leave rather than beside one of them.
            seams.push((rect.x + rect.w + band.rect.x) / 2.0);
            rect = Rect {
                w: band.rect.x + band.rect.w - rect.x,
                h: rect.h.max(band.rect.h),
                ..rect
            };
        }
        Some((rect, seams))
    }

    /// The ring around one box, panel-local, as closed rings of corners.
    ///
    /// More than one when the centre block stands wholly inside the box: an
    /// outer ring and a hole. Empty for a box with no tiles.
    pub fn band_ring(&self, band: usize, scroll: f32) -> Vec<Vec<(f32, f32)>> {
        let Some(band) = self.bands.get(band) else {
            return Vec::new();
        };
        ring_of(&band.cells, &self.metrics)
            .into_iter()
            .map(|ring| ring.into_iter().map(|(x, y)| (x, y - scroll)).collect())
            .collect()
    }

    /// The panel's own button: always there, bottom right, one cell of the grid.
    ///
    /// So the panel can be worked without knowing a right-click menu exists. It
    /// is the one control that is always in the same place, which is what makes
    /// it findable by someone pointing with their eyes.
    ///
    /// A cell, and the layout keeps that cell clear - `home_reserve`. Panel
    /// local rather than content space, because chrome that scrolled off the
    /// top of a long grid would not be always in the same place at all. The two
    /// are the same rectangle whenever the content fits, which is the case the
    /// button is drawn in nearly always.
    pub fn home_rect(&self) -> Rect {
        let m = self.metrics;
        Rect {
            x: (m.padding + (self.cols.saturating_sub(1)) as f32 * (m.tile_w + m.gap))
                .min((self.panel.w - m.tile_w).max(0.0)),
            y: (self.panel.h - m.padding - m.tile_h).max(0.0),
            w: m.tile_w,
            h: m.tile_h,
        }
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
        let cells = &band.cells;
        let cols = cells.cols.max(1);
        let m = self.metrics;

        // Which cell of the box's own grid the pointer landed in. Measured
        // against tile centres across, so the drop goes to the gap it is
        // nearest rather than to the tile it happens to be over.
        let row = ((y + scroll - cells.y) / (m.tile_h + m.gap))
            .floor()
            .clamp(0.0, cells.rows as f32) as usize;
        let column = ((x - cells.x) / (m.tile_w + m.gap) + 0.5)
            .floor()
            .clamp(0.0, cols as f32) as usize;

        // Then how many tiles the box actually put before that cell. Not the
        // cell's own index: a cell the centre block is standing on holds no
        // tile, so counting cells would land the drop that many places further
        // along than the pointer.
        let target = (row * cols + column).min(cells.filled.len());
        cells.filled[..target]
            .iter()
            .filter(|filled| **filled)
            .count()
            .min(band.count)
    }
}

/// The boundary of a box's cells, as closed rings in content space.
///
/// One ring for the outside, and one more for every hole strictly inside it -
/// the centre block standing in the middle of a box rather than against an edge
/// of it. Every corner is a right angle; rounding them is the drawing's job.
///
/// Points sit a quarter of a gap outside the tiles - half the gutter, so two
/// boxes side by side leave the other half of it clear between their rings.
///
/// Not the whole half-gap. Boxes tile the panel, so a ring drawn the full way
/// out would land in the same pixel as its neighbour's, and one faint seam
/// shared by two boxes was fine while every box wore the same colour. Two
/// colours in one line read as a fringe. It also keeps the ring clear of the
/// centre block's own frame, which does sit at the full half-gap - the block is
/// in front of the layout, and a sliver of panel between them says so.
fn ring_of(cells: &Cells, m: &Metrics) -> Vec<Vec<(f32, f32)>> {
    // Cell corner `k` in pixels: the middle of the gutter, which is the one
    // place a boundary can sit and mean the same thing from both sides. Uniform
    // across the grid, so two cells sharing an edge produce the same two points
    // and the walk below closes exactly. `inset` then pulls the finished ring
    // back off it, which is a thing only a closed shape can be asked to do.
    let px = |c: usize| cells.x + c as f32 * (m.tile_w + m.gap) - m.gap / 2.0;
    let py = |r: usize| cells.y + r as f32 * (m.tile_h + m.gap) - m.gap / 2.0;

    // Directed so the inside is always on the right of travel: right along a
    // top, down a right, left along a bottom, up a left. Holes come out wound
    // the other way for free, which is what a fill rule wants.
    let mut edges: Vec<((usize, usize), (usize, usize))> = Vec::new();
    for row in 0..cells.rows {
        for col in 0..cells.cols {
            if !cells.at(row, col) {
                continue;
            }
            if !(row > 0 && cells.at(row - 1, col)) {
                edges.push(((col, row), (col + 1, row)));
            }
            if !cells.at(row, col + 1) {
                edges.push(((col + 1, row), (col + 1, row + 1)));
            }
            if !cells.at(row + 1, col) {
                edges.push(((col + 1, row + 1), (col, row + 1)));
            }
            if !(col > 0 && cells.at(row, col - 1)) {
                edges.push(((col, row + 1), (col, row)));
            }
        }
    }

    // Follow each edge to the one leaving where it arrived. Every vertex has as
    // many edges leaving as arriving, so a walk that starts on an unused edge
    // always comes back to where it began.
    let mut used = vec![false; edges.len()];
    let mut rings = Vec::new();
    for start in 0..edges.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let first = edges[start].0;
        let mut corners = vec![first];
        let mut here = edges[start].1;
        // The ceiling is only so a shape nobody expected cannot spin the UI
        // thread, which is the one failure that looks like a broken PC.
        while here != first && corners.len() <= edges.len() {
            corners.push(here);
            // Two cells touching only at a corner leave two edges from that
            // point. Either continues a real ring, so take whichever is free.
            let Some(next) = (0..edges.len()).find(|&e| !used[e] && edges[e].0 == here) else {
                break;
            };
            used[next] = true;
            here = edges[next].1;
        }
        if here != first || corners.len() < 4 {
            continue;
        }
        let ring: Vec<(f32, f32)> = corners_only(&corners)
            .into_iter()
            .map(|(col, row)| (px(col), py(row)))
            .collect();
        rings.push(inset(&ring, m.gap / 4.0));
    }
    rings
}

/// Pull a closed ring in off the gutter's middle, so two boxes side by side
/// leave the other half of the gutter clear between their rings.
///
/// Every edge moves `d` toward the inside and the corners are put back where
/// the moved edges cross. Doing it per point instead would push a reflex
/// corner - the inside of a C, where the centre block bit into the box - the
/// wrong way, and the notch would close over the block.
///
/// The rings arrive wound so the inside is always on the right of travel, which
/// is what makes one rule work for the outer ring and for a hole alike.
fn inset(ring: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
    let n = ring.len();
    // Where each edge lands once moved. Edges are axis aligned, so an edge is
    // one coordinate and the inward normal only ever touches that one.
    let moved: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let (from, to) = (ring[i], ring[(i + 1) % n]);
            let (dx, dy) = (to.0 - from.0, to.1 - from.1);
            let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
            // Right of travel, in screen coordinates: (x, y) -> (-y, x).
            let normal = (-dy / len * d, dx / len * d);
            (from.0 + normal.0, from.1 + normal.1)
        })
        .collect();

    // Corner `i` is where the edge arriving and the edge leaving cross. One is
    // horizontal and one is vertical, so the crossing is one coordinate from
    // each - no line intersection needed.
    (0..n)
        .map(|i| {
            let arriving = ring[(i + n - 1) % n];
            let (before, here) = (moved[(i + n - 1) % n], moved[i]);
            if (arriving.1 - ring[i].1).abs() < f32::EPSILON {
                // Arrived along a horizontal run, so it fixes y and the run
                // leaving fixes x.
                (here.0, before.1)
            } else {
                (before.0, here.1)
            }
        })
        .collect()
}

/// Drop the points a straight run passes through. Three in a line are one
/// corner too many, and every corner costs an arc when this is drawn.
fn corners_only(ring: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let n = ring.len();
    (0..n)
        .filter(|&i| {
            let before = ring[(i + n - 1) % n];
            let here = ring[i];
            let after = ring[(i + 1) % n];
            let straight = (before.0 == here.0 && here.0 == after.0)
                || (before.1 == here.1 && here.1 == after.1);
            !straight
        })
        .map(|i| ring[i])
        .collect()
}

/// One option offered for the box being edited.
///
/// Three ideas, kept apart because mixing them is what made this confusing:
///
/// 1. **Claim a side.** The box becomes the whole of it, full height or full
///    width, and whatever was there is moved off.
/// 2. **Arrange.** Move up and down the stack that fills whatever the claimed
///    sides left over.
/// 3. **Size.** How much of its cut the box takes, and how many tiles it shows.
///
/// The verbs a tiling window manager uses, because the layout is the same
/// structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    ClaimLeft,
    ClaimTop,
    ClaimBottom,
    ClaimRight,
    /// Earlier or later in the stack that fills the leftover space.
    MoveUp,
    MoveDown,
    /// More or less of the cut this box sits on.
    Grow,
    Shrink,
    Fewer,
    More,
    Done,
}

impl Control {
    /// The side this option claims, if it is one of those.
    pub fn side(self) -> Option<Side> {
        match self {
            Control::ClaimLeft => Some(Side::Left),
            Control::ClaimRight => Some(Side::Right),
            Control::ClaimTop => Some(Side::Top),
            Control::ClaimBottom => Some(Side::Bottom),
            _ => None,
        }
    }

    /// The big mark on the option tile.
    pub fn glyph(self) -> &'static str {
        match self {
            Control::ClaimLeft => "\u{2590}",
            Control::ClaimRight => "\u{258C}",
            Control::ClaimTop => "\u{2584}",
            Control::ClaimBottom => "\u{2580}",
            Control::MoveUp => "\u{2191}",
            Control::MoveDown => "\u{2193}",
            // Replaced by `wording` whenever a box is selected: which way these
            // move is a property of the cut, not of the button.
            Control::Grow => "\u{2194}",
            Control::Shrink => "\u{2192}\u{2190}",
            Control::Fewer => "\u{2212}",
            Control::More => "+",
            Control::Done => "\u{2713}",
        }
    }

    /// Said in words underneath. A gaze pointer settles on a tile long enough
    /// to read one, and a glyph alone is a guess.
    pub fn label(self) -> &'static str {
        match self {
            Control::ClaimLeft => "Full left",
            Control::ClaimRight => "Full right",
            Control::ClaimTop => "Full top",
            Control::ClaimBottom => "Full bottom",
            Control::MoveUp => "Move up",
            Control::MoveDown => "Move down",
            Control::Grow => "Bigger",
            Control::Shrink => "Smaller",
            Control::Fewer => "Fewer tiles",
            Control::More => "More tiles",
            Control::Done => "Done",
        }
    }
}

impl Control {
    /// The share this option should write, given where the box is now.
    ///
    /// Across the panel it steps a whole column at a time, so every click moves
    /// the edge somewhere visible. A 5% nudge often did not cross a column
    /// boundary at all, and a button that sometimes does nothing reads as
    /// broken.
    pub fn resized(self, state: &BoxState) -> Option<f32> {
        let step = match self {
            Control::Grow => 1.0,
            Control::Shrink => -1.0,
            _ => return None,
        };
        let at = state.at.as_ref()?;

        if at.splits_width() && state.panel_cols > 1 {
            let cols = (state.cols as f32 + step).clamp(1.0, state.panel_cols as f32 - 1.0);
            Some(cols / state.panel_cols as f32)
        } else {
            Some((state.share_now + step * 0.08).clamp(0.1, 0.9))
        }
    }

    /// Whether this is the side the box already holds. Drawn lit, and clicking
    /// it gives the side back rather than claiming it again.
    pub fn holds(self, state: &BoxState) -> bool {
        self.side().is_some_and(|side| {
            state.at.as_ref().map(|at| at.cuts.as_slice()) == Some(&[side])
        })
    }

    /// Glyph and label for this option against a particular box.
    ///
    /// Only the two size options change: everything else means the same thing
    /// wherever the box sits. "Bigger" on a box down the left means wider, and
    /// on a box across the bottom means taller, so the button has to say which.
    pub fn wording(self, state: &BoxState) -> (&'static str, &'static str) {
        let across = state.at.as_ref().is_some_and(At::splits_width);
        if self.holds(state) {
            // Says what the click does, not where the box is. Where it is, the
            // lit square already shows.
            return match self {
                Control::ClaimLeft => (self.glyph(), "Leave left"),
                Control::ClaimRight => (self.glyph(), "Leave right"),
                Control::ClaimTop => (self.glyph(), "Leave top"),
                Control::ClaimBottom => (self.glyph(), "Leave bottom"),
                _ => (self.glyph(), self.label()),
            };
        }
        match self {
            Control::Grow if across => ("\u{2194}", "Wider"),
            Control::Grow => ("\u{2195}", "Taller"),
            Control::Shrink if across => ("\u{2192}\u{2190}", "Narrower"),
            Control::Shrink => ("\u{2193}\u{2191}", "Shorter"),
            _ => (self.glyph(), self.label()),
        }
    }
}

/// The box being edited, as the options need to see it. Gathered by the panel,
/// judged here, so the rule can be tested without a window.
#[derive(Debug, Clone, PartialEq)]
pub struct BoxState {
    /// Tiles it shows, and tiles it could show.
    pub shown: usize,
    pub total: usize,
    /// Where it sits now, if it has been placed at all.
    pub at: Option<At>,
    /// Boxes on the panel. A lone box has nowhere to be sent.
    pub boxes: usize,
    /// Position in the stack that fills the leftover space, and how many are
    /// in it. Only meaningful when `at` is `None`.
    pub at_stack: usize,
    pub stack: usize,
    /// Columns the box takes, and the panel's whole budget.
    pub cols: usize,
    pub panel_cols: usize,
    /// The fraction of its cut the box currently fills, as laid out.
    ///
    /// Measured rather than read from `at`: a box sized by its contents has no
    /// share written down, and asking the config produced 0.0 - which read as
    /// "already at the bottom" and greyed out the button for every box that had
    /// never been resized.
    pub share_now: f32,
}

impl Control {
    /// Whether this option would do something sensible right now.
    ///
    /// Offering one that cannot apply is worse than hiding it. Every option
    /// writes to the config, and the two that used to run off the bottom wrote
    /// the *opposite* of the word on the button: one column narrower became
    /// "share the row", one tile fewer became "show all of them". A layout
    /// breaking under someone being careful is the thing this prevents.
    pub fn allowed(self, state: &BoxState) -> bool {
        match self {
            Control::Done => true,
            // Nothing to divide the panel with.
            _ if state.boxes < 2 => false,
            // Always available. The one matching the side the box already
            // holds gives it back, which is why there is no separate button
            // for that: the lit one shows where you are and clicking it
            // undoes it.
            Control::ClaimLeft
            | Control::ClaimTop
            | Control::ClaimBottom
            | Control::ClaimRight => true,
            // Moving is about the stack filling the leftover space. A box that
            // holds a side of its own is not in that stack.
            Control::MoveUp => state.at.is_none() && state.at_stack > 0,
            Control::MoveDown => {
                state.at.is_none() && state.at_stack + 1 < state.stack
            }
            // Sizing is a property of a cut, so it needs the box to sit on one.
            // Along the across axis the limit is columns, because that is what
            // actually changes; down the panel it is the share.
            Control::Grow if !state.at.as_ref().is_some_and(At::splits_width) => {
                state.at.is_some() && state.share_now < 0.9
            }
            Control::Shrink if !state.at.as_ref().is_some_and(At::splits_width) => {
                state.at.is_some() && state.share_now > 0.1
            }
            Control::Grow => state.at.is_some() && state.cols + 1 < state.panel_cols,
            Control::Shrink => state.at.is_some() && state.cols > 1,
            Control::Fewer => state.shown > 1,
            Control::More => state.shown < state.total,
        }
    }
}

/// Reading order, four to a row.
/// Reading order, five to a row.
/// Reading order, four to a row.
pub const CONTROLS: [Control; 11] = [
    // Claim a side.
    Control::ClaimLeft,
    Control::ClaimTop,
    Control::ClaimBottom,
    Control::ClaimRight,
    // Arrange.
    Control::MoveUp,
    Control::MoveDown,
    Control::Grow,
    // Size.
    Control::Shrink,
    Control::Fewer,
    Control::More,
    Control::Done,
];

/// How many big squares sit in one row.
const MENU_COLS: usize = 4;

/// A grid of big squares, centred in the panel.
///
/// Centred and tile-sized on purpose: this app is pointed at, sometimes by
/// gaze, and the middle of the screen is the cheapest place to reach. A strip
/// of small buttons is the one shape that cannot be used that way.
///
/// Panel-local, and deliberately not in content space: menus are overlays and
/// must not scroll away from under the pointer.
pub fn centred_grid(panel: Rect, count: usize, tile_w: f32, tile_h: f32, gap: f32) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let across = count.min(MENU_COLS);
    let rows = count.div_ceil(MENU_COLS);
    let total_w = across as f32 * tile_w + (across - 1) as f32 * gap;
    let total_h = rows as f32 * tile_h + (rows - 1) as f32 * gap;

    let left = ((panel.w - total_w) / 2.0).max(0.0);
    let top = ((panel.h - total_h) / 2.0).max(0.0);

    (0..count)
        .map(|n| Rect {
            x: left + (n % MENU_COLS) as f32 * (tile_w + gap),
            y: top + (n / MENU_COLS) as f32 * (tile_h + gap),
            w: tile_w,
            h: tile_h,
        })
        .collect()
}

/// The option tiles for the box being edited.
pub fn controls(panel: Rect, tile_w: f32, tile_h: f32, gap: f32) -> Vec<(Control, Rect)> {
    CONTROLS
        .iter()
        .copied()
        .zip(centred_grid(panel, CONTROLS.len(), tile_w, tile_h, gap))
        .collect()
}


/// What the app's own button opens: everything the right-click menu had, as
/// squares big enough to aim at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    EditLayout,
    /// Fill and empty the centre block.
    Favorites,
    /// Clicking closes things instead of switching to them.
    CloseApps,
    AddApp,
    AddFolder,
    AddFile,
    Settings,
    Close,
}

/// Eight, which is two full rows of four. The three modes lead, because they
/// are what this menu is now mostly for: the three pickers below them are one
/// job each and are reachable from the right-click menu as well.
pub const COMMANDS: [Command; 8] = [
    Command::EditLayout,
    Command::Favorites,
    Command::CloseApps,
    Command::Settings,
    Command::AddApp,
    Command::AddFolder,
    Command::AddFile,
    Command::Close,
];

impl Command {
    pub fn glyph(self) -> &'static str {
        match self {
            Command::EditLayout => "\u{25A6}",
            // A star, in its text presentation. Every mark on these squares is
            // a line drawing from the UI font; one coloured pictogram among
            // them reads as the odd one out rather than as a set.
            Command::Favorites => "\u{2605}",
            // A crossed circle, not the plain cross: the plain one already
            // means "close this menu" two squares along.
            Command::CloseApps => "\u{2297}",
            Command::AddApp => "+",
            Command::AddFolder => "\u{1F5C0}",
            Command::AddFile => "\u{1F5CE}",
            Command::Settings => "\u{2699}",
            Command::Close => "\u{2715}",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Command::EditLayout => "Edit layout",
            Command::Favorites => "Favorites",
            Command::CloseApps => "Close apps",
            Command::AddApp => "Add app",
            Command::AddFolder => "Add folder",
            Command::AddFile => "Add file",
            Command::Settings => "Settings",
            Command::Close => "Close menu",
        }
    }

    /// The mode this square turns on, if it turns one on.
    pub fn mode(self) -> Option<Mode> {
        match self {
            Command::EditLayout => Some(Mode::Layout),
            Command::Favorites => Some(Mode::Favorites),
            Command::CloseApps => Some(Mode::Close),
            Command::AddApp
            | Command::AddFolder
            | Command::AddFile
            | Command::Settings
            | Command::Close => None,
        }
    }
}

pub fn commands(panel: Rect, tile_w: f32, tile_h: f32, gap: f32) -> Vec<(Command, Rect)> {
    COMMANDS
        .iter()
        .copied()
        .zip(centred_grid(panel, COMMANDS.len(), tile_w, tile_h, gap))
        .collect()
}

/// The panel as a rectangle cut in two, over and over.
///
/// This is the structure a tiling window manager uses, and it is here for the
/// same reason: a flat list of rows can say "three boxes across" but never
/// "one down the left, and the remainder split in half". That second shape is
/// what a bento is, and no number of options on a flat list produces it.
/// A cut was made here but nothing has filled this half yet.
const EMPTY: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Leaf(usize),
    Cut {
        axis: Axis,
        /// 0 to size both halves by what they hold.
        share: f32,
        near: Box<Node>,
        far: Box<Node>,
    },
}

/// Size in whole tiles: what a subtree wants before anything is divided up.
///
/// Counted in tiles rather than pixels because tile size is fixed and never
/// changes with item count. That rule is what keeps the panel growing from the
/// centre instead of reflowing, and it survives the tree intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Want {
    cols: usize,
    rows: usize,
}

impl Node {
    /// Put a box somewhere, cutting the panel as the path asks.
    ///
    /// A cut that already exists on the right axis is reused, so two sections
    /// naming `right/top` and `right/bottom` land either side of one cut rather
    /// than each making their own.
    fn insert(&mut self, at: &[Side], share: f32, leaf: usize) {
        let Some((side, rest)) = at.split_first() else {
            // Nothing left to say. Two boxes claiming one spot stack, so the
            // second is visible rather than silently dropped.
            let taken = std::mem::replace(self, Node::Leaf(leaf));
            if !matches!(taken, Node::Leaf(EMPTY)) {
                *self = Node::Cut {
                    axis: Axis::Down,
                    share: 0.0,
                    near: Box::new(taken),
                    far: Box::new(Node::Leaf(leaf)),
                };
            }
            return;
        };

        let axis = side.axis();
        // Not already cut this way: cut it, keeping whatever was here on the
        // opposite side from the one being asked for.
        if !matches!(self, Node::Cut { axis: existing, .. } if *existing == axis) {
            let taken = std::mem::replace(self, Node::Leaf(EMPTY));
            let empty = Box::new(Node::Leaf(EMPTY));
            *self = if side.first() {
                Node::Cut { axis, share: 0.0, near: empty, far: Box::new(taken) }
            } else {
                Node::Cut { axis, share: 0.0, near: Box::new(taken), far: empty }
            };
        }

        let Node::Cut { share: existing, near, far, .. } = self else {
            return;
        };
        // The share is written on the cut the box named, and the near half owns
        // it: "left@35" and "right@65" describe the same panel.
        if rest.is_empty() && share > 0.0 {
            *existing = if side.first() { share } else { 1.0 - share };
        }
        let branch = if side.first() { near } else { far };
        branch.insert(rest, share, leaf);
    }

    /// The first half a cut opened up and nothing filled.
    fn hole(&mut self) -> Option<&mut Node> {
        match self {
            Node::Leaf(EMPTY) => Some(self),
            Node::Leaf(_) => None,
            Node::Cut { near, far, .. } => near.hole().or_else(|| far.hole()),
        }
    }

    /// Drop the placeholders left where a path cut the panel but nothing filled
    /// the other half.
    fn prune(self) -> Option<Node> {
        match self {
            Node::Leaf(EMPTY) => None,
            Node::Leaf(index) => Some(Node::Leaf(index)),
            Node::Cut { axis, share, near, far } => match (near.prune(), far.prune()) {
                (Some(near), Some(far)) => Some(Node::Cut {
                    axis,
                    share,
                    near: Box::new(near),
                    far: Box::new(far),
                }),
                // A cut with one side empty is not a cut.
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    fn want(&self, sections: &[SectionShape], capped: usize) -> Want {
        match self {
            Node::Leaf(index) => {
                let section = &sections[*index];
                let cols = if section.columns > 0 {
                    section.columns
                } else {
                    section.count.min(capped).max(1)
                };
                Want { cols, rows: section.count.div_ceil(cols).max(1) }
            }
            Node::Cut { axis, near, far, .. } => {
                let (a, b) = (near.want(sections, capped), far.want(sections, capped));
                match axis {
                    Axis::Across => Want { cols: a.cols + b.cols, rows: a.rows.max(b.rows) },
                    Axis::Down => Want { cols: a.cols.max(b.cols), rows: a.rows + b.rows },
                }
            }
        }
    }

    /// Hand out columns down the tree, then let each box lay itself out in what
    /// it was given.
    ///
    /// Only the across axis is divided. Down the panel a box takes the height
    /// it needs, which is what keeps the tile size fixed and the panel growing
    /// from the centre rather than squashing to fit.
    fn place(&self, sections: &[SectionShape], cut: Cut<'_>, out: &mut Placement) -> f32 {
        let Cut { m, capped, x, y, cols, holes } = cut;
        match self {
            Node::Leaf(index) => {
                place_box(sections, *index, Spot { m, x, y, cols, holes }, out)
            }
            Node::Cut { axis, share, near, far } => {
                let (a, b) = (near.want(sections, capped), far.want(sections, capped));
                match axis {
                    Axis::Across => {
                        let split = divide(cols, a.cols, b.cols, *share);
                        let step = split as f32 * (m.tile_w + m.gap);
                        let left = near.place(sections, Cut { cols: split, ..cut }, out);
                        let right = far.place(
                            sections,
                            Cut { x: x + step, cols: cols - split, ..cut },
                            out,
                        );
                        left.max(right)
                    }
                    Axis::Down => {
                        let top = near.place(sections, cut, out);
                        far.place(sections, Cut { y: next_row(top, m), ..cut }, out)
                    }
                }
            }
        }
    }
}

/// Where a subtree is being laid out. Grouped so the recursion passes a value
/// rather than seven positional arguments that are one swap away from a bug.
#[derive(Clone, Copy)]
struct Cut<'a> {
    m: &'a Metrics,
    capped: usize,
    x: f32,
    y: f32,
    cols: usize,
    /// Content-space rectangles the grid is not allowed to fill, clearance
    /// already added: the centre block, and the cell the app's own button
    /// holds. Tiles flow around them; the tree never knows they are there.
    holes: &'a [Rect],
}

/// Whether one row of a box's cells has free ones on both sides of the hole.
///
/// That is the arrangement a box must not read across: tiles at the left of the
/// row, then a jump over the centre block, then more at the right, in reading
/// order and in no order at all to look at. A hole against a side of the box
/// leaves one run and reads fine.
fn straddles(holes: &[Rect], x: f32, y: f32, cols: usize, m: &Metrics) -> bool {
    let free = |col: usize| {
        let cell = Rect {
            x: x + col as f32 * (m.tile_w + m.gap),
            y,
            w: m.tile_w,
            h: m.tile_h,
        };
        !holes.iter().any(|hole| hole.overlaps(&cell))
    };
    let Some(blocked) = (0..cols).find(|&col| !free(col)) else {
        return false;
    };
    blocked > 0 && (blocked..cols).any(free)
}

/// Where the box stacked under one ending at `bottom` starts: the next row of
/// the panel's one lattice, plus whatever whole rows of clearance were asked
/// for.
///
/// Every tile in every box sits on that lattice - `search_h + padding`, then a
/// tile and a gap over and over - and it is measured from the top of the
/// content, never from the box a tile happens to be in. A box that started
/// where its neighbour ended plus a few pixels put its rows a fraction off
/// every other box's, and a row that is ten pixels out does not read as two
/// boxes being apart. It reads as the grid being crooked.
///
/// This is the same rule the centre block already lives by, and for the same
/// reason: on the lattice it is part of the grid, and off it every row it
/// grazes is spent on nothing.
fn next_row(bottom: f32, m: &Metrics) -> f32 {
    let origin = m.search_h + m.padding;
    let pitch = m.tile_h + m.gap;
    if pitch <= 0.0 {
        return bottom.max(origin);
    }
    // Rounded, not floored: a `section_gap` left at the old pixel value means
    // no clear row rather than a surprise one.
    let clear = (m.section_gap / pitch).round().max(0.0);
    // The epsilon is for a bottom already sitting exactly on a row, which comes
    // out of a multiplication and is one ulp either side of it.
    let row = (((bottom - origin) / pitch) - 1e-3).ceil().max(0.0);
    origin + (row + clear) * pitch
}

/// One pass over the tree. Returns the placement and the bottom edge the tree
/// reached, which is not the bottom of the panel when the centre hangs lower.
fn lay_out(
    sections: &[SectionShape],
    m: &Metrics,
    tree: &Option<Node>,
    capped: usize,
    cols: usize,
    reserve: Option<Rect>,
    home: Option<Rect>,
) -> (Placement, f32) {
    // Every slot up front, in section order. A box that claims a side is laid
    // out before the boxes that fill what it left, so appending would hand it
    // the first tiles as well - and the panel indexes its items in section
    // order. Titles and contents came apart exactly there.
    let slots = sections.iter().map(|s| s.count).sum();
    let blank = Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    let mut out = Placement {
        tiles: vec![blank; slots],
        headers: Vec::new(),
        bands: Vec::new(),
    };
    let top = m.search_h + m.padding;
    let holes: Vec<Rect> = [reserve, home]
        .into_iter()
        .flatten()
        .map(|r| clearance(r, m.gap))
        .collect();
    let bottom = tree.as_ref().map_or(top, |tree| {
        let cut = Cut { m, capped, x: m.padding, y: top, cols, holes: &holes };
        tree.place(sections, cut, &mut out)
    });
    (out, bottom)
}

/// How far down the content actually goes. The centre hangs below a short
/// bento, and the panel still has to be tall enough to hold it.
fn reaching(bottom: f32, reserve: Option<Rect>) -> f32 {
    reserve.map_or(bottom, |r| bottom.max(r.y + r.h))
}

/// Clear space around the centre, so a box whose rows do not line up with it
/// never sits flush against it.
///
/// Half a gap, not a whole one. The hole lands on whole cells of the grid, so
/// the row above it already ends a full gap clear and must not be counted as
/// blocked - that was the bug that filled the panel with space: a row
/// overlapping the block by ten pixels lost its middle columns exactly as a row
/// sitting squarely behind it did. Only a box with a row grid of its own, from
/// being stacked below something, comes closer than that.
fn clearance(reserve: Rect, gap: f32) -> Rect {
    Rect { y: reserve.y - gap / 2.0, h: reserve.h + gap, ..reserve }
}

/// The row grid the centre lines up with: where the first box's tiles start,
/// and how far apart its rows are.
///
/// Landing the hole on whole cells is what makes the block *part* of the grid -
/// the middle few squares of it, outlined - rather than something dropped on
/// top of it. Off the grid it costs every row it grazes, and the panel fills
/// with space that is not holding anything.
///
/// The first box, because boxes side by side start at the same y and share a
/// row grid, and side by side is the arrangement the centre lives in. A box
/// stacked below has a phase of its own and takes the loose fit.
fn row_grid(out: &Placement, m: &Metrics) -> Option<(f32, f32)> {
    let band = out.bands.iter().find(|band| band.count > 0)?;
    let first = out.tiles.get(band.first)?;
    Some((first.y, m.tile_h + m.gap))
}

/// Two reserves that describe the same place. Compared loosely because these
/// come out of a division that repeats: exact equality would let a half-pixel
/// disagreement run the settling loop out to its cap for nothing.
fn settled(before: Option<Rect>, after: Option<Rect>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(a), Some(b)) => (a.y - b.y).abs() < 0.5 && (a.x - b.x).abs() < 0.5,
        _ => false,
    }
}

/// The cell the app's own button holds: bottom right of the grid, one tile.
///
/// Reserved the way the centre block is, and for the same reason. The button is
/// the one control that is always in the same place - that is the whole of what
/// it is for - so a box drawing a tile behind it is a click that lands on the
/// wrong thing, and a box drawing its ring around it says the button is one of
/// that box's items.
///
/// On the grid, not `gap` in from the panel's corner. Off the grid it was eight
/// pixels adrift of the column and the row it sits in, which on a panel where
/// everything else lines up is the one thing that looks broken.
fn home_reserve(cols: usize, bottom: f32, m: &Metrics) -> Option<Rect> {
    (cols > 0 && bottom > m.tile_h).then(|| Rect {
        x: m.padding + (cols - 1) as f32 * (m.tile_w + m.gap),
        y: bottom - m.tile_h,
        w: m.tile_w,
        h: m.tile_h,
    })
}

/// The centre block's boxes, in the order they sit left to right.
///
/// Empty ones drop out, the same rule the tree uses: a half with nothing in it
/// is not a box, and leaving it in would hold width the rest could use.
fn center_boxes(sections: &[SectionShape]) -> Vec<usize> {
    let mut found: Vec<usize> = (0..sections.len())
        .filter(|&index| sections[index].center.is_some() && sections[index].count > 0)
        .collect();
    found.sort_by_key(|&index| sections[index].center.unwrap_or(0));
    found
}

/// How many columns each half of the centre gets, given what the panel has.
///
/// Normally each half gets what it asked for, and the block is the same width
/// on every panel - which is the whole of its worth. The budget only bites on a
/// panel too narrow to hold it, and there the halves give up a column each from
/// the widest rather than one of them disappearing: half a block in the middle
/// of the screen is worse than a smaller one.
///
/// The one place the block's width is decided. Both the rectangle it reserves
/// and the tiles laid into it come off this, so they cannot disagree - and a
/// disagreement is a hole the grid wraps around with nothing in it, or a block
/// hanging off the edge of the panel.
fn center_widths(sections: &[SectionShape], order: &[usize], budget: usize) -> Vec<usize> {
    let mut want: Vec<usize> = order
        .iter()
        .map(|&index| sections[index].columns.max(1))
        .collect();
    let mut total: usize = want.iter().sum();
    while total > budget && want.iter().any(|&w| w > 1) {
        let Some(widest) = want
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .map(|(index, _)| index)
        else {
            break;
        };
        want[widest] -= 1;
        total -= 1;
    }
    want
}

/// How many rows tall the block is: the tallest of its halves, so a half with
/// fewer slots leaves its bottom row empty rather than shortening the block.
fn center_rows(sections: &[SectionShape], order: &[usize], widths: &[usize]) -> usize {
    order
        .iter()
        .zip(widths)
        .map(|(&index, &across)| sections[index].count.div_ceil(across.max(1)))
        .max()
        .unwrap_or(0)
}

/// Where the centre block sits, in content space.
///
/// Snapped to whole columns of the grid every box shares, so nothing is left
/// with half a column to sit in. An odd number of spare columns rounds the
/// block to the left, which is a half-column off centre and the price of
/// keeping the wrap clean.
fn center_reserve(
    size: (usize, usize),
    m: &Metrics,
    cols: usize,
    panel_h: f32,
    align: Option<(f32, f32)>,
) -> Option<Rect> {
    let (across, rows) = size;
    if across == 0 || rows == 0 {
        return None;
    }
    let w = across as f32 * m.tile_w + (across - 1) as f32 * m.gap;
    let h = rows as f32 * m.tile_h + (rows - 1) as f32 * m.gap;
    let x = m.padding + (cols.saturating_sub(across) / 2) as f32 * (m.tile_w + m.gap);
    // The middle of what is on screen, not of the whole scroll. The panel opens
    // unscrolled, and the middle of the screen is the entire point of this.
    let floor = m.search_h + m.padding;
    let want = ((m.search_h + panel_h - h) / 2.0).max(floor);
    // Then to the nearest whole row, so the block occupies cells of the grid
    // rather than a rectangle that grazes them.
    let y = match align {
        Some((origin, step)) if step > 0.0 => {
            let rows_down = ((want - origin) / step).round().max(0.0);
            (origin + rows_down * step).max(floor)
        }
        _ => want,
    };
    Some(Rect { x, y, w, h })
}

/// Lay the centre block into the rectangle it claimed.
///
/// Runs after `stretch`, so the bands it appends sit past the tree's and
/// nothing that walks the tree by leaf order can reach them.
fn place_center(
    sections: &[SectionShape],
    m: &Metrics,
    order: &[usize],
    widths: &[usize],
    reserve: Rect,
    out: &mut Placement,
) {
    let mut x = reserve.x;
    for (&index, &box_cols) in order.iter().zip(widths) {
        // No hole to dodge: this box is the hole.
        let spot = Spot { m, x, y: reserve.y, cols: box_cols, holes: &[] };
        place_box(sections, index, spot, out);
        if let Some(band) = out.bands.last_mut() {
            band.center = true;
            // The half's whole rectangle, including the rows its shorter
            // neighbour left empty, so every point of the block belongs to one
            // half of it.
            band.rect = Rect { y: reserve.y, h: reserve.h, ..band.rect };
        }
        x += box_cols as f32 * (m.tile_w + m.gap);
    }
}

/// Split a column budget between two halves. An explicit share wins; otherwise
/// they divide it in proportion to what they hold. Neither half ever gets none.
fn divide(budget: usize, near: usize, far: usize, share: f32) -> usize {
    if budget < 2 {
        return 1;
    }
    let split = if share > 0.0 {
        (budget as f32 * share).round() as usize
    } else {
        let total = (near + far).max(1);
        ((budget * near) as f32 / total as f32).round() as usize
    };
    split.clamp(1, budget - 1)
}

/// Build the tree from what each section says about where it sits.
///
/// Sections naming a place are put there first, so the shape is theirs. The
/// rest stack into whatever those cuts left over - which is why "left" alone is
/// a complete instruction: one box down the left, everything else filling the
/// right.
fn plan(sections: &[SectionShape]) -> Option<Node> {
    let mut tree = Node::Leaf(EMPTY);
    let mut stacked: Option<Node> = None;

    for (index, section) in sections.iter().enumerate() {
        // The centre is placed by hand afterwards. The tree is planned as if it
        // were not there, which is exactly what makes the rest wrap around it
        // instead of being cut by it.
        if section.count == 0 || section.center.is_some() {
            continue;
        }
        match &section.at {
            Some(at) => tree.insert(&at.cuts, at.share, index),
            None => {
                stacked = Some(match stacked {
                    None => Node::Leaf(index),
                    Some(above) => Node::Cut {
                        axis: Axis::Down,
                        share: 0.0,
                        near: Box::new(above),
                        far: Box::new(Node::Leaf(index)),
                    },
                });
            }
        }
    }

    if let Some(rest) = stacked {
        let bare = tree == Node::Leaf(EMPTY);
        // The other half of a cut somebody made is exactly where the unplaced
        // sections belong.
        if let Some(hole) = tree.hole() {
            *hole = rest;
        } else if bare {
            tree = rest;
        } else {
            tree = Node::Cut {
                axis: Axis::Down,
                share: 0.0,
                near: Box::new(tree),
                far: Box::new(rest),
            };
        }
    }
    tree.prune()
}

/// What `compute` fills in as it places boxes.
struct Placement {
    tiles: Vec<Rect>,
    headers: Vec<Header>,
    bands: Vec<Band>,
}

/// Where one box goes. A value for the same reason `Cut` is one: these five
/// travel together, and a swapped pair of them is a silent bug.
#[derive(Clone, Copy)]
struct Spot<'a> {
    m: &'a Metrics,
    x: f32,
    y: f32,
    /// Tile columns the box has been given.
    cols: usize,
    /// Content-space rectangles this box may not fill, clearance included.
    /// Empty for the centre block itself, which is one of them.
    holes: &'a [Rect],
}

/// Lay one box out where its `Spot` says. Returns its bottom edge.
///
/// The one place a box becomes rectangles, so no two arrangements can drift
/// apart in how a header sits over its tiles.
fn place_box(
    sections: &[SectionShape],
    index: usize,
    spot: Spot<'_>,
    out: &mut Placement,
) -> f32 {
    let Spot { m, x, y, cols: box_cols, holes } = spot;
    let section = &sections[index];
    let box_cols = box_cols.max(1);
    let box_w = box_cols as f32 * m.tile_w + (box_cols - 1) as f32 * m.gap;
    // Where this box's tiles sit in the flat run: everything configured ahead
    // of it, however the boxes ended up arranged on screen.
    let first_tile: usize = sections[..index].iter().map(|s| s.count).sum();
    let mut inner = y;

    // A box that fits on one row is a bar, and a bar with the hole in the
    // middle of its row is unreadable: the six moves came out as four down the
    // left and three up on the right - in reading order, and in no order at all
    // to look at. It slides down past the hole instead.
    //
    // Only when the row is *straddled*, though. A hole against one side of the
    // box leaves a single run of free cells, and a run is something a bar can
    // wrap into - which beats sliding past and leaving those cells empty, which
    // is what put a three-tile box two rows below the space it fitted in.
    if !holes.is_empty() && section.count > 0 && section.count <= box_cols {
        // The holes are finite, so a row below them is always clear.
        for _ in 0..64 {
            if !straddles(holes, x, inner, box_cols, m) {
                break;
            }
            inner += m.tile_h + m.gap;
        }
    }

    // Left to right, then down, stepping over whatever the centre block is
    // standing on. Reading order survives the hole: the tiles flow around it
    // rather than through it, which is what a bento does when something is
    // pinned in the middle of it.
    //
    // Slot, not tile: a skipped slot costs a position but not an item, which
    // is why these are counted apart.
    let grid_y = inner;
    let mut placed = 0;
    let mut slot = 0;
    let mut tile_rows = 0;
    // The hole is finite, so any row below it is free and this always ends.
    // The ceiling is only so a metric nobody expected cannot spin the UI
    // thread, which is the one failure that looks like a broken PC.
    let ceiling = section.count.saturating_mul(4) + 64;
    while placed < section.count && slot < ceiling {
        let rect = Rect {
            x: x + (slot % box_cols) as f32 * (m.tile_w + m.gap),
            y: inner + (slot / box_cols) as f32 * (m.tile_h + m.gap),
            w: m.tile_w,
            h: m.tile_h,
        };
        slot += 1;
        if holes.iter().any(|hole| hole.overlaps(&rect)) {
            continue;
        }
        out.tiles[first_tile + placed] = rect;
        placed += 1;
        tile_rows = slot.div_ceil(box_cols);
    }
    inner += tile_rows as f32 * m.tile_h + (tile_rows.saturating_sub(1)) as f32 * m.gap;

    // What the ring encloses. Every cell of every row the box reached, minus
    // the ones the centre block is standing on - so a ragged last row is
    // squared off and only the block puts a bite in the shape.
    let mut filled = Vec::with_capacity(tile_rows * box_cols);
    for row in 0..tile_rows {
        for col in 0..box_cols {
            let cell = Rect {
                x: x + col as f32 * (m.tile_w + m.gap),
                y: grid_y + row as f32 * (m.tile_h + m.gap),
                w: m.tile_w,
                h: m.tile_h,
            };
            filled.push(!holes.iter().any(|hole| hole.overlaps(&cell)));
        }
    }

    // The title rides the ring's top edge rather than taking a row above it.
    // A section costs a header plus a whole row even for one tile, and that
    // row is what stopped the panel being split into the boxes it wants to be
    // - so the title stops costing one. It is a mark on the ring now, in the
    // ring's own colour, and the colour is what says which box this is.
    //
    // After the tiles are placed, because a bar slides down past the hole and
    // the label goes where the tiles ended up.
    //
    // The centre block never wears one. It is the most valuable space on the
    // panel, and a title would spend it saying what the icons already say.
    if !section.title.is_empty() && m.header_h > 0.0 && section.center.is_none() {
        // On the ring's own top left corner, which is not the box's: the block
        // can take the whole start of a box's first row, and a title left at
        // the box's edge then floats over the block with none of its own tiles
        // anywhere near it.
        let start = filled.iter().position(|f| *f).unwrap_or(0);
        let (row, col) = (start / box_cols, start % box_cols);
        out.headers.push(Header {
            title: section.title.clone(),
            rect: Rect {
                x: x + col as f32 * (m.tile_w + m.gap) + m.header_gap,
                // Centred on the line the ring is drawn along, which sits a
                // quarter of a gap out from the tiles.
                y: grid_y + row as f32 * (m.tile_h + m.gap) - m.gap / 4.0 - m.header_h / 2.0,
                w: (box_w - col as f32 * (m.tile_w + m.gap) - m.header_gap).max(0.0),
                h: m.header_h,
            },
            band: out.bands.len(),
        });
    }

    out.bands.push(Band {
        section: index,
        first: first_tile,
        count: section.count,
        rect: Rect { x, y, w: box_w, h: inner - y },
        cols: box_cols,
        center: false,
        cells: Cells { x, y: grid_y, cols: box_cols, rows: tile_rows, filled },
    });
    inner
}

/// Grow every box out to fill the rectangle its subtree owns, so the panel is
/// covered with nothing in between: a click in the space beside or below a box
/// should still mean *that* box, not nothing.
///
/// Walked down the tree rather than by looking for the nearest rectangle. The
/// tree already partitions the panel exactly; guessing from proximity produced
/// boxes that overlapped, which meant a click could belong to two of them.
fn stretch(node: &Node, bands: &mut [Band], first: usize, rect: Rect) {
    match node {
        Node::Leaf(_) => bands[first].rect = rect,
        Node::Cut { axis, near, far, .. } => {
            let split = leaf_count(near);
            // Where the two halves actually ended up, before either is grown.
            let (a, b) = (
                covering(&bands[first..first + split]),
                covering(&bands[first + split..first + split + leaf_count(far)]),
            );

            let (near_rect, far_rect) = match axis {
                Axis::Across => {
                    let edge = (a.x + a.w + b.x) / 2.0;
                    (
                        Rect { w: edge - rect.x, ..rect },
                        Rect { x: edge, w: rect.x + rect.w - edge, ..rect },
                    )
                }
                Axis::Down => {
                    let edge = (a.y + a.h + b.y) / 2.0;
                    (
                        Rect { h: edge - rect.y, ..rect },
                        Rect { y: edge, h: rect.y + rect.h - edge, ..rect },
                    )
                }
            };
            stretch(near, bands, first, near_rect);
            stretch(far, bands, first + split, far_rect);
        }
    }
}

fn leaf_count(node: &Node) -> usize {
    match node {
        Node::Leaf(_) => 1,
        Node::Cut { near, far, .. } => leaf_count(near) + leaf_count(far),
    }
}

/// The rectangle around a run of bands.
fn covering(bands: &[Band]) -> Rect {
    let left = bands.iter().map(|b| b.rect.x).fold(f32::MAX, f32::min);
    let top = bands.iter().map(|b| b.rect.y).fold(f32::MAX, f32::min);
    let right = bands.iter().map(|b| b.rect.x + b.rect.w).fold(f32::MIN, f32::max);
    let bottom = bands.iter().map(|b| b.rect.y + b.rect.h).fold(f32::MIN, f32::max);
    Rect { x: left, y: top, w: right - left, h: bottom - top }
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
    /// A real 1080p work area, for the rules that only mean anything against
    /// the shape of an actual panel.
    const SCREEN: Rect = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1040.0 };

    /// The working config's grid: ten columns, a three-a-side centre block.
    fn live() -> Metrics {
        Metrics {
            tile_w: 140.0,
            tile_h: 100.0,
            gap: 10.0,
            padding: 18.0,
            max_fraction: 0.92,
            max_cols: 10,
            fixed_cols: 0,
            header_h: 16.0,
            header_gap: 14.0,
            section_gap: 0.0,
            search_h: 0.0,
        }
    }

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
            header_gap: 0.0,
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

    /// A section that says nothing about where it sits.
    fn shape(title: &str, count: usize) -> SectionShape {
        SectionShape { title: title.into(), count, at: None, columns: 0, center: None }
    }

    /// A section placed by path, spelled the way a config would spell it.
    fn at(title: &str, count: usize, spec: &str) -> SectionShape {
        SectionShape {
            title: title.into(),
            count,
            at: At::parse(spec),
            columns: 0,
            center: None,
        }
    }

    /// One half of the centre block: a fixed number of slots, a fixed width.
    fn middle(half: usize, count: usize, columns: usize) -> SectionShape {
        SectionShape {
            title: String::new(),
            count,
            at: None,
            columns,
            center: Some(half),
        }
    }

    // --- edit options ---

    const PANEL: Rect = Rect { x: 0.0, y: 0.0, w: 1376.0, h: 660.0 };

    #[test]
    fn every_option_gets_a_tile_sized_square() {
        let placed = controls(PANEL, 140.0, 100.0, 10.0);
        assert_eq!(placed.len(), CONTROLS.len());
        for (control, rect) in &placed {
            assert_eq!((rect.w, rect.h), (140.0, 100.0), "{control:?} is not tile sized");
            assert!(!control.label().is_empty(), "{control:?} has no words");
            assert!(!control.glyph().is_empty(), "{control:?} has no mark");
        }
    }

    #[test]
    fn the_options_are_centred_in_the_panel() {
        // Middle of the screen is the cheapest place for a gaze pointer to
        // reach, so this is the property that matters most.
        let placed = controls(PANEL, 140.0, 100.0, 10.0);
        let left = placed.iter().map(|(_, r)| r.x).fold(f32::MAX, f32::min);
        let right = placed.iter().map(|(_, r)| r.x + r.w).fold(0.0, f32::max);
        let top = placed.iter().map(|(_, r)| r.y).fold(f32::MAX, f32::min);
        let bottom = placed.iter().map(|(_, r)| r.y + r.h).fold(0.0, f32::max);

        assert!((left - (PANEL.w - right)).abs() < 0.5, "left {left} vs right {}", PANEL.w - right);
        assert!((top - (PANEL.h - bottom)).abs() < 0.5, "top {top} vs bottom {}", PANEL.h - bottom);
    }

    #[test]
    fn options_do_not_overlap() {
        let placed = controls(PANEL, 140.0, 100.0, 10.0);
        for (a, (_, one)) in placed.iter().enumerate() {
            for (_, other) in placed.iter().skip(a + 1) {
                let apart = one.x + one.w <= other.x
                    || other.x + other.w <= one.x
                    || one.y + one.h <= other.y
                    || other.y + other.h <= one.y;
                assert!(apart, "{one:?} overlaps {other:?}");
            }
        }
    }

    #[test]
    fn options_stay_on_a_panel_too_small_for_them() {
        // A panel narrower than the option grid still has to put every tile
        // somewhere clickable rather than off the left edge.
        let tiny = Rect { x: 0.0, y: 0.0, w: 300.0, h: 200.0 };
        let placed = controls(tiny, 140.0, 100.0, 10.0);
        for (control, rect) in &placed {
            assert!(rect.x >= 0.0 && rect.y >= 0.0, "{control:?} sits off the panel");
        }
    }

    #[test]
    fn a_plate_drawn_round_the_options_covers_all_of_them() {
        // The options sit on a plate derived from where they landed. If the two
        // ever disagree, a square hangs off the edge of its own backing.
        let placed = controls(PANEL, 140.0, 100.0, 10.0);
        let margin = 10.0;
        let left = placed.iter().map(|(_, r)| r.x).fold(f32::MAX, f32::min) - margin;
        let top = placed.iter().map(|(_, r)| r.y).fold(f32::MAX, f32::min) - margin;
        let right = placed.iter().map(|(_, r)| r.x + r.w).fold(f32::MIN, f32::max) + margin;
        let bottom = placed.iter().map(|(_, r)| r.y + r.h).fold(f32::MIN, f32::max) + margin;

        for (control, rect) in &placed {
            assert!(rect.x >= left, "{control:?} juts out to the left");
            assert!(rect.y >= top, "{control:?} juts out above");
            assert!(rect.x + rect.w <= right, "{control:?} juts out to the right");
            assert!(rect.y + rect.h <= bottom, "{control:?} juts out below");
        }
        assert!(right - left <= PANEL.w, "the plate is wider than the panel");
    }

    #[test]
    fn the_plate_covers_every_square_and_the_gaps_between_them() {
        // What the panel hit-tests against, so a click on a greyed square, or
        // in the space beside one, does not fall through to the box behind.
        let panel = Rect { x: 0.0, y: 0.0, w: 1376.0, h: 632.0 };
        let placed = controls(panel, 140.0, 100.0, 10.0);
        let margin = 10.0;
        let left = placed.iter().map(|(_, r)| r.x).fold(f32::MAX, f32::min) - margin;
        let top = placed.iter().map(|(_, r)| r.y).fold(f32::MAX, f32::min) - margin;
        let right = placed.iter().map(|(_, r)| r.x + r.w).fold(f32::MIN, f32::max) + margin;
        let bottom = placed.iter().map(|(_, r)| r.y + r.h).fold(f32::MIN, f32::max) + margin;
        let plate = Rect { x: left, y: top, w: right - left, h: bottom - top };

        for (control, rect) in &placed {
            assert!(plate.contains(rect.x, rect.y), "{control:?} starts off the plate");
            assert!(
                plate.contains(rect.x + rect.w - 0.01, rect.y + rect.h - 0.01),
                "{control:?} ends off the plate"
            );
        }
        // And a point in the gap between two squares is still the plate's.
        let first = placed[0].1;
        assert!(plate.contains(first.x + first.w + 2.0, first.y + 2.0));
    }

    // --- the app's own button and its menu ---

    #[test]
    fn the_home_button_sits_in_the_same_corner_whatever_the_panel() {
        // The one control that never moves. Someone pointing with their eyes
        // learns where it is once.
        let m = metrics();
        for count in [1, 7, 24, 61] {
            let l = Layout::compute(&[shape("Apps", count)], m, WORK);
            let button = l.home_rect();
            assert!(button.x + button.w <= l.panel.w + 0.01, "off the right edge");
            assert!(button.y + button.h <= l.panel.h + 0.01, "off the bottom edge");
            assert!(button.x >= 0.0 && button.y >= 0.0);
            assert_eq!((button.w, button.h), (m.tile_w, m.tile_h), "not tile sized");
        }
    }

    #[test]
    fn the_home_button_is_a_cell_of_the_grid() {
        // On the lattice like everything else, and the same inset from the
        // panel's corner that a tile has. It used to sit `gap` in, which on a
        // panel where everything else lines up is eight pixels of adrift.
        let m = metrics();
        let l = Layout::compute(&[shape("Apps", 14), shape("Active", 3)], m, WORK);
        let button = l.home_rect();

        let col = (button.x - m.padding) / (m.tile_w + m.gap);
        assert!((col - col.round()).abs() < 0.01, "the button is off the columns");
        assert_eq!(button.x + button.w, l.panel.w - m.padding, "not inset like a tile");
        assert_eq!(button.y + button.h, l.panel.h - m.padding);

        // And it is the last column, not somewhere in the middle.
        assert_eq!(col.round() as usize, l.cols - 1);
    }

    #[test]
    fn nothing_is_laid_into_the_corner_the_button_holds() {
        // A tile behind it is a click that lands on the wrong thing, and a ring
        // drawn around it says the button is one of that box's items.
        let m = metrics();
        for count in [4, 9, 14, 23, 40] {
            let l = Layout::compute(&[shape("Apps", count), shape("Active", 3)], m, WORK);
            let button = l.home_rect();
            // Content space: the two agree whenever the content fits, which it
            // does at these counts.
            assert_eq!(l.max_scroll, 0.0, "{count} items scrolled; test needs a panel that fits");

            for n in 0..l.tile_count() {
                assert!(
                    !l.tile_rect(n, 0.0).overlaps(&button),
                    "{count} items: tile {n} sits behind the button"
                );
            }
            for band in 0..l.bands().len() {
                for ring in l.band_ring(band, 0.0) {
                    for (x, y) in ring {
                        assert!(
                            !button.contains(x, y),
                            "{count} items: band {band}'s ring runs through the button"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_big_menu_is_centred_and_tile_sized() {
        let panel = Rect { x: 0.0, y: 0.0, w: 1376.0, h: 632.0 };
        let placed = commands(panel, 140.0, 100.0, 10.0);
        assert_eq!(placed.len(), COMMANDS.len());

        let left = placed.iter().map(|(_, r)| r.x).fold(f32::MAX, f32::min);
        let right = placed.iter().map(|(_, r)| r.x + r.w).fold(0.0, f32::max);
        assert!((left - (panel.w - right)).abs() < 0.5, "not centred across");
        for (command, rect) in &placed {
            assert_eq!((rect.w, rect.h), (140.0, 100.0), "{command:?} is not tile sized");
            assert!(!command.label().is_empty());
        }
    }

    #[test]
    fn the_menu_and_the_edit_options_are_laid_out_the_same_way() {
        // Both are big squares in the middle. One helper, so they cannot drift.
        let panel = Rect { x: 0.0, y: 0.0, w: 1376.0, h: 632.0 };
        let menu = centred_grid(panel, COMMANDS.len(), 140.0, 100.0, 10.0);
        let from_commands: Vec<Rect> = commands(panel, 140.0, 100.0, 10.0)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect();
        assert_eq!(menu, from_commands);
    }

    // --- which options apply ---

    /// A box holding the left side, with room to move in every direction.
    fn placed() -> BoxState {
        BoxState {
            shown: 8,
            total: 32,
            at: At::parse("left@50"),
            boxes: 3,
            at_stack: 0,
            stack: 2,
            cols: 4,
            panel_cols: 9,
            share_now: 0.5,
        }
    }

    /// A box with no side of its own, sitting in the leftover stack.
    fn loose() -> BoxState {
        BoxState { at: None, at_stack: 1, stack: 3, ..placed() }
    }

    fn offered(state: &BoxState) -> Vec<Control> {
        CONTROLS.iter().copied().filter(|c| c.allowed(state)).collect()
    }

    #[test]
    fn the_side_a_box_holds_is_the_button_that_gives_it_back() {
        // One button, both directions. There is no separate "un-claim": the
        // lit square shows where the box is, and clicking it undoes that.
        let state = placed();
        assert!(Control::ClaimLeft.holds(&state), "the left square is the lit one");
        assert!(Control::ClaimLeft.allowed(&state), "and it is still clickable");
        assert_eq!(Control::ClaimLeft.wording(&state).1, "Leave left");

        assert!(!Control::ClaimRight.holds(&state));
        assert_eq!(Control::ClaimRight.wording(&state).1, "Full right");
    }

    #[test]
    fn a_box_holding_no_side_lights_nothing() {
        let state = loose();
        for control in CONTROLS {
            assert!(!control.holds(&state), "{control:?} is lit with no side held");
        }
    }

    #[test]
    fn the_last_tile_cannot_be_taken_away() {
        // `max_items = 0` means "all of them", so one fewer than one used to
        // show everything. The button is simply not offered now.
        let state = BoxState { shown: 1, ..placed() };
        assert!(!Control::Fewer.allowed(&state));
        assert!(Control::More.allowed(&state));
    }

    #[test]
    fn a_box_showing_everything_it_has_cannot_show_more() {
        let state = BoxState { shown: 32, total: 32, ..placed() };
        assert!(!Control::More.allowed(&state));
        assert!(Control::Fewer.allowed(&state));
    }

    #[test]
    fn sizing_needs_a_cut_to_size() {
        // "Bigger" is a bigger share of a cut. A box that holds no side is not
        // sitting on one, so there is nothing to take more of.
        let state = loose();
        assert!(!Control::Grow.allowed(&state));
        assert!(!Control::Shrink.allowed(&state));
        assert!(Control::ClaimLeft.allowed(&state), "it can still claim a side");
    }

    #[test]
    fn a_box_that_was_never_resized_can_still_be_resized() {
        // `at = "left"` with no share is a box sized by its contents. Reading
        // the share off the config gave 0.0, which looked like "already as
        // small as it goes" and greyed the button out for every box that had
        // never been touched.
        let untouched = BoxState {
            at: At::parse("left"),
            cols: 5,
            panel_cols: 9,
            share_now: 5.0 / 9.0,
            ..placed()
        };
        assert_eq!(untouched.at.as_ref().unwrap().share, 0.0, "nothing written down");
        assert!(Control::Shrink.allowed(&untouched), "it is 5 of 9 columns wide");
        assert!(Control::Grow.allowed(&untouched));
    }

    #[test]
    fn resizing_across_moves_by_a_whole_column() {
        // A 5% nudge often did not cross a column boundary, so the click
        // appeared to do nothing at all.
        let state = BoxState {
            at: At::parse("left"),
            cols: 5,
            panel_cols: 9,
            share_now: 5.0 / 9.0,
            ..placed()
        };
        let narrower = Control::Shrink.resized(&state).unwrap();
        let wider = Control::Grow.resized(&state).unwrap();
        assert_eq!((narrower * 9.0).round() as usize, 4);
        assert_eq!((wider * 9.0).round() as usize, 6);
    }

    #[test]
    fn a_box_cannot_be_narrowed_out_of_existence_or_take_the_whole_panel() {
        let thin = BoxState { at: At::parse("left"), cols: 1, panel_cols: 9, ..placed() };
        assert!(!Control::Shrink.allowed(&thin));
        assert!(Control::Grow.allowed(&thin));

        let fat = BoxState { at: At::parse("left"), cols: 8, panel_cols: 9, ..placed() };
        assert!(!Control::Grow.allowed(&fat), "the other half is owed a column");
        assert!(Control::Shrink.allowed(&fat));
    }

    #[test]
    fn resizing_down_the_panel_steps_by_share() {
        // Heights are not counted in columns, so a top or bottom box moves by
        // a fraction instead.
        let state = BoxState {
            at: At::parse("bottom"),
            cols: 9,
            panel_cols: 9,
            share_now: 0.5,
            ..placed()
        };
        let taller = Control::Grow.resized(&state).unwrap();
        let shorter = Control::Shrink.resized(&state).unwrap();
        assert!(taller > 0.5 && taller <= 0.9);
        assert!((0.1..0.5).contains(&shorter));

        let squat = BoxState { share_now: 0.05, ..state };
        assert!(!Control::Shrink.allowed(&squat));
        assert!(Control::Grow.allowed(&squat));
    }

    #[test]
    fn the_button_and_the_action_agree_about_the_edge() {
        // Whenever the rule says a size option applies, it also has a share to
        // write. A button that is offered and then does nothing is the bug
        // this pairing prevents.
        for at in ["left", "left@50", "bottom", "bottom@30", "right/top"] {
            for cols in [1usize, 3, 8] {
                let state = BoxState {
                    at: At::parse(at),
                    cols,
                    panel_cols: 9,
                    share_now: cols as f32 / 9.0,
                    ..placed()
                };
                for control in [Control::Grow, Control::Shrink] {
                    if control.allowed(&state) {
                        assert!(
                            control.resized(&state).is_some(),
                            "{control:?} offered for {at} at {cols} cols but writes nothing"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn moving_walks_the_leftover_stack_not_the_whole_list() {
        // A box holding a side is not in the stack, so it has nowhere to move.
        assert!(!Control::MoveUp.allowed(&placed()));
        assert!(!Control::MoveDown.allowed(&placed()));

        let middle = loose();
        assert!(Control::MoveUp.allowed(&middle));
        assert!(Control::MoveDown.allowed(&middle));

        let first = BoxState { at_stack: 0, ..loose() };
        assert!(!Control::MoveUp.allowed(&first));
        assert!(Control::MoveDown.allowed(&first));

        let last = BoxState { at_stack: 2, stack: 3, ..loose() };
        assert!(Control::MoveUp.allowed(&last));
        assert!(!Control::MoveDown.allowed(&last));
    }

    #[test]
    fn a_lone_box_has_nowhere_to_be_sent() {
        // Nothing to divide the panel with, so every arrangement is the same
        // arrangement.
        let alone = BoxState { boxes: 1, ..placed() };
        assert_eq!(offered(&alone), [Control::Done]);
    }

    #[test]
    fn done_is_always_offered() {
        // Whatever else is greyed out, there is always a way to stop editing.
        let stuck = BoxState {
            shown: 1,
            total: 1,
            at: None,
            boxes: 1,
            at_stack: 0,
            stack: 1,
            cols: 1,
            panel_cols: 1,
            share_now: 1.0,
        };
        assert_eq!(offered(&stuck), [Control::Done]);
    }

    #[test]
    fn bigger_says_which_way_it_will_go() {
        // The complaint this fixes: one button reading "Bigger" for a box that
        // grows wider and a box that grows taller.
        let across = BoxState { at: At::parse("left@50"), ..placed() };
        assert_eq!(Control::Grow.wording(&across).1, "Wider");
        assert_eq!(Control::Shrink.wording(&across).1, "Narrower");

        let down = BoxState { at: At::parse("bottom@50"), ..placed() };
        assert_eq!(Control::Grow.wording(&down).1, "Taller");
        assert_eq!(Control::Shrink.wording(&down).1, "Shorter");

        // And the glyphs go with them.
        assert_ne!(
            Control::Grow.wording(&across).0,
            Control::Grow.wording(&down).0
        );
    }

    #[test]
    fn only_the_size_options_and_the_held_side_change_their_wording() {
        // Everything else means the same thing wherever the box sits, so it
        // reads the same. A square whose words move around is one you have to
        // re-read every time.
        let state = placed();
        for control in CONTROLS {
            let changes = matches!(control, Control::Grow | Control::Shrink)
                || control.holds(&state);
            if changes {
                continue;
            }
            assert_eq!(control.wording(&state), (control.glyph(), control.label()));
        }
    }

    // --- the tree ---

    #[test]
    fn a_path_parses_and_spells_back() {
        let parsed = At::parse("right/top").unwrap();
        assert_eq!(parsed.cuts, [Side::Right, Side::Top]);
        assert_eq!(parsed.share, 0.0);
        assert_eq!(parsed.spell(), "right/top");

        let pinned = At::parse("left@35").unwrap();
        assert_eq!(pinned.cuts, [Side::Left]);
        assert!((pinned.share - 0.35).abs() < 0.001);
        assert_eq!(pinned.spell(), "left@35");
    }

    #[test]
    fn nonsense_in_a_path_costs_one_section_its_place_not_the_panel() {
        assert!(At::parse("sideways").is_none());
        assert!(At::parse("").is_none());
        // The section still shows up; it just fills the rest.
        let l = Layout::compute(&[shape("A", 4), shape("B", 4)], metrics(), WORK);
        assert_eq!(l.bands().len(), 2);
    }

    #[test]
    fn one_box_down_the_left_and_the_rest_beside_it() {
        // The shape a flat list of rows cannot say, and the reason for the tree.
        let sections = [at("Side", 6, "left"), shape("Top", 4), shape("Bottom", 4)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let (side, top, bottom) = (&l.bands()[0], &l.bands()[1], &l.bands()[2]);

        assert!(top.rect.x >= side.rect.x + side.rect.w, "the rest is not beside the side");
        assert_eq!(top.rect.x, bottom.rect.x, "the rest is not one column");
        assert!(bottom.rect.y > top.rect.y, "the rest does not stack");
    }

    #[test]
    fn a_tile_belongs_to_the_section_it_was_configured_under() {
        // The panel flattens its items in section order and looks a rect up by
        // that flat index. Boxes are laid out in tree order, which puts a box
        // that claims a side first however far down the list it is configured.
        // While tiles were appended as they were placed, the two orders came
        // apart and every box drew somebody else's items under its own title.
        let sections = [shape("First", 3), at("Placed", 2, "right@30"), shape("Last", 4)];
        let l = Layout::compute(&sections, metrics(), WORK);

        let mut flat = 0;
        for (index, section) in sections.iter().enumerate() {
            let band = l
                .bands()
                .iter()
                .find(|band| band.section == index)
                .unwrap_or_else(|| panic!("{} has no band", section.title));
            assert_eq!(
                band.first, flat,
                "{}'s tiles do not start where its items do",
                section.title
            );
            for n in 0..section.count {
                let tile = l.tile_rect(flat + n, 0.0);
                assert!(
                    band.rect.contains(tile.x, tile.y),
                    "{}'s tile {n} at {tile:?} is outside its own box {:?}",
                    section.title,
                    band.rect
                );
            }
            flat += section.count;
        }
    }

    #[test]
    fn a_header_names_the_section_whose_tiles_are_under_it() {
        let sections = [shape("First", 3), at("Placed", 2, "right@30"), shape("Last", 4)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let bands = l.bands();

        for (title, _, band) in l.headers(0.0) {
            let section = &sections[bands[band].section];
            assert_eq!(title, section.title, "a header sits over the wrong box");
        }
    }

    #[test]
    fn the_side_runs_the_whole_height() {
        // What makes it the side of the panel rather than a box in a corner.
        let sections = [at("Side", 2, "left"), shape("Top", 8), shape("Bottom", 8)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let side = &l.bands()[0];
        assert!(
            (side.rect.y + side.rect.h - l.content_h).abs() < 0.5,
            "the side stops at {} but the content runs to {}",
            side.rect.y + side.rect.h,
            l.content_h
        );
    }

    #[test]
    fn the_remainder_can_be_cut_again() {
        // "One vertical all the way, then the remainder split in half."
        let sections = [
            at("Side", 6, "left"),
            at("Top", 4, "right/top"),
            at("Bottom", 4, "right/bottom"),
        ];
        let l = Layout::compute(&sections, metrics(), WORK);
        let (side, top, bottom) = (&l.bands()[0], &l.bands()[1], &l.bands()[2]);

        assert!(top.rect.x >= side.rect.x + side.rect.w);
        assert_eq!(top.rect.x, bottom.rect.x);
        assert!(bottom.rect.y > top.rect.y);
    }

    #[test]
    fn a_share_pins_a_side_to_its_fraction() {
        let wide = Layout::compute(
            &[at("Side", 6, "left@70"), shape("Rest", 6)],
            metrics(),
            WORK,
        );
        let narrow = Layout::compute(
            &[at("Side", 6, "left@20"), shape("Rest", 6)],
            metrics(),
            WORK,
        );
        assert!(
            wide.bands()[0].cols > narrow.bands()[0].cols,
            "70% ({}) should beat 20% ({})",
            wide.bands()[0].cols,
            narrow.bands()[0].cols
        );
    }

    #[test]
    fn naming_either_side_of_a_cut_describes_the_same_panel() {
        // "left@30" and "right@70" are one 30/70 cut said two ways. The box
        // lands on opposite sides of it, but the panel is the same panel.
        let from_left = Layout::compute(&[at("A", 6, "left@30"), shape("B", 6)], metrics(), WORK);
        let from_right = Layout::compute(&[shape("B", 6), at("A", 6, "right@70")], metrics(), WORK);

        let widths = |l: &Layout| {
            let mut cols: Vec<usize> = l.bands().iter().map(|band| band.cols).collect();
            cols.sort_unstable();
            cols
        };
        assert_eq!(widths(&from_left), widths(&from_right));

        // And the box that named a share is the one that got it.
        let a_left = from_left.bands().iter().find(|b| b.section == 0).unwrap();
        let a_right = from_right.bands().iter().find(|b| b.section == 1).unwrap();
        assert!(a_left.cols < a_right.cols, "30% should be narrower than 70%");
    }

    #[test]
    fn neither_half_of_a_cut_is_ever_squeezed_out() {
        for spec in ["left@5", "left@95"] {
            let l = Layout::compute(&[at("A", 9, spec), shape("B", 9)], metrics(), WORK);
            for band in l.bands() {
                assert!(band.cols >= 1, "{spec} squeezed a box out");
            }
        }
    }

    #[test]
    fn two_boxes_claiming_one_spot_stack_rather_than_one_vanishing() {
        let sections = [at("A", 4, "left"), at("B", 4, "left"), shape("C", 4)];
        let l = Layout::compute(&sections, metrics(), WORK);
        assert_eq!(l.bands().len(), 3, "a box was dropped");
    }

    #[test]
    fn nothing_placed_is_the_plain_stacked_panel() {
        // The default has to survive the tree untouched.
        let l = Layout::compute(&[shape("A", 7), shape("B", 3)], metrics(), WORK);
        let (a, b) = (&l.bands()[0], &l.bands()[1]);
        assert_eq!(a.rect.x, b.rect.x, "the default stopped stacking");
        assert!(b.rect.y > a.rect.y);
        assert_eq!(a.cols, l.cols);
    }

    #[test]
    fn boxes_never_overlap() {
        // Two boxes claiming the same pixel means a click belongs to both, and
        // whichever the scan happened to find first wins.
        let sections = [
            at("Side", 6, "left@40"),
            at("Top", 3, "right/top"),
            shape("Rest", 5),
        ];
        let l = Layout::compute(&sections, metrics(), WORK);
        for (n, one) in l.bands().iter().enumerate() {
            for other in l.bands().iter().skip(n + 1) {
                let apart = one.rect.x + one.rect.w <= other.rect.x + 0.01
                    || other.rect.x + other.rect.w <= one.rect.x + 0.01
                    || one.rect.y + one.rect.h <= other.rect.y + 0.01
                    || other.rect.y + other.rect.h <= one.rect.y + 0.01;
                assert!(apart, "{:?} overlaps {:?}", one.rect, other.rect);
            }
        }
    }

    #[test]
    fn every_box_keeps_its_own_tiles_inside_it() {
        // The band is what a click resolves to, so a tile drawn outside its own
        // band is a tile that cannot be clicked.
        let sections = [
            at("Side", 6, "left@40"),
            at("Top", 3, "right/top"),
            shape("Rest", 5),
        ];
        let l = Layout::compute(&sections, metrics(), WORK);
        for band in l.bands() {
            for n in band.first..band.first + band.count {
                let tile = l.tile_rect(n, 0.0);
                assert!(
                    tile.x >= band.rect.x - 0.01
                        && tile.x + tile.w <= band.rect.x + band.rect.w + 0.01,
                    "tile {n} at {} escapes its box at {}",
                    tile.x,
                    band.rect.x
                );
            }
        }
    }

    #[test]
    fn tiles_read_left_to_right_wherever_the_box_sits() {
        // The complaint the tree was built for: a box whose icons ran down
        // instead of across. Reading order belongs to the box, not its place.
        for spec in ["left", "right/top", "bottom"] {
            let l = Layout::compute(&[at("Box", 6, spec), shape("Rest", 12)], metrics(), WORK);
            let band = l.bands().iter().find(|b| b.section == 0).unwrap();
            if band.cols < 2 {
                continue;
            }
            let first = l.tile_rect(band.first, 0.0);
            let second = l.tile_rect(band.first + 1, 0.0);
            assert!(second.x > first.x, "{spec}: second tile is not to the right");
            assert_eq!(second.y, first.y, "{spec}: second tile dropped a row");
        }
    }

    #[test]
    fn bands_cover_the_panel_with_nothing_in_between() {
        // A click anywhere has to mean some box, whatever shape the tree is.
        let sections = [
            at("Side", 6, "left"),
            at("Top", 4, "right/top"),
            at("Bottom", 4, "right/bottom"),
        ];
        let l = Layout::compute(&sections, metrics(), WORK);
        for across in 0..50 {
            for down in 0..50 {
                let x = across as f32 * (l.panel.w / 50.0);
                let y = l.metrics.search_h
                    + down as f32 * ((l.content_h - l.metrics.search_h) / 50.0);
                assert!(
                    l.bands().iter().any(|band| band.rect.contains(x, y)),
                    "nothing covers {x},{y}"
                );
            }
        }
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
    fn every_tile_on_the_panel_sits_on_one_lattice() {
        // Regardless of which box it is in, what that box is titled, or what a
        // box above it did. A row ten pixels out does not read as two boxes
        // being apart - it reads as the grid being crooked, and a 3x3 centre
        // block makes it obvious because more boxes end up stacked.
        let m = Metrics { section_gap: 20.0, ..metrics() };
        let l = Layout::compute(
            &[
                at("Apps", 14, "left@50"),
                at("Bookmarks", 12, "right/top"),
                at("Browsing", 3, "right/bottom"),
                shape("Active", 2),
                middle(0, 9, 3),
                middle(1, 9, 3),
            ],
            m,
            WORK,
        );
        let origin = m.search_h + m.padding;
        let pitch = m.tile_h + m.gap;
        for n in 0..l.tile_count() {
            let tile = l.tile_rect(n, 0.0);
            let row = (tile.y - origin) / pitch;
            assert!(
                (row - row.round()).abs() < 0.01,
                "tile {n} at y {} is {} of a row off the lattice",
                tile.y,
                row - row.round()
            );
            let col = (tile.x - m.padding) / (m.tile_w + m.gap);
            assert!(
                (col - col.round()).abs() < 0.01,
                "tile {n} at x {} is {} of a column off the lattice",
                tile.x,
                col - col.round()
            );
        }
    }

    #[test]
    fn a_stacked_box_starts_on_the_row_after_the_one_above_it() {
        // Flush, not flush plus a few pixels. `section_gap` buys whole rows or
        // it buys nothing.
        let m = Metrics { section_gap: 0.0, ..metrics() };
        let l = Layout::compute(&[shape("Apps", 3), shape("Active", 2)], m, WORK);
        let above = l.tile_rect(0, 0.0);
        let below = l.tile_rect(3, 0.0);
        assert_eq!(below.y, above.y + m.tile_h + m.gap);

        // And a whole row of clearance when one is actually asked for.
        let spaced = Metrics { section_gap: m.tile_h + m.gap, ..m };
        let l = Layout::compute(&[shape("Apps", 3), shape("Active", 2)], spaced, WORK);
        assert_eq!(
            l.tile_rect(3, 0.0).y,
            l.tile_rect(0, 0.0).y + 2.0 * (m.tile_h + m.gap)
        );
    }

    #[test]
    fn a_title_costs_no_layout_at_all() {
        // The rule this replaced: a title used to take a row above its tiles,
        // and a section costing a header plus a row is what stopped the panel
        // being split into the boxes it wants to be. It rides the ring now.
        let m = Metrics { header_gap: 6.0, ..metrics() };
        let titled = Layout::compute(&[shape("Pinned", 4)], m, WORK);
        let bare = Layout::compute(&[shape("", 4)], m, WORK);

        assert_eq!(titled.content_h, bare.content_h);
        assert_eq!(titled.tile_rect(0, 0.0), bare.tile_rect(0, 0.0));
    }

    #[test]
    fn the_title_sits_on_the_ring_above_its_own_tiles() {
        let m = metrics();
        let l = Layout::compute(&[shape("Pinned", 4)], m, WORK);
        let header = l.headers(0.0).next().unwrap().1;
        let tile = l.tile_rect(0, 0.0);

        // Centred on the line the ring is drawn along, which runs a quarter of
        // a gap above the tiles. Anywhere else and it reads as a label that
        // missed.
        assert_eq!(header.y + m.header_h / 2.0, tile.y - m.gap / 4.0);
        // Inset from the corner, so it never lands on the arc.
        assert_eq!(header.x, tile.x + m.header_gap);
    }

    // --- the ring round a box ---

    /// A ring, as whole cells of the box's own grid: which corners it turns,
    /// counted off the tile pitch so the numbers read like the picture.
    fn ring_cells(l: &Layout, band: usize, m: &Metrics) -> Vec<Vec<(i32, i32)>> {
        let cells = &l.bands()[band].cells;
        l.band_ring(band, 0.0)
            .into_iter()
            .map(|ring| {
                ring.into_iter()
                    .map(|(x, y)| {
                        (
                            (((x + m.gap / 2.0) - cells.x) / (m.tile_w + m.gap)).round() as i32,
                            (((y + m.gap / 2.0) - cells.y) / (m.tile_h + m.gap)).round() as i32,
                        )
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_box_with_nothing_in_its_way_gets_four_corners() {
        let m = metrics();
        let l = Layout::compute(&[shape("Pinned", 8)], m, WORK);
        let rings = ring_cells(&l, 0, &m);
        assert_eq!(rings.len(), 1, "one box, one ring");
        assert_eq!(rings[0].len(), 4, "a rectangle has four corners: {:?}", rings[0]);
    }

    #[test]
    fn a_ragged_last_row_is_squared_off() {
        // The shape follows the centre block, which never moves, and not the
        // item count, which changes every time a window opens. A ring that
        // stepped in around a half-empty last row would never sit still.
        let m = metrics();
        let full = Layout::compute(&[shape("Pinned", 8)], m, WORK);
        let ragged = Layout::compute(&[shape("Pinned", 8), shape("", 0)], m, WORK);
        assert_eq!(ring_cells(&full, 0, &m), ring_cells(&ragged, 0, &m));

        // Same box, one tile short of filling its last row.
        let short = Layout::compute(&[shape("Pinned", 7)], m, WORK);
        let rings = ring_cells(&short, 0, &m);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 4, "the tail of the list is not a corner: {:?}", rings[0]);
    }

    #[test]
    fn a_box_wrapping_the_centre_block_comes_out_a_c() {
        // The default shape: the panel split down the middle, the block
        // straddling the seam, so it takes a bite out of the side of each half
        // rather than sitting inside one of them.
        let m = metrics();
        let l = Layout::compute(
            &[
                at("Apps", 24, "left@50"),
                at("Browsing", 24, "right@50"),
                middle(0, 4, 2),
                middle(1, 4, 2),
            ],
            m,
            WORK,
        );
        for band in 0..2 {
            let rings = ring_cells(&l, band, &m);
            assert_eq!(rings.len(), 1, "the block is against an edge, so no hole");
            // Four corners is a rectangle. Eight is a rectangle with a bite in
            // it, which is the C.
            assert_eq!(
                rings[0].len(),
                8,
                "band {band} wrapped round the block is not a C: {:?}",
                rings[0]
            );
        }
    }

    /// A box's cells, spelled as a picture: `#` filled, `.` taken by a hole.
    fn cells_of(rows: &[&str]) -> Cells {
        let cols = rows.first().map_or(0, |row| row.len());
        Cells {
            x: 0.0,
            y: 0.0,
            cols,
            rows: rows.len(),
            filled: rows.iter().flat_map(|row| row.chars().map(|c| c == '#')).collect(),
        }
    }

    #[test]
    fn a_block_inside_a_box_leaves_a_hole_in_the_ring() {
        // The block standing in the middle of a box rather than against an edge
        // of it. Two rings, and the inner one is what says the middle of the box
        // is not part of it.
        //
        // Straight off the cells rather than off a layout: which shapes come
        // out of a real panel depends on how many tiles there happen to be, and
        // this is a rule about the shape, not about the panel.
        let m = metrics();
        let rings = ring_of(&cells_of(&["#####", "#...#", "#...#", "#####"]), &m);
        assert_eq!(rings.len(), 2, "an outer ring and a hole: {rings:?}");
        for ring in &rings {
            assert_eq!(ring.len(), 4, "both are rectangles: {ring:?}");
        }

        // Wound opposite ways, which is what tells a fill rule they are not two
        // separate shapes. Shoelace: the outer one turns one way, the hole the
        // other.
        let area = |ring: &Vec<(f32, f32)>| {
            let n = ring.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (ring[i], ring[(i + 1) % n]);
                    a.0 * b.1 - b.0 * a.1
                })
                .sum::<f32>()
        };
        assert!(
            area(&rings[0]) * area(&rings[1]) < 0.0,
            "the hole is wound the same way as the outer ring"
        );
    }

    #[test]
    fn a_notch_from_two_holes_at_once_is_still_one_ring() {
        // The centre block and the corner the app's own button holds are both
        // taken out of the same box. Two bites, one shape.
        let m = metrics();
        let rings = ring_of(&cells_of(&["#####", "..###", "..###", "####."]), &m);
        assert_eq!(rings.len(), 1, "one shape, not one per bite: {rings:?}");
        assert_eq!(rings[0].len(), 10, "{:?}", rings[0]);

        // A bite that does not reach a side is a hole, and a hole is its own
        // ring - the block standing in the middle of a box.
        let inner = ring_of(&cells_of(&["#####", "#..##", "#..##", "####."]), &m);
        assert_eq!(inner.len(), 2, "the interior bite should be its own ring");
    }

    #[test]
    fn two_boxes_side_by_side_leave_their_rings_apart() {
        // Half the gutter each. Boxes tile the panel, so rings drawn the full
        // way out would land in the same pixel - which was fine while every box
        // wore one faint colour, and reads as a fringe now that they do not.
        let m = metrics();
        let l = Layout::compute(&[at("Apps", 8, "left@50"), at("Web", 8, "right@50")], m, WORK);
        let right_of = |band| {
            l.band_ring(band, 0.0)[0].iter().map(|p| p.0).fold(f32::MIN, f32::max)
        };
        let left_of = |band| {
            l.band_ring(band, 0.0)[0].iter().map(|p| p.0).fold(f32::MAX, f32::min)
        };
        assert!(
            left_of(1) - right_of(0) >= m.gap / 2.0,
            "the two rings meet: {} then {}",
            right_of(0),
            left_of(1)
        );

        // And still outside their own tiles, or the ring crops what it is
        // supposed to be round.
        let tile = l.tile_rect(0, 0.0);
        assert!(left_of(0) < tile.x);
        assert!(right_of(0) > tile.x + tile.w);
    }

    #[test]
    fn the_notch_opens_rather_than_closes_over_the_block() {
        // The inset moves every edge toward the inside of its own shape. On a
        // reflex corner - the inside of the C, where the block bit into the box
        // - that is *away* from the block. Get the sign wrong and the notch
        // closes over the block's tiles, and the ring is drawn across them.
        let m = metrics();
        let l = Layout::compute(
            &[
                at("Apps", 24, "left@50"),
                at("Browsing", 24, "right@50"),
                middle(0, 4, 2),
                middle(1, 4, 2),
            ],
            m,
            WORK,
        );
        let (block, _) = l.center_frame().expect("a block");
        for band in 0..l.bands().len() {
            for ring in l.band_ring(band, 0.0) {
                for (x, y) in ring {
                    assert!(
                        !block.contains(x, y),
                        "band {band}: the ring runs through the block at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_ring_never_crosses_a_tile() {
        // It sits in the gutter, so no corner of it can land on anything the
        // panel draws a tile in - its own or a neighbour's.
        let m = metrics();
        let l = Layout::compute(
            &[
                at("Apps", 24, "left@50"),
                at("Browsing", 17, "right@50"),
                middle(0, 4, 2),
                middle(1, 4, 2),
            ],
            m,
            WORK,
        );
        let tiles: Vec<Rect> = (0..l.tile_count()).map(|n| l.tile_rect(n, 0.0)).collect();
        for band in 0..l.bands().len() {
            for ring in l.band_ring(band, 0.0) {
                for (x, y) in ring {
                    assert!(
                        !tiles.iter().any(|tile| tile.contains(x, y)),
                        "band {band}: a ring corner lands on a tile at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_ring_closes() {
        // The walk follows edges until it comes back to where it started. A
        // ring that did not close would be drawn as an open path, which is a
        // stroke running off across the panel.
        let m = metrics();
        for count in 1..24 {
            let l = Layout::compute(
                &[shape("Apps", count), middle(0, 4, 2), middle(1, 4, 2)],
                m,
                WORK,
            );
            for (index, band) in l.bands().iter().enumerate() {
                if band.count == 0 {
                    continue;
                }
                for ring in l.band_ring(index, 0.0) {
                    assert!(ring.len() >= 4, "{count} items: a ring of {}", ring.len());
                    assert_eq!(ring.len() % 2, 0, "{count} items: odd corners on a rectilinear ring");
                }
            }
        }
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
        for ((_, a, _), (_, b, _)) in l.headers(0.0).zip(l.headers(scroll)) {
            assert!((a.y - b.y - scroll).abs() < 0.01);
        }
    }

    // --- the centre block ---

    /// Every tile belonging to a box that is not the centre.
    fn around(l: &Layout, middle: &[usize]) -> Vec<Rect> {
        let center: Vec<&Band> = l.bands().iter().filter(|b| b.center).collect();
        assert_eq!(center.len(), middle.len(), "wrong number of centre bands");
        let held: Vec<usize> = center
            .iter()
            .flat_map(|b| b.first..b.first + b.count)
            .collect();
        (0..l.tile_count())
            .filter(|index| !held.contains(index))
            .map(|index| l.tile_rect(index, 0.0))
            .collect()
    }

    /// The rectangle the centre block actually occupies.
    fn block(l: &Layout) -> Rect {
        covering(
            &l.bands()
                .iter()
                .filter(|b| b.center)
                .cloned()
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn a_three_by_three_a_side_block_still_leaves_a_panel_around_it() {
        // What a new install starts with: nine slots a half, six columns of
        // block. It is the widest thing the settings squares can ask for short
        // of four each way, so this is where "the block wins over max_columns"
        // has to stop being free.
        let sections = vec![shape("Apps", 24), middle(0, 9, 3), middle(1, 9, 3)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let b = block(&l);

        assert!(b.x >= 0.0 && b.x + b.w <= l.panel.w, "the block hangs off the panel");
        assert!((b.w - (6.0 * 200.0 + 5.0 * 10.0)).abs() < 0.5, "the block is {} wide", b.w);
        assert!((b.h - (3.0 * 140.0 + 2.0 * 10.0)).abs() < 0.5, "the block is {} tall", b.h);
        // And the grid still has somewhere to wrap to on both sides of it.
        let around = around(&l, &[1, 2]);
        assert!(around.iter().any(|r| r.x + r.w <= b.x), "nothing fits down the left");
        assert!(around.iter().any(|r| r.x >= b.x + b.w), "nothing fits down the right");
    }

    #[test]
    fn the_centre_holds_the_middle_of_the_screen() {
        // The whole reason it exists: this is where a gaze pointer is most
        // accurate, so it is the one box whose position is not up for grabs.
        //
        // Measured against the screen, not the panel. Wrapping is a step
        // function and the two chase each other forever, so the panel is what
        // moves - which is fine, because the screen is what the eyes are aimed
        // at.
        for count in [4usize, 17, 30, 45] {
            let sections = vec![shape("Apps", count), middle(0, 4, 2), middle(1, 4, 2)];
            let l = Layout::compute(&sections, metrics(), WORK);
            let b = block(&l);

            let across = l.panel.x + b.x + b.w / 2.0;
            let down = l.panel.y + b.y + b.h / 2.0;
            assert!(
                (across - (WORK.x + WORK.w / 2.0)).abs() < 1.0,
                "{count} tiles: the block's middle is at x {across}, the screen's at {}",
                WORK.x + WORK.w / 2.0
            );
            assert!(
                (down - (WORK.y + WORK.h / 2.0)).abs() < 1.0,
                "{count} tiles: the block's middle is at y {down}, the screen's at {}",
                WORK.y + WORK.h / 2.0
            );
        }
    }

    #[test]
    fn the_panel_is_still_centred_when_there_is_no_block() {
        let sections = vec![shape("Apps", 30)];
        let l = Layout::compute(&sections, metrics(), WORK);
        assert!(
            ((l.panel.y - WORK.y) - (WORK.h - l.panel.h - (l.panel.y - WORK.y))).abs() < 1.5
        );
    }

    #[test]
    fn a_block_on_a_panel_taller_than_the_screen_still_starts_on_the_screen() {
        // Sixty tiles overflows and scrolls. The panel cannot slide up to
        // centre the block without hanging off the top.
        let sections = vec![shape("Apps", 60), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        assert!(l.panel.y >= WORK.y - 0.5, "the panel starts at {}", l.panel.y);
        assert!(
            l.panel.y + l.panel.h <= WORK.y + WORK.h + 0.5,
            "the panel ends at {}",
            l.panel.y + l.panel.h
        );
    }

    #[test]
    fn nothing_else_is_laid_out_underneath_the_centre() {
        let sections = vec![shape("Apps", 40), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let b = block(&l);

        for tile in around(&l, &[0, 1]) {
            assert!(
                !tile.overlaps(&b),
                "a tile at {},{} sits under the centre block {b:?}",
                tile.x,
                tile.y
            );
        }
    }

    #[test]
    fn the_boxes_wrap_around_the_centre_rather_than_being_cut_by_it() {
        // The point of the whole arrangement. The bento is planned as if the
        // centre were not there, so a box that reaches across the panel comes
        // out either side of it - not split into two boxes, and not stopped.
        let sections = vec![shape("Apps", 40), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let b = block(&l);
        let tiles = around(&l, &[0, 1]);

        let spans = |edge: f32, side: fn(&Rect, f32) -> bool| {
            tiles.iter().filter(|t| side(t, edge)).count()
        };
        let left = spans(b.x, |t, edge| t.x + t.w <= edge + 0.01);
        let right = spans(b.x + b.w, |t, edge| t.x >= edge - 0.01);
        assert!(left > 0 && right > 0, "{left} tiles left of the centre, {right} right of it");

        // And one box, not two: every one of those tiles belongs to the same
        // band it would have without a centre at all.
        assert_eq!(l.bands().iter().filter(|b| !b.center).count(), 1);
    }

    #[test]
    fn a_bar_slides_past_the_centre_rather_than_breaking_around_it() {
        // Seven controls on one row. Wrapped, they came out as four down the
        // left and three up on the right - in reading order and in no order at
        // all to look at. A bar has one row either way, so it has nothing to
        // gain by going round and its shape to lose.
        let sections = vec![shape("Apps", 24), shape("Bar", 7), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let bar = l
            .bands()
            .iter()
            .find(|band| band.section == 1)
            .expect("the bar has a band");

        let tiles: Vec<Rect> = (bar.first..bar.first + bar.count)
            .map(|index| l.tile_rect(index, 0.0))
            .collect();
        let top = tiles[0].y;
        assert!(
            tiles.iter().all(|tile| (tile.y - top).abs() < 0.01),
            "the bar broke across rows: {tiles:?}"
        );
        // And it is still one unbroken run, left to right.
        let m = metrics();
        for pair in tiles.windows(2) {
            assert!(
                (pair[1].x - pair[0].x - (m.tile_w + m.gap)).abs() < 0.01,
                "a gap opened in the bar at {}", pair[1].x
            );
        }
        for tile in &tiles {
            assert!(!tile.overlaps(&block(&l)), "the bar sits under the centre");
        }
    }

    #[test]
    fn a_bar_wraps_into_the_cells_beside_the_block_rather_than_sliding_past() {
        // The block against one side of a box leaves a single run of free
        // cells, and a run is something a bar can wrap into. Sliding past it
        // instead put a three-tile box two rows below the space it fitted in,
        // and left that space holding nothing.
        // The working config's own numbers, because this is a rule about how
        // much room the block leaves beside it and that depends on the shape of
        // the real panel: ten columns, a three-a-side block, two columns clear
        // either side of it.
        let m = live();
        let l = Layout::compute(
            &[
                at("Browsing", 3, "right/bottom"),
                shape("Apps", 14),
                shape("Active", 2),
                at("Bookmarks", 12, "right/top"),
                at("", 4, "bottom"),
                middle(0, 9, 3),
                middle(1, 9, 3),
            ],
            m,
            SCREEN,
        );
        let bar = l.bands().iter().find(|band| band.section == 0).expect("the bar");
        let first = l.tile_rect(bar.first, 0.0);
        let last = l.tile_rect(bar.first + bar.count - 1, 0.0);
        let (block, _) = l.center_frame().expect("a block");

        // Beside the block, not below it.
        assert!(
            first.y < block.y + block.h,
            "the bar slid to {}, past the block ending at {}",
            first.y,
            block.y + block.h
        );
        // And clear of it, which is the whole point of not just ignoring the
        // hole.
        for n in bar.first..bar.first + bar.count {
            assert!(!l.tile_rect(n, 0.0).overlaps(&block), "tile {n} sits under the block");
        }
        assert!(last.y >= first.y, "the bar ran backwards");
    }

    #[test]
    fn a_title_lands_on_its_own_ring_and_not_on_the_block() {
        // The block can take the whole start of a box's first row. A title left
        // at the box's left edge then floats over the block, with none of the
        // tiles it names anywhere near it.
        let m = live();
        let l = Layout::compute(
            &[
                at("Browsing", 3, "right/bottom"),
                shape("Apps", 14),
                shape("Active", 2),
                at("Bookmarks", 12, "right/top"),
                at("", 4, "bottom"),
                middle(0, 9, 3),
                middle(1, 9, 3),
            ],
            m,
            SCREEN,
        );
        let (block, _) = l.center_frame().expect("a block");
        for (title, rect, band) in l.headers(0.0) {
            assert!(
                !block.contains(rect.x, rect.y + rect.h / 2.0),
                "\"{title}\" sits on the block at {},{}",
                rect.x,
                rect.y
            );
            // And over its own first tile's column, which is what makes it
            // read as belonging to that box.
            let first = l.tile_rect(l.bands()[band].first, 0.0);
            assert_eq!(rect.x, first.x + m.header_gap, "\"{title}\" is not over its tiles");
        }
    }

    #[test]
    fn a_drop_counts_tiles_and_not_the_cells_the_block_took() {
        // The box wraps round the block, so its cells and its tiles are not the
        // same list. Counting cells put every drop past the block that many
        // places too far along, and silently scrambled the order it wrote.
        let m = metrics();
        let l = Layout::compute(
            &[shape("Apps", 24), middle(0, 4, 2), middle(1, 4, 2)],
            m,
            WORK,
        );
        let band = l
            .bands()
            .iter()
            .position(|band| !band.center && band.count > 0)
            .expect("the apps box");

        // Dropped on a tile's own left edge, the slot is that tile's index -
        // for every tile, including the ones past the hole.
        for n in 0..l.bands()[band].count {
            let tile = l.tile_rect(n, 0.0);
            assert_eq!(
                l.insert_slot(band, tile.x, tile.y + tile.h / 2.0, 0.0),
                n,
                "tile {n} at {},{} resolved to the wrong slot",
                tile.x,
                tile.y
            );
        }
    }

    #[test]
    fn a_bar_that_already_clears_the_centre_is_left_where_it_was() {
        // Two tiles down the left, nowhere near the middle. Nothing to slide
        // past, so nothing moves. Untitled, so its band starts where its tiles
        // do and any push would show.
        // Listed first, so it takes the top of the content and a push would
        // show as a row of empty space above it.
        let short = vec![shape("", 2), shape("Apps", 24), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&short, metrics(), WORK);
        let bar = l.bands().iter().find(|band| band.section == 0).unwrap();
        let first = l.tile_rect(bar.first, 0.0);
        assert!(!first.overlaps(&block(&l)), "the test bar is not clear of the centre");
        assert!(
            (first.y - metrics().padding).abs() < 0.01,
            "the bar was pushed to {} instead of sitting at the top",
            first.y
        );
    }

    #[test]
    fn the_centre_lands_on_whole_cells_of_the_grid() {
        // Off the grid it costs every row it grazes: a row overlapping it by
        // ten pixels loses its middle columns exactly as a row sitting squarely
        // behind it does, and the panel fills with space holding nothing.
        for count in [12usize, 24, 40] {
            let sections = vec![shape("Apps", count), middle(0, 4, 2), middle(1, 4, 2)];
            let l = Layout::compute(&sections, metrics(), WORK);
            let m = metrics();
            let step = m.tile_h + m.gap;
            let b = block(&l);

            // A tile of the box the block sits in, to read the row grid off.
            let grid_top = l.tile_rect(0, 0.0).y;
            let offset = (b.y - grid_top) / step;
            assert!(
                (offset - offset.round()).abs() < 0.01,
                "{count} tiles: the block sits {offset} rows down, not a whole number"
            );
        }
    }

    #[test]
    fn a_row_that_only_grazes_the_centre_keeps_its_middle() {
        // The bug this fixes: every row either sits squarely behind the block
        // or is clear of it, so the only cells lost are the ones the block is
        // actually standing on.
        let sections = vec![shape("Apps", 40), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let b = block(&l);
        let m = metrics();

        // Count the tiles on each row of the surrounding box. Only the rows the
        // block occupies may be short, and they are short by exactly its width.
        let mut per_row: std::collections::BTreeMap<i64, usize> = Default::default();
        for tile in around(&l, &[0, 1]) {
            *per_row.entry((tile.y * 100.0) as i64).or_default() += 1;
        }
        let across = 4;
        for (key, held) in per_row {
            let y = key as f32 / 100.0;
            let row = Rect { x: 0.0, y, w: l.panel.w, h: m.tile_h };
            let behind = row.overlaps(&b);
            let last = held < l.cols && !behind;
            assert!(
                held == l.cols || held == l.cols - across || last,
                "row at {y} holds {held} of {} tiles, block behind it: {behind}",
                l.cols
            );
        }
    }

    #[test]
    fn reading_order_survives_the_hole() {
        // Left to right then down, skipping what the centre stands on. A tile
        // is never earlier in the list than the one above and left of it.
        let sections = vec![shape("Apps", 40), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let tiles = around(&l, &[0, 1]);

        for pair in tiles.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let later = b.y > a.y + 0.01 || ((b.y - a.y).abs() < 0.01 && b.x > a.x);
            assert!(later, "tile at {},{} does not read after {},{}", b.x, b.y, a.x, a.y);
        }
    }

    #[test]
    fn a_short_panel_still_grows_tall_enough_to_hold_the_centre() {
        // One tile and a centre block: the panel cannot be one tile tall.
        let sections = vec![shape("Apps", 1), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let b = block(&l);
        assert!(
            b.y + b.h <= l.content_h + 0.01,
            "the centre ends at {} and the content at {}",
            b.y + b.h,
            l.content_h
        );
        assert!(b.y >= 0.0, "the centre starts above the panel at {}", b.y);
    }

    #[test]
    fn the_centre_keeps_its_shape_whatever_is_around_it() {
        // A learnable position means the same rectangle every summon, however
        // many windows happen to be open.
        let m = metrics();
        let shapes = |count| vec![shape("Apps", count), middle(0, 4, 2), middle(1, 4, 2)];
        let sizes: Vec<(f32, f32)> = [4usize, 20, 45]
            .into_iter()
            .map(|count| {
                let b = block(&Layout::compute(&shapes(count), m, WORK));
                (b.w, b.h)
            })
            .collect();
        assert!(
            sizes.windows(2).all(|pair| pair[0] == pair[1]),
            "the centre changed size with the grid around it: {sizes:?}"
        );
    }

    #[test]
    fn a_half_that_is_short_leaves_its_slots_empty_rather_than_shortening_the_block() {
        // Two rows on the left, one on the right: the block is still two rows.
        let sections = vec![shape("Apps", 20), middle(0, 4, 2), middle(1, 2, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let m = metrics();
        let b = block(&l);
        assert!((b.h - (2.0 * m.tile_h + m.gap)).abs() < 0.5, "block is {} tall", b.h);
    }

    #[test]
    fn an_empty_centre_leaves_the_panel_exactly_as_it_was() {
        let plain = vec![shape("Apps", 24)];
        let with_centre = vec![shape("Apps", 24), middle(0, 0, 2), middle(1, 0, 2)];
        let a = Layout::compute(&plain, metrics(), WORK);
        let b = Layout::compute(&with_centre, metrics(), WORK);
        assert_eq!(a.panel, b.panel);
        assert_eq!(a.cols, b.cols);
        for index in 0..a.tile_count() {
            assert_eq!(a.tile_rect(index, 0.0), b.tile_rect(index, 0.0));
        }
    }

    #[test]
    fn the_centre_is_never_squeezed_by_a_narrow_panel() {
        // Four tiles across is what the block asked for, and it gets them even
        // though the grid beside it wants only two.
        let sections = vec![shape("Apps", 2), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let m = metrics();
        assert!(l.cols >= 4, "panel came out {} columns wide", l.cols);
        let b = block(&l);
        assert!((b.w - (4.0 * m.tile_w + 3.0 * m.gap)).abs() < 0.5, "block is {} wide", b.w);
    }

    #[test]
    fn a_centre_wider_than_the_panel_takes_the_panel_and_nothing_breaks() {
        // A hard column cap the block cannot fit inside. It takes what there
        // is rather than overflowing, and the grid wraps below it.
        let m = Metrics { max_cols: 3, ..metrics() };
        let sections = vec![shape("Apps", 12), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, m, WORK);
        let b = block(&l);
        assert!(b.x >= 0.0 && b.x + b.w <= l.panel.w + 0.01, "block {b:?} left the panel");
        for tile in around(&l, &[0, 1]) {
            assert!(!tile.overlaps(&b), "a tile at {},{} sits under the centre", tile.x, tile.y);
        }
    }

    #[test]
    fn every_tile_of_the_centre_lands_inside_the_block() {
        let sections = vec![shape("Apps", 20), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let b = block(&l);
        for band in l.bands().iter().filter(|band| band.center) {
            for index in band.first..band.first + band.count {
                let tile = l.tile_rect(index, 0.0);
                assert!(
                    tile.x >= b.x - 0.01
                        && tile.x + tile.w <= b.x + b.w + 0.01
                        && tile.y >= b.y - 0.01
                        && tile.y + tile.h <= b.y + b.h + 0.01,
                    "centre tile {index} at {},{} is outside the block {b:?}",
                    tile.x,
                    tile.y
                );
            }
        }
    }

    #[test]
    fn the_two_halves_sit_side_by_side_in_order() {
        let sections = vec![shape("Apps", 20), middle(0, 4, 2), middle(1, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let halves: Vec<&Band> = l.bands().iter().filter(|b| b.center).collect();
        assert_eq!(halves.len(), 2);
        assert!(
            halves[0].rect.x + halves[0].rect.w <= halves[1].rect.x + 0.01,
            "the halves overlap: {:?} and {:?}",
            halves[0].rect,
            halves[1].rect
        );
        assert!((halves[0].rect.y - halves[1].rect.y).abs() < 0.01, "the halves are not level");
    }

    #[test]
    fn the_centre_takes_no_header_however_it_is_titled() {
        // A header would spend a row of the most valuable space on the panel
        // saying what the icons already say.
        let mut titled = middle(0, 4, 2);
        titled.title = "Favorites".into();
        let sections = vec![shape("Apps", 12), titled];
        let l = Layout::compute(&sections, metrics(), WORK);
        assert_eq!(l.headers(0.0).count(), 1, "the centre drew a header");
    }

    #[test]
    fn the_centre_never_takes_a_place_in_the_tree() {
        // Placed by hand after the tree, so its bands come last and `stretch`
        // - which walks the tree by leaf order - can never reach them.
        let sections = vec![at("Left", 8, "left@30"), shape("Rest", 20), middle(0, 4, 2)];
        let l = Layout::compute(&sections, metrics(), WORK);
        let bands = l.bands();
        assert!(bands.last().is_some_and(|band| band.center));
        assert!(bands[..bands.len() - 1].iter().all(|band| !band.center));
    }
}
