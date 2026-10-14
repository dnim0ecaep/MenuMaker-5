//! Tool definitions and the drawing algorithms complex enough to deserve
//! their own function. Shape rasterizers (line/rect/ellipse previews) land
//! here in later milestones alongside the shape tools that use them.

use std::collections::HashSet;

use super::ansi::PaintCellSer;

use super::canvas::Canvas;
use super::history::Delta;
use super::strings::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pencil,
    Brush,
    Eraser,
    ColorEraser,
    Airbrush,
    Fill,
    Picker,
    Line,
    Rect,
    Ellipse,
    RoundRect,
    Curve,
    Polygon,
    SelectRect,
    SelectFree,
    Text,
    Zoom,
}

/// How a tool turns pointer/keyboard input into canvas edits — determines
/// which branch of `PaintState`'s gesture handling a tool goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interaction {
    /// Paints continuously along the drag path (Bresenham-interpolated).
    Freehand,
    /// Fires once per click/keypress; drag events are ignored.
    OneShot,
    /// Down records a start point; each Drag recomputes a live preview from
    /// start to the current point; Up rasterizes it for real.
    Shape,
    /// Two chained Shape-like gestures: the first drags out a straight
    /// chord, the second bends its midpoint into a control point.
    Curve,
    /// Each click/keypress appends a vertex; Enter closes and rasterizes
    /// the polygon, Esc discards it.
    Polygon,
    /// Down outside the current selection starts a new marquee/lasso; Down
    /// inside it lifts the selected cells into a draggable floating patch.
    Select,
    /// Down starts composing at that cell; typed characters paint and
    /// advance the cursor immediately; Enter or clicking away commits;
    /// Esc discards without touching history.
    Text,
    /// Left-click zooms in, right-click zooms out, both centered on the
    /// clicked point.
    Zoom,
}

impl Tool {
    pub const ALL: [Tool; 17] = [
        Tool::Pencil,
        Tool::Brush,
        Tool::Eraser,
        Tool::ColorEraser,
        Tool::Airbrush,
        Tool::Fill,
        Tool::Picker,
        Tool::Line,
        Tool::Rect,
        Tool::RoundRect,
        Tool::Ellipse,
        Tool::Curve,
        Tool::Polygon,
        Tool::SelectRect,
        Tool::SelectFree,
        Tool::Text,
        Tool::Zoom,
    ];

