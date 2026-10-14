//! The painting surface: a fixed-resolution `Canvas` independent of any
//! window's size, viewed through a scrollable, zoomable `Viewport`. This is
//! the foundation the rest of Paint's tools build on — window resizing
//! never touches canvas content, it only changes how much of it is visible
//! at once.

use std::collections::HashMap;

use super::ansi::PaintCellSer;

pub struct Canvas {
    pub width: u16,
    pub height: u16,
    pub cells: HashMap<(u16, u16), PaintCellSer>,
}

impl Canvas {
    pub fn new(width: u16, height: u16) -> Self {
        Self { width, height, cells: HashMap::new() }
    }

    pub fn get(&self, x: u16, y: u16) -> Option<PaintCellSer> {
        self.cells.get(&(x, y)).cloned()
    }

    /// Sets (or, if `cell` is `None`, clears) a cell, returning its prior
    /// value — the primitive undo/redo deltas are built from.
    pub fn set(&mut self, x: u16, y: u16, cell: Option<PaintCellSer>) -> Option<PaintCellSer> {
        match cell {
            Some(c) => self.cells.insert((x, y), c),
            None => self.cells.remove(&(x, y)),
        }
    }

    pub fn in_bounds(&self, x: u16, y: u16) -> bool {
        x < self.width && y < self.height
    }

    /// The smallest rect containing every painted cell, or `None` if the
    /// canvas is entirely blank.
    pub fn bounds(&self) -> Option<(u16, u16, u16, u16)> {
        if self.cells.is_empty() {
            return None;
        }
        let min_x = self.cells.keys().map(|(x, _)| *x).min().unwrap();
        let max_x = self.cells.keys().map(|(x, _)| *x).max().unwrap();
        let min_y = self.cells.keys().map(|(_, y)| *y).min().unwrap();
        let max_y = self.cells.keys().map(|(_, y)| *y).max().unwrap();
        Some((min_x, min_y, max_x, max_y))
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }
}

/// The zoom levels a `Viewport` can be at, cycled via the Magnifier tool.
/// Positive values are the familiar "N terminal cells per canvas cell"
/// zoomed-in levels; negative values invert that ratio ("N canvas cells per
/// terminal cell") to zoom out past 1:1 and fit more of a canvas larger
/// than the window into view at once.
pub const ZOOM_LEVELS: [i16; 7] = [-8, -4, -2, 1, 2, 4, 8];

/// A scrollable, zoomable window onto a `Canvas`. Local (viewport) and
/// canvas coordinates are always 0-based; `scroll_x/scroll_y` is the canvas
/// coordinate shown at local `(0, 0)`.
#[derive(Clone, Copy)]
pub struct Viewport {
    pub scroll_x: u16,
    pub scroll_y: u16,
    /// See `ZOOM_LEVELS`. Never 0.
    pub zoom: i16,
}

impl Viewport {
    pub fn new() -> Self {
        Self { scroll_x: 0, scroll_y: 0, zoom: 1 }
    }

    /// Computes how many canvas cells are visible in a `area_w × area_h`
    /// (terminal-cell) viewport area at the current zoom, clamping scroll
    /// so the view never runs past the canvas edges. Call once per render
    /// before drawing.
    pub fn visible(&mut self, area_w: u16, area_h: u16, canvas: &Canvas) -> (u16, u16) {
        let (vis_w, vis_h) = if self.zoom > 0 {
            let zoom = self.zoom as u16;
            ((area_w / zoom).max(1), (area_h / zoom).max(1))
        } else {
            let out = (-self.zoom) as u16;
            (area_w.saturating_mul(out).max(1), area_h.saturating_mul(out).max(1))
        };
        let vis_w = vis_w.min(canvas.width.max(1));
        let vis_h = vis_h.min(canvas.height.max(1));
        let max_scroll_x = canvas.width.saturating_sub(vis_w);
        let max_scroll_y = canvas.height.saturating_sub(vis_h);
        self.scroll_x = self.scroll_x.min(max_scroll_x);
        self.scroll_y = self.scroll_y.min(max_scroll_y);
        (vis_w, vis_h)
    }

