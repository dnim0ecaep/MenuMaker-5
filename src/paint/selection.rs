//! Selection and clipboard: a `Selection` is either a plain rectangle or a
//! free-form region traced with a lasso and rasterized into a cell mask.
//! `Clipboard` holds a copied/cut region at offsets relative to its
//! top-left corner, so it can be pasted back at any canvas position.

use std::collections::HashMap;
use std::collections::HashSet;

use super::ansi::PaintCellSer;

#[derive(Debug, Clone)]
pub struct Selection {
    pub x0: u16,
    pub y0: u16,
    pub x1: u16,
    pub y1: u16,
    /// `None` for a plain rectangular selection; `Some` for a free-form
    /// one, holding exactly the selected cells (absolute canvas
    /// coordinates) within the bounding box.
    pub mask: Option<HashSet<(u16, u16)>>,
}

impl Selection {
    pub fn rect(a: (u16, u16), b: (u16, u16)) -> Self {
        Self { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1), mask: None }
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        if x < self.x0 || x > self.x1 || y < self.y0 || y > self.y1 {
            return false;
        }
        match &self.mask {
            Some(mask) => mask.contains(&(x, y)),
            None => true,
        }
    }

    /// Translates the selection by `(dx, dy)` canvas cells — used after a
    /// move-floating gesture commits at its new position.
    pub fn translate(&self, dx: i32, dy: i32) -> Self {
        let shift = |v: u16, d: i32| (v as i32 + d).max(0) as u16;
        Self {
            x0: shift(self.x0, dx),
            y0: shift(self.y0, dy),
            x1: shift(self.x1, dx),
            y1: shift(self.y1, dy),
            mask: self.mask.as_ref().map(|m| m.iter().map(|&(x, y)| (shift(x, dx), shift(y, dy))).collect()),
        }
    }
}

/// Builds a free-form selection mask from a closed lasso boundary: flood
/// -fills from the boundary's bounding-box edges treating boundary cells as
/// walls, then everything the flood-fill couldn't reach is "inside". This
/// is simpler and more robust for a hand-drawn, possibly concave or
/// self-crossing loop than classic polygon scanline fill, at terminal-cell
/// resolution where the visual difference wouldn't be noticeable anyway.
pub fn lasso_mask(boundary: &[(u16, u16)]) -> HashSet<(u16, u16)> {
    if boundary.is_empty() {
        return HashSet::new();
    }
    let min_x = boundary.iter().map(|(x, _)| *x).min().unwrap();
    let max_x = boundary.iter().map(|(x, _)| *x).max().unwrap();
    let min_y = boundary.iter().map(|(_, y)| *y).min().unwrap();
    let max_y = boundary.iter().map(|(_, y)| *y).max().unwrap();
    let walls: HashSet<(u16, u16)> = boundary.iter().copied().collect();

    let mut outside = HashSet::new();
    let mut stack = Vec::new();
    for x in min_x..=max_x {
        stack.push((x, min_y));
        stack.push((x, max_y));
    }
    for y in min_y..=max_y {
        stack.push((min_x, y));
        stack.push((max_x, y));
    }
    while let Some((x, y)) = stack.pop() {
        if walls.contains(&(x, y)) || !outside.insert((x, y)) {
            continue;
        }
        for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
            if (min_x..=max_x).contains(&nx) && (min_y..=max_y).contains(&ny) {
                stack.push((nx, ny));
            }
        }
    }

    let mut mask = HashSet::new();
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if !outside.contains(&(x, y)) {
                mask.insert((x, y));
            }
        }
    }
    mask
}

#[derive(Debug, Clone)]
pub struct Clipboard {
    pub width: u16,
    pub height: u16,
    /// Cells at offsets relative to the copied region's top-left corner.
    pub cells: HashMap<(u16, u16), PaintCellSer>,
}