    pub fn label(self) -> &'static str {
        t(match self {
            Tool::Pencil => "tool.pencil.label",
            Tool::Brush => "tool.brush.label",
            Tool::Eraser => "tool.eraser.label",
            Tool::ColorEraser => "tool.color_eraser.label",
            Tool::Airbrush => "tool.airbrush.label",
            Tool::Fill => "tool.fill.label",
            Tool::Picker => "tool.picker.label",
            Tool::Line => "tool.line.label",
            Tool::Rect => "tool.rect.label",
            Tool::RoundRect => "tool.round_rect.label",
            Tool::Ellipse => "tool.ellipse.label",
            Tool::Curve => "tool.curve.label",
            Tool::Polygon => "tool.polygon.label",
            Tool::SelectRect => "tool.select_rect.label",
            Tool::SelectFree => "tool.select_free.label",
            Tool::Text => "tool.text.label",
            Tool::Zoom => "tool.zoom.label",
        })
    }

    /// Compact label for the clickable tool strip (row 0) — `label()` is
    /// used for status messages, where there's room to spell things out.
    pub fn strip_label(self) -> &'static str {
        t(match self {
            Tool::Pencil => "tool.pencil.strip",
            Tool::Brush => "tool.brush.strip",
            Tool::Eraser => "tool.eraser.strip",
            Tool::ColorEraser => "tool.color_eraser.strip",
            Tool::Airbrush => "tool.airbrush.strip",
            Tool::Fill => "tool.fill.strip",
            Tool::Picker => "tool.picker.strip",
            Tool::Line => "tool.line.strip",
            Tool::Rect => "tool.rect.strip",
            Tool::RoundRect => "tool.round_rect.strip",
            Tool::Ellipse => "tool.ellipse.strip",
            Tool::Curve => "tool.curve.strip",
            Tool::Polygon => "tool.polygon.strip",
            Tool::SelectRect => "tool.select_rect.strip",
            Tool::SelectFree => "tool.select_free.strip",
            Tool::Text => "tool.text.strip",
            Tool::Zoom => "tool.zoom.strip",
        })
    }

    /// Nerd Font glyph shown next to the number/label in the tool strip —
    /// see `icons::ICON_TOOL_*` for provenance notes.
    pub fn icon(self) -> &'static str {
        match self {
            Tool::Pencil => super::icons::ICON_TOOL_PENCIL,
            Tool::Brush => super::icons::ICON_TOOL_BRUSH,
            Tool::Eraser => super::icons::ICON_TOOL_ERASER,
            Tool::ColorEraser => super::icons::ICON_TOOL_COLOR_ERASER,
            Tool::Airbrush => super::icons::ICON_TOOL_AIRBRUSH,
            Tool::Fill => super::icons::ICON_TOOL_FILL,
            Tool::Picker => super::icons::ICON_TOOL_PICKER,
            Tool::Line => super::icons::ICON_TOOL_LINE,
            Tool::Rect => super::icons::ICON_TOOL_RECT,
            Tool::RoundRect => super::icons::ICON_TOOL_ROUND_RECT,
            Tool::Ellipse => super::icons::ICON_TOOL_ELLIPSE,
            Tool::Curve => super::icons::ICON_TOOL_CURVE,
            Tool::Polygon => super::icons::ICON_TOOL_POLYGON,
            Tool::SelectRect => super::icons::ICON_TOOL_SELECT_RECT,
            Tool::SelectFree => super::icons::ICON_TOOL_SELECT_FREE,
            Tool::Text => super::icons::ICON_TOOL_TEXT,
            Tool::Zoom => super::icons::ICON_TOOL_ZOOM,
        }
    }

    /// Both the direct keyboard shortcut and the legend/tool-strip key.
    pub fn key(self) -> char {
        match self {
            Tool::Pencil => '1',
            Tool::Brush => '2',
            Tool::Eraser => '3',
            Tool::ColorEraser => '4',
            Tool::Airbrush => '5',
            Tool::Fill => '6',
            Tool::Picker => '7',
            Tool::Line => '8',
            Tool::Rect => '9',
            Tool::RoundRect => '0',
            Tool::Ellipse => 'o',
            Tool::Curve => 'u',
            Tool::Polygon => 'p',
            Tool::SelectRect => 'r',
            Tool::SelectFree => 'f',
            Tool::Text => 't',
            Tool::Zoom => 'q',
        }
    }

    pub fn from_key(key: char) -> Option<Tool> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    pub fn interaction(self) -> Interaction {
        match self {
            Tool::Pencil | Tool::Brush | Tool::Eraser | Tool::ColorEraser | Tool::Airbrush => Interaction::Freehand,
            Tool::Fill | Tool::Picker => Interaction::OneShot,
            Tool::Line | Tool::Rect | Tool::RoundRect | Tool::Ellipse => Interaction::Shape,
            Tool::Curve => Interaction::Curve,
            Tool::Polygon => Interaction::Polygon,
            Tool::SelectRect | Tool::SelectFree => Interaction::Select,
            Tool::Text => Interaction::Text,
            Tool::Zoom => Interaction::Zoom,
        }
    }
}

/// Tracks an in-progress multi-step gesture (shape/curve/polygon tools).
/// Freehand and one-shot tools don't need this — they act immediately.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DragState {
    #[default]
    None,
    Shape {
        start: (u16, u16),
    },
    /// Curve, stage 1: dragging out the straight chord.
    CurveLine {
        start: (u16, u16),
    },
    /// Curve, stage 2: bending the chord's midpoint into a control point.
    CurveBend {
        start: (u16, u16),
        end: (u16, u16),
    },
    Polygon {
        points: Vec<(u16, u16)>,
    },
    /// Rectangular-select drag: `start` to the current cursor.
    Marquee {
        start: (u16, u16),
    },
    /// Free-form select drag: the traced boundary path so far (already
    /// Bresenham-interpolated point-to-point, same as freehand tools).
    Lasso {
        points: Vec<(u16, u16)>,
    },
    /// Dragging a lifted selection (or a fresh paste) to a new position;
    /// `anchor` is where the drag started, so `cursor - anchor` is the live
    /// offset applied to every floating cell.
    MovingFloating {
        anchor: (u16, u16),
    },
    /// Composing text: `origin` is where entry started (the leftmost
    /// column Backspace won't go past); the live insertion point is
    /// `PaintState::cursor`.
    Text {
        origin: (u16, u16),
    },
}

/// 4-connected flood fill from `(x, y)`, replacing every reachable cell
/// that matches the source cell's contents with `replacement`.
pub fn flood_fill(canvas: &mut Canvas, delta: &mut Delta, x: u16, y: u16, replacement: Option<PaintCellSer>) {
    if !canvas.in_bounds(x, y) {
        return;
    }
    let target = canvas.get(x, y);
    if target == replacement {
        return;
    }
    let mut stack = vec![(x, y)];
    let mut seen = HashSet::new();
    seen.insert((x, y));
    while let Some((cx, cy)) = stack.pop() {
        if canvas.get(cx, cy) != target {
            continue;
        }
        delta.record(canvas, cx, cy);
        canvas.set(cx, cy, replacement.clone());
        for (nx, ny) in [(cx.wrapping_sub(1), cy), (cx + 1, cy), (cx, cy.wrapping_sub(1)), (cx, cy + 1)] {
            if canvas.in_bounds(nx, ny) && seen.insert((nx, ny)) {
                stack.push((nx, ny));
            }
        }
    }
}