    /// Maps a local (0-based) viewport coordinate to a canvas coordinate.
    pub fn to_canvas(self, x: u16, y: u16) -> (u16, u16) {
        if self.zoom > 0 {
            let zoom = self.zoom as u16;
            (self.scroll_x + x / zoom, self.scroll_y + y / zoom)
        } else {
            let out = (-self.zoom) as u16;
            (self.scroll_x + x * out, self.scroll_y + y * out)
        }
    }

    /// Scrolls so `(cx, cy)` falls within the last-computed visible region,
    /// nudging the minimum amount needed rather than re-centering.
    pub fn ensure_visible(&mut self, cx: u16, cy: u16, visible: (u16, u16)) {
        let (vis_w, vis_h) = visible;
        if cx < self.scroll_x {
            self.scroll_x = cx;
        } else if cx >= self.scroll_x + vis_w {
            self.scroll_x = cx + 1 - vis_w;
        }
        if cy < self.scroll_y {
            self.scroll_y = cy;
        } else if cy >= self.scroll_y + vis_h {
            self.scroll_y = cy + 1 - vis_h;
        }
    }

    pub fn zoom_in(&mut self) {
        if let Some(&next) = ZOOM_LEVELS.iter().find(|&&z| z > self.zoom) {
            self.zoom = next;
        }
    }

    pub fn zoom_out(&mut self) {
        if let Some(&prev) = ZOOM_LEVELS.iter().rev().find(|&&z| z < self.zoom) {
            self.zoom = prev;
        }
    }

    /// The current zoom expressed as a display percentage (100 = 1:1).
    pub fn zoom_percent(self) -> u32 {
        if self.zoom > 0 { self.zoom as u32 * 100 } else { 100 / (-self.zoom) as u32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_in_climbs_through_out_levels_then_in_levels() {
        let mut vp = Viewport::new();
        vp.zoom = -8;
        vp.zoom_in();
        assert_eq!(vp.zoom, -4);
        vp.zoom_in();
        assert_eq!(vp.zoom, -2);
        vp.zoom_in();
        assert_eq!(vp.zoom, 1);
        vp.zoom_in();
        assert_eq!(vp.zoom, 2);
        vp.zoom_in();
        assert_eq!(vp.zoom, 4);
        vp.zoom_in();
        assert_eq!(vp.zoom, 8);
        vp.zoom_in(); // already maxed out
        assert_eq!(vp.zoom, 8);
    }

    #[test]
    fn zoom_out_descends_from_in_levels_past_1_into_out_levels() {
        let mut vp = Viewport::new();
        assert_eq!(vp.zoom, 1);
        vp.zoom_out();
        assert_eq!(vp.zoom, -2);
        vp.zoom_out();
        assert_eq!(vp.zoom, -4);
        vp.zoom_out();
        assert_eq!(vp.zoom, -8);
        vp.zoom_out(); // already maxed out
        assert_eq!(vp.zoom, -8);
    }

    #[test]
    fn zoomed_out_visible_covers_more_canvas_than_the_terminal_area() {
        let canvas = Canvas::new(100, 100);
        let mut vp = Viewport::new();
        vp.zoom = -4;
        let (vis_w, vis_h) = vp.visible(10, 10, &canvas);
        assert_eq!((vis_w, vis_h), (40, 40));
    }

    #[test]
    fn zoomed_out_visible_clamps_to_canvas_size() {
        let canvas = Canvas::new(20, 20);
        let mut vp = Viewport::new();
        vp.zoom = -4;
        let (vis_w, vis_h) = vp.visible(10, 10, &canvas);
        assert_eq!((vis_w, vis_h), (20, 20));
    }

    #[test]
    fn to_canvas_scales_by_the_out_factor_when_zoomed_out() {
        let mut vp = Viewport::new();
        vp.zoom = -2;
        vp.scroll_x = 5;
        vp.scroll_y = 7;
        assert_eq!(vp.to_canvas(3, 4), (11, 15));
    }

    #[test]
    fn zoom_percent_reports_the_familiar_100_scale() {
        let mut vp = Viewport::new();
        vp.zoom = 4;
        assert_eq!(vp.zoom_percent(), 400);
        vp.zoom = -4;
        assert_eq!(vp.zoom_percent(), 25);
        vp.zoom = 1;
        assert_eq!(vp.zoom_percent(), 100);
    }
}
