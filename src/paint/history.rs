//! Undo/redo: one `HistoryEntry` per user gesture. Most entries are a
//! `Delta` (a whole stroke, a fill, a shape commit — not one per cell),
//! storing each touched cell's value from just before the gesture. A few
//! operations (flip/rotate/clear/resize/stretch/skew) touch every cell *and*
//! can change the canvas's dimensions, so they're captured as a full
//! `CanvasSnapshot` instead — cheap at this canvas's typical size, and
//! simpler than teaching `Delta` to also carry a width/height change.
//!
//! Undo and redo share one swap routine: pop an entry, capture the canvas's
//! *current* state into a fresh entry of the same shape for the opposite
//! stack, then apply the popped entry's stored state.

use std::collections::VecDeque;

use super::ansi::PaintCellSer;

use super::canvas::Canvas;

const MAX_HISTORY: usize = 100;

pub struct Delta(Vec<((u16, u16), Option<PaintCellSer>)>);

impl Delta {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Records `(x, y)`'s value just before it's about to change — but only
    /// the first time within this delta, so a cell touched repeatedly in
    /// one gesture (e.g. a stroke crossing itself) keeps its true original
    /// value, not an intermediate one.
    pub fn record(&mut self, canvas: &Canvas, x: u16, y: u16) {
        if self.0.iter().any(|((px, py), _)| *px == x && *py == y) {
            return;
        }
        self.0.push(((x, y), canvas.get(x, y)));
    }

    /// Restores every touched cell to its pre-gesture value without
    /// recording anything — for discarding an in-progress gesture the user
    /// cancelled before it was ever pushed to history (so there's nothing
    /// to `undo` later; this just undoes it immediately and forgets it).
    pub fn revert(self, canvas: &mut Canvas) {
        for ((x, y), value) in self.0 {
            canvas.set(x, y, value);
        }
    }
}

/// A full canvas capture (dimensions + every painted cell) — the undo unit
/// for whole-canvas transforms that a cell-level `Delta` can't represent
/// (anything that changes `width`/`height`).
pub struct CanvasSnapshot {
    width: u16,
    height: u16,
    cells: std::collections::HashMap<(u16, u16), PaintCellSer>,
}

impl CanvasSnapshot {
    pub fn capture(canvas: &Canvas) -> Self {
        Self { width: canvas.width, height: canvas.height, cells: canvas.cells.clone() }
    }

    fn restore(self, canvas: &mut Canvas) {
        canvas.width = self.width;
        canvas.height = self.height;
        canvas.cells = self.cells;
    }
}

enum HistoryEntry {
    Cells(Delta),
    Canvas(CanvasSnapshot),
}

pub struct History {
    undo: VecDeque<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl History {
    pub fn new() -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new() }
    }

    /// Pushes a completed gesture's delta (a no-op delta is dropped), and
    /// clears the redo stack — a fresh action invalidates any redo history.
    pub fn push(&mut self, delta: Delta) {
        if delta.is_empty() {
            return;
        }
        self.push_entry(HistoryEntry::Cells(delta));
    }

    /// Pushes a whole-canvas snapshot taken *before* a flip/rotate/clear/
    /// resize/stretch/skew, so that op becomes undoable like any other
    /// gesture. Same redo-clearing behavior as `push`.
    pub fn push_canvas(&mut self, snapshot: CanvasSnapshot) {
        self.push_entry(HistoryEntry::Canvas(snapshot));
    }

    fn push_entry(&mut self, entry: HistoryEntry) {
        self.undo.push_back(entry);
        if self.undo.len() > MAX_HISTORY {
            self.undo.pop_front();
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, canvas: &mut Canvas) -> bool {
        Self::swap(&mut self.undo, &mut self.redo, canvas)
    }

    pub fn redo(&mut self, canvas: &mut Canvas) -> bool {
        // `redo` is a `Vec` used as a stack (push/pop from the end), same
        // discipline as `VecDeque::push_back`/`pop_back` on `undo`.
        let Some(entry) = self.redo.pop() else { return false };
        let inverse = Self::apply(entry, canvas);
        self.undo.push_back(inverse);
        true
    }

    fn swap(from: &mut VecDeque<HistoryEntry>, to: &mut Vec<HistoryEntry>, canvas: &mut Canvas) -> bool {
        let Some(entry) = from.pop_back() else { return false };
        let inverse = Self::apply(entry, canvas);
        to.push(inverse);
        true
    }

    /// Applies `entry` to `canvas`, returning the inverse entry (the
    /// canvas's state, in the same shape, from just before this application).
    fn apply(entry: HistoryEntry, canvas: &mut Canvas) -> HistoryEntry {
        match entry {
            HistoryEntry::Cells(delta) => {
                let mut inverse = Delta::new();
                for ((x, y), value) in delta.0 {
                    inverse.0.push(((x, y), canvas.get(x, y)));
                    canvas.set(x, y, value);
                }
                HistoryEntry::Cells(inverse)
            }
            HistoryEntry::Canvas(snapshot) => {
                let inverse = CanvasSnapshot::capture(canvas);
                snapshot.restore(canvas);
                HistoryEntry::Canvas(inverse)
            }
        }
    }
}