/// Bresenham's line algorithm — every cell from `(x0, y0)` to `(x1, y1)`
/// inclusive. Used to interpolate freehand strokes between drag events so
/// fast mouse motion doesn't leave gaps, and later reused by the Line tool.
pub fn line_cells(x0: u16, y0: u16, x1: u16, y1: u16) -> Vec<(u16, u16)> {
    let (mut x0, mut y0) = (x0 as i32, y0 as i32);
    let (x1, y1) = (x1 as i32, y1 as i32);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut out = Vec::new();
    loop {
        out.push((x0 as u16, y0 as u16));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
    out
}

/// A tiny, dependency-free xorshift PRNG — just enough randomness for the
/// airbrush's spray pattern, not worth pulling in the `rand` crate for.
pub fn xorshift(seed: &mut u64) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed % 1000) as f32 / 1000.0
}

/// The outline cells of the axis-aligned rectangle with `(x0,y0)` and
/// `(x1,y1)` as opposite corners.
pub fn rect_outline_cells(x0: u16, y0: u16, x1: u16, y1: u16) -> Vec<(u16, u16)> {
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    let mut out = Vec::new();
    for x in x0..=x1 {
        out.push((x, y0));
        out.push((x, y1));
    }
    for y in y0..=y1 {
        out.push((x0, y));
        out.push((x1, y));
    }
    out
}

/// Same as `rect_outline_cells`, minus its four corner cells — a
/// terminal-cell-resolution approximation of a rounded corner.
pub fn rounded_rect_outline_cells(x0: u16, y0: u16, x1: u16, y1: u16) -> Vec<(u16, u16)> {
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    rect_outline_cells(x0, y0, x1, y1).into_iter().filter(|c| !corners.contains(c)).collect()
}

/// The outline cells of the ellipse inscribed in the rect spanning
/// `(x0,y0)`..`(x1,y1)`, found by sampling its parametric form and
/// Bresenham-connecting consecutive samples. Simpler and more robust at
/// terminal-cell resolution than the classic midpoint algorithm's
/// quadrant-mirroring, for no visible difference in output.
pub fn ellipse_cells(x0: u16, y0: u16, x1: u16, y1: u16) -> Vec<(u16, u16)> {
    let (x0, x1) = (x0.min(x1) as f64, x0.max(x1) as f64);
    let (y0, y1) = (y0.min(y1) as f64, y0.max(y1) as f64);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    if rx < 0.5 || ry < 0.5 {
        return line_cells(x0 as u16, y0 as u16, x1 as u16, y1 as u16);
    }
    let steps = ((rx.max(ry)) * 8.0).max(32.0) as usize;
    sample_parametric(steps, std::f64::consts::TAU, |t| (cx + rx * t.cos(), cy + ry * t.sin()))
}

/// A quadratic Bézier from `p0` through control point `ctrl` to `p1`.
pub fn quad_bezier_cells(p0: (u16, u16), ctrl: (u16, u16), p1: (u16, u16)) -> Vec<(u16, u16)> {
    let (x0, y0) = (p0.0 as f64, p0.1 as f64);
    let (cx, cy) = (ctrl.0 as f64, ctrl.1 as f64);
    let (x1, y1) = (p1.0 as f64, p1.1 as f64);
    sample_parametric(24, 1.0, move |t| {
        let mt = 1.0 - t;
        (mt * mt * x0 + 2.0 * mt * t * cx + t * t * x1, mt * mt * y0 + 2.0 * mt * t * cy + t * t * y1)
    })
}

/// Samples `f` at `steps + 1` evenly spaced points over the parameter range
/// `0..=t_scale` (`TAU` for the ellipse, `1.0` for the Bézier),
/// Bresenham-connecting consecutive samples into one continuous cell path.
fn sample_parametric(steps: usize, t_scale: f64, f: impl Fn(f64) -> (f64, f64)) -> Vec<(u16, u16)> {
    let mut out = HashSet::new();
    let mut prev: Option<(u16, u16)> = None;
    for i in 0..=steps {
        let t = i as f64 / steps as f64 * t_scale;
        let (px, py) = f(t);
        if px < 0.0 || py < 0.0 {
            continue;
        }
        let p = (px.round() as u16, py.round() as u16);
        match prev {
            Some(pv) => out.extend(line_cells(pv.0, pv.1, p.0, p.1)),
            None => {
                out.insert(p);
            }
        }
        prev = Some(p);
    }
    out.into_iter().collect()
}
