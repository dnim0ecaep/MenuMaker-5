//! Paint's DeskMate-style drawing and the mouse handling that mirrors it.
//! `render` records every clickable rectangle in `LayoutCache` (absolute
//! screen coordinates) and `handle_mouse` hit-tests against exactly that.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::dmui::{self, dm_box, fit_width, pad_width, screen_chrome, Palette};

use super::palette::{ColorComposer, RgbChannel, RgbColor};
use super::selection::Selection;
use super::tools::{DragState, Interaction, Tool};
use super::filedialog::{DialogPurpose, DialogResult, FileDialog};
use super::{tools, PaintApp, PaintResult, ResizeMode, MENU_BAR};
use std::path::PathBuf;

/// What a click on a recorded button does.
#[derive(Clone, Copy, Debug)]
pub(super) enum Click {
    /// Behave exactly like this key press.
    Key(KeyEvent),
    /// Make this the composer's active channel.
    Channel(RgbChannel),
    /// Make this the active channel and nudge it.
    Adjust(RgbChannel, i32),
}

/// Hit-test rectangles from the last `render`, in absolute coordinates.
#[derive(Default)]
pub(super) struct LayoutCache {
    /// Shortcut chips and dialog buttons; later entries are drawn on top.
    pub(super) buttons: Vec<(Rect, Click)>,
    pub(super) menu_bar: Vec<(Rect, usize)>,
    pub(super) dropdown: Vec<(Rect, usize)>,
    pub(super) tools: Vec<(Rect, Tool)>,
    pub(super) brush: Rect,
    pub(super) fg_swatch: Rect,
    pub(super) bg_swatch: Rect,
    pub(super) swatches: Vec<(Rect, usize)>,
    /// The canvas box's inner area.
    pub(super) viewport: Rect,
    pub(super) palette_cells: Vec<(Rect, usize)>,
    /// File dialog rows -> index into `FileDialog::visible()`.
    pub(super) file_rows: Vec<(Rect, usize)>,
}

fn hit(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

fn text_w(s: &str) -> u16 {
    UnicodeWidthStr::width(s).min(u16::MAX as usize) as u16
}

/// Writes `text` at `(x, y)`, clipped to `max_w` columns and the buffer.
fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, max_w: u16) {
    let a = buf.area;
    if y < a.y || y >= a.bottom() || x < a.x || x >= a.right() || max_w == 0 {
        return;
    }
    let w = max_w.min(a.right() - x);
    buf.set_stringn(x, y, text, w as usize, style);
}

/// Replaces one cell if it lies inside both `clip` and the buffer.
fn put_cell(buf: &mut Buffer, clip: Rect, x: u16, y: u16, ch: char, style: Style) {
    if !hit(clip, x, y) || !hit(buf.area, x, y) {
        return;
    }
    let cell = buf.get_mut(x, y);
    cell.reset();
    cell.set_char(ch);
    cell.set_style(style);
}

fn centered(screen: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(screen.width);
    let h = h.min(screen.height);
    Rect { x: screen.x + (screen.width - w) / 2, y: screen.y + (screen.height - h) / 2, width: w, height: h }
}

/// Black or white, whichever reads on top of `color`.
fn contrast_mark(color: RgbColor) -> Color {
    let luminance = color.0 as u32 * 299 + color.1 as u32 * 587 + color.2 as u32 * 114;
    if luminance > 500_000 {
        Color::Black
    } else {
        Color::White
    }
}

fn button_style(pal: &Palette) -> Style {
    Style::default().fg(pal.surface).bg(pal.accent).add_modifier(Modifier::BOLD)
}

/// Keeps the end of `text` (e.g. a long folder path) within `width`.
fn tail_fit(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut used = 1; // the leading ellipsis
    for &ch in chars.iter().rev() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.reverse();
    format!("…{}", out.into_iter().collect::<String>())
}

/// A dialog's inner drawing area; rows are relative to `inner.y`, text is
/// indented one column.
struct Dialog {
    inner: Rect,
}

impl Dialog {
    /// Draws the dialog box and drops any click targets it now covers.
    fn open(frame: &mut Frame, screen: Rect, title: &str, w: u16, h: u16, pal: &Palette, layout: &mut LayoutCache) -> Self {
        let rect = centered(screen, w, h);
        layout.buttons.retain(|(r, _)| r.intersection(rect).area() == 0);
        let inner = dm_box(frame, rect, title, pal, pal.fill(), true);
        Self { inner }
    }

    fn row_y(&self, row: u16) -> Option<u16> {
        (row < self.inner.height).then_some(self.inner.y + row)
    }

    fn text_width(&self) -> u16 {
        self.inner.width.saturating_sub(2)
    }

    fn line(&self, buf: &mut Buffer, row: u16, text: &str, style: Style) {
        if let Some(y) = self.row_y(row) {
            put(buf, self.inner.x + 1, y, text, style, self.text_width());
        }
    }

    /// A centered row of buttons, each recorded as a click target.
    fn buttons(&self, buf: &mut Buffer, row: u16, items: &[(&str, Click)], pal: &Palette, layout: &mut LayoutCache) {
        let Some(y) = self.row_y(row) else { return };
        let total: u16 = items.iter().map(|(l, _)| text_w(l) + 2).sum::<u16>() + 2 * items.len().saturating_sub(1) as u16;
        let mut x = self.inner.x + self.inner.width.saturating_sub(total) / 2;
        for (label, click) in items {
            let w = text_w(label) + 2;
            if x >= self.inner.right() {
                break;
            }
            let shown = w.min(self.inner.right() - x);
            put(buf, x, y, &format!(" {label} "), button_style(pal), shown);
            layout.buttons.push((Rect { x, y, width: shown, height: 1 }, *click));
            x = x.saturating_add(w + 2);
        }
    }

    /// `label  ◄ value ►` with clickable arrows.
    #[allow(clippy::too_many_arguments)]
    fn spinner(
        &self,
        buf: &mut Buffer,
        row: u16,
        label: &str,
        label_style: Style,
        value: &str,
        dec: Click,
        inc: Click,
        pal: &Palette,
        layout: &mut LayoutCache,
    ) {
        let Some(y) = self.row_y(row) else { return };
        let x0 = self.inner.x + 1;
        let max = self.text_width();
        put(buf, x0, y, &pad_width(label, 12), label_style, max);
        let arrow_x = x0 + 13;
        if arrow_x + 12 > self.inner.right() {
            return;
        }
        put(buf, arrow_x, y, "◄", button_style(pal), 1);
        layout.buttons.push((Rect { x: arrow_x, y, width: 1, height: 1 }, dec));
        put(buf, arrow_x + 1, y, &format!(" {:^8} ", value), pal.selected(), 10);
        put(buf, arrow_x + 11, y, "►", button_style(pal), 1);
        layout.buttons.push((Rect { x: arrow_x + 11, y, width: 1, height: 1 }, inc));
    }
}

fn key(code: KeyCode) -> Click {
    Click::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

impl PaintApp {
    /// Draws the whole screen: `dmui::screen_chrome` (title, clickable
    /// shortcut chips, status) and the Paint UI in the desktop area.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, title: &str, pal: &Palette) {
        let mut layout = LayoutCache::default();

        // Only as many chips as fit, so every visible chip is clickable.
        let chips = self.chips();
        let mut count = chips.len();
        let spans = loop {
            let pairs: Vec<(&str, &str)> = chips[..count].iter().map(|c| (c.key, c.label)).collect();
            if let Some(spans) = dmui::shortcut_spans(area.width, &pairs) {
                break spans;
            }
            count -= 1;
        };
        let pairs: Vec<(&str, &str)> = chips[..count].iter().map(|c| (c.key, c.label)).collect();
        let heading = format!("{title} - Paint - {}", self.file_label());
        let status = self.status_text();
        let desk = screen_chrome(frame, area, &heading, &pairs, &status, pal);
        if area.height >= 4 {
            for (chip, (x, w)) in chips.iter().zip(spans) {
                layout.buttons.push((Rect { x: area.x + x, y: area.y + 1, width: w, height: 1 }, Click::Key(chip.event)));
            }
        }

        self.render_desk(frame, desk, pal, &mut layout);
        self.render_dropdown(frame, area, pal, &mut layout);
        // Dialogs sit between the shortcut bar and the status bar when
        // there's room, so the chips stay visible.
        let dialog_area = if area.height > 12 {
            Rect { y: area.y + 2, height: area.height - 3, ..area }
        } else {
            area
        };
        self.render_dialogs(frame, dialog_area, pal, &mut layout);
        self.layout = layout;
    }

    fn render_desk(&mut self, frame: &mut Frame, desk: Rect, pal: &Palette, layout: &mut LayoutCache) {
        if desk.width < 12 || desk.height < 2 {
            return;
        }
        self.render_menu_bar(frame.buffer_mut(), Rect { height: 1, ..desk }, pal, layout);
        let body = Rect { x: desk.x, y: desk.y + 1, width: desk.width, height: desk.height - 1 };
        if body.height < 3 {
            return;
        }
        let toolbox_w = self.render_toolbox(frame, body, pal, layout);
        let gap = u16::from(toolbox_w > 0);
        let right = Rect {
            x: body.x + toolbox_w + gap,
            y: body.y,
            width: body.width.saturating_sub(toolbox_w + gap),
            height: body.height,
        };
        if right.width < 6 {
            return;
        }

        // The Color Box sits under the canvas; the canvas keeps >= 3 rows.
        let swatch_count = self.palette.len() as u16;
        let cb_cols = (right.width.saturating_sub(2) / 2).max(1);
        let wanted_rows = if swatch_count == 0 { 0 } else { swatch_count.div_ceil(cb_cols).min(2) };
        let spare = right.height.saturating_sub(3);
        let cb_h = if spare >= 3 { 3 + wanted_rows.min(spare - 3) } else { 0 };
        let canvas_rect = Rect { height: right.height - cb_h, ..right };

        let focused = !self.is_modal() && self.menu_open.is_none();
        let zoom = if self.viewport.zoom != 1 { format!(" {}%", self.viewport.zoom_percent()) } else { String::new() };
        let title = format!("CANVAS {}x{}{zoom}", self.canvas.width, self.canvas.height);
        let inner = dm_box(frame, canvas_rect, &title, pal, pal.fill(), focused);
        layout.viewport = inner;
        self.render_canvas(frame.buffer_mut(), inner);

        if cb_h > 0 {
            let cb_rect = Rect { x: right.x, y: right.y + canvas_rect.height, width: right.width, height: cb_h };
            self.render_color_box(frame, cb_rect, pal, layout);
        }
    }

    /// The DeskMate command bar: `File F2  Edit F3 …`.
    fn render_menu_bar(&self, buf: &mut Buffer, row: Rect, pal: &Palette, layout: &mut LayoutCache) {
        let bar = Style::default().bg(pal.bar_bg).fg(pal.bar_label);
        let key_style = Style::default().bg(pal.bar_bg).fg(pal.bar_key).add_modifier(Modifier::BOLD);
        put(buf, row.x, row.y, &" ".repeat(row.width as usize), bar, row.width);
        let mut x = row.x + 1;
        for (idx, menu) in MENU_BAR.iter().enumerate() {
            let fkey = format!("F{}", menu.fkey);
            let w = text_w(menu.label) + text_w(&fkey) + 3;
            if x + w > row.right() {
                break;
            }
            if self.menu_open == Some(idx) {
                put(buf, x, row.y, &format!(" {} {fkey} ", menu.label), pal.selected(), w);
            } else {
                put(buf, x, row.y, &format!(" {} ", menu.label), bar, w);
                put(buf, x + 2 + text_w(menu.label), row.y, &format!("{fkey} "), key_style, w);
            }
            layout.menu_bar.push((Rect { x, y: row.y, width: w, height: 1 }, idx));
            x += w + 1;
        }
    }

    /// The TOOLS box along the left; returns its width (0 when skipped).
    /// Falls back to compact key+icon cells when full labels won't fit.
    fn render_toolbox(&self, frame: &mut Frame, body: Rect, pal: &Palette, layout: &mut LayoutCache) -> u16 {
        let inner_h = body.height.saturating_sub(2);
        if inner_h == 0 || body.width < 24 {
            return 0;
        }
        let count = Tool::ALL.len() as u16;
        let cols_needed = count.div_ceil(inner_h);
        let max_w = body.width / 3;
        let (col_w, cols) = if cols_needed * 12 + 2 <= max_w {
            (12, cols_needed)
        } else if cols_needed * 4 + 2 <= max_w {
            (4, cols_needed)
        } else {
            (4, (max_w.saturating_sub(2) / 4).max(1))
        };
        let box_w = cols * col_w + 2;
        let rect = Rect { x: body.x, y: body.y, width: box_w, height: body.height };
        let inner = dm_box(frame, rect, "TOOLS", pal, pal.fill(), false);
        let buf = frame.buffer_mut();
        for (i, tool) in Tool::ALL.into_iter().enumerate() {
            let (col, row) = (i as u16 / inner_h, i as u16 % inner_h);
            if col >= cols {
                break;
            }
            let (x, y) = (inner.x + col * col_w, inner.y + row);
            let text = if col_w == 12 {
                format!(" {} {} {}", tool.key(), tool.icon(), tool.strip_label())
            } else {
                format!(" {}{}", tool.key(), tool.icon())
            };
            let style = if tool == self.tool { pal.selected() } else { pal.fill() };
            put(buf, x, y, &pad_width(&text, col_w as usize), style, col_w);
            layout.tools.push((Rect { x, y, width: col_w, height: 1 }, tool));
        }
        box_w
    }

    /// The COLORS box: brush/Fg/Bg info row plus clickable swatches.
    fn render_color_box(&self, frame: &mut Frame, rect: Rect, pal: &Palette, layout: &mut LayoutCache) {
        let inner = dm_box(frame, rect, "COLORS", pal, pal.fill(), false);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let buf = frame.buffer_mut();
        let brush = self.brushes[self.brush_idx];
        let brush_txt = format!(" Brush:[{}]", if brush == ' ' { '⌫' } else { brush });
        let bw = text_w(&brush_txt).min(inner.width);
        put(buf, inner.x, inner.y, &brush_txt, pal.label(), inner.width);
        layout.brush = Rect { x: inner.x, y: inner.y, width: bw, height: 1 };
        let mut x = inner.x + bw;
        for (label, idx, is_fg) in [(" Fg ", self.fg_idx, true), (" Bg ", self.bg_idx, false)] {
            if x + 6 > inner.right() {
                break;
            }
            put(buf, x, inner.y, label, pal.label(), 4);
            let color = self.palette.color_at(idx);
            let swatch = Rect { x: x + 4, y: inner.y, width: 2, height: 1 };
            put(buf, swatch.x, swatch.y, "  ", Style::default().bg(color.into()), 2);
            if is_fg {
                layout.fg_swatch = swatch;
            } else {
                layout.bg_swatch = swatch;
            }
            x += 6;
        }
        let hint = "  click: L=Fg R=Bg";
        if x + text_w(hint) <= inner.right() {
            put(buf, x, inner.y, hint, pal.fill().add_modifier(Modifier::ITALIC), inner.right() - x);
        }

        let cols = (inner.width / 2).max(1);
        for (i, &color) in self.palette.colors().iter().enumerate() {
            let row = i as u16 / cols;
            if row + 1 >= inner.height {
                break;
            }
            let col = i as u16 % cols;
            let (sx, sy) = (inner.x + col * 2, inner.y + 1 + row);
            if sx + 2 > inner.right() {
                continue;
            }
            let style = Style::default().bg(color.into());
            put(buf, sx, sy, "  ", style, 2);
            let mark = style.fg(contrast_mark(color));
            if i == self.fg_idx {
                put(buf, sx, sy, "F", mark, 1);
            }
            if i == self.bg_idx {
                put(buf, sx + 1, sy, "B", mark, 1);
            }
            layout.swatches.push((Rect { x: sx, y: sy, width: 2, height: 1 }, i));
        }
    }

    /// Draws the visible part of the canvas (checkerboard for blank cells,
    /// selection/preview/cursor overlays, a floating selection on top).
    fn render_canvas(&mut self, buf: &mut Buffer, vp: Rect) {
        if vp.width == 0 || vp.height == 0 {
            return;
        }
        let (vis_w, vis_h) = self.viewport.visible(vp.width, vp.height, &self.canvas);
        self.last_visible = (vis_w, vis_h);
        let zoom = self.viewport.zoom;
        let (sx, sy) = (self.viewport.scroll_x, self.viewport.scroll_y);
        let preview: std::collections::HashSet<(u16, u16)> = self.preview_cells().into_iter().collect();
        let selected = |sel: &Option<Selection>, x: u16, y: u16| sel.as_ref().is_some_and(|s| s.contains(x, y));
        let checker = |x: u16, y: u16| {
            if (x / 2 + y).is_multiple_of(2) {
                Color::Rgb(28, 28, 34)
            } else {
                Color::Rgb(20, 20, 25)
            }
        };

        if zoom > 0 {
            let zoom = zoom as u16;
            for cy in sy..sy.saturating_add(vis_h) {
                for cx in sx..sx.saturating_add(vis_w) {
                    let mut style = Style::default().bg(checker(cx, cy));
                    let mut ch = ' ';
                    let painted = self.canvas.get(cx, cy);
                    if let Some(c) = &painted {
                        ch = c.ch;
                        style = Style::default().fg(c.fg.into()).bg(c.bg.into());
                    }
                    if selected(&self.selection, cx, cy) {
                        style = style.bg(Color::Rgb(60, 100, 200));
                    }
                    if preview.contains(&(cx, cy)) {
                        style = style.bg(Color::Rgb(255, 190, 60)).fg(Color::Black);
                        if ch == ' ' {
                            ch = '▒';
                        }
                    }
                    if (cx, cy) == self.cursor {
                        style = style.bg(Color::Rgb(255, 255, 255)).fg(Color::Black);
                        if ch == ' ' && painted.is_none() {
                            ch = '+';
                        }
                    }
                    let lx = (cx - sx) * zoom;
                    let ly = (cy - sy) * zoom;
                    for dy in 0..zoom {
                        for dx in 0..zoom {
                            put_cell(buf, vp, vp.x + lx + dx, vp.y + ly + dy, ch, style);
                        }
                    }
                }
            }
        } else {
            // Zoomed out: each terminal cell stands for an out×out block;
            // show the cursor if it's inside, else the first painted cell.
            let out = (-zoom) as u16;
            let cols = vis_w.div_ceil(out);
            let rows = vis_h.div_ceil(out);
            for ly in 0..rows {
                let by0 = sy + ly * out;
                let by1 = (by0 + out).min(sy + vis_h);
                for lx in 0..cols {
                    let bx0 = sx + lx * out;
                    let bx1 = (bx0 + out).min(sx + vis_w);
                    let mut rep = (bx0, by0);
                    let mut rep_painted = self.canvas.get(bx0, by0);
                    let (mut is_cursor, mut any_selected, mut any_preview) = (false, false, false);
                    for by in by0..by1 {
                        for bx in bx0..bx1 {
                            any_selected |= selected(&self.selection, bx, by);
                            any_preview |= preview.contains(&(bx, by));
                            if (bx, by) == self.cursor {
                                rep = (bx, by);
                                rep_painted = self.canvas.get(bx, by);
                                is_cursor = true;
                            } else if !is_cursor && rep_painted.is_none() {
                                let p = self.canvas.get(bx, by);
                                if p.is_some() {
                                    rep = (bx, by);
                                    rep_painted = p;
                                }
                            }
                        }
                    }
                    let mut style = Style::default().bg(checker(rep.0, rep.1));
                    let mut ch = ' ';
                    if let Some(c) = &rep_painted {
                        ch = c.ch;
                        style = Style::default().fg(c.fg.into()).bg(c.bg.into());
                    }
                    if any_selected {
                        style = style.bg(Color::Rgb(60, 100, 200));
                    }
                    if any_preview {
                        style = style.bg(Color::Rgb(255, 190, 60)).fg(Color::Black);
                        if ch == ' ' {
                            ch = '▒';
                        }
                    }
                    if is_cursor {
                        style = style.bg(Color::Rgb(255, 255, 255)).fg(Color::Black);
                        if ch == ' ' && rep_painted.is_none() {
                            ch = '+';
                        }
                    }
                    put_cell(buf, vp, vp.x + lx, vp.y + ly, ch, style);
                }
            }
        }

        // A lifted selection being dragged, at its live offset.
        if let DragState::MovingFloating { anchor } = &self.shape {
            let (step, block): (u16, u16) = if zoom > 0 { (1, zoom as u16) } else { ((-zoom) as u16, 1) };
            let (dx, dy) = (self.cursor.0 as i32 - anchor.0 as i32, self.cursor.1 as i32 - anchor.1 as i32);
            for (&(ox, oy), cell) in &self.floating {
                let (nx, ny) = (ox as i32 + dx, oy as i32 + dy);
                if nx < sx as i32 || ny < sy as i32 || nx >= (sx + vis_w) as i32 || ny >= (sy + vis_h) as i32 {
                    continue;
                }
                let (nx, ny) = (nx as u16, ny as u16);
                let lx = (nx - sx) / step * block;
                let ly = (ny - sy) / step * block;
                let style = Style::default().fg(cell.fg.into()).bg(cell.bg.into());
                for dyy in 0..block {
                    for dxx in 0..block {
                        put_cell(buf, vp, vp.x + lx + dxx, vp.y + ly + dyy, cell.ch, style);
                    }
                }
            }
        }
    }

    fn render_dropdown(&self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut LayoutCache) {
        let Some(menu_idx) = self.menu_open else { return };
        let Some(&(bar, _)) = layout.menu_bar.iter().find(|(_, i)| *i == menu_idx) else { return };
        let items = MENU_BAR[menu_idx].items;
        let label_w = items.iter().map(|i| text_w(i.label)).max().unwrap_or(0);
        let hint_w = items.iter().map(|i| text_w(i.hint)).max().unwrap_or(0);
        let w = (label_w + hint_w + 7).min(area.width);
        let h = (items.len() as u16 + 2).min(area.bottom().saturating_sub(bar.y + 1));
        if w < 6 || h < 3 {
            return;
        }
        let x = bar.x.min(area.right().saturating_sub(w));
        let rect = Rect { x, y: bar.y + 1, width: w, height: h };
        let inner = dm_box(frame, rect, "", pal, pal.fill(), false);
        let buf = frame.buffer_mut();
        for (i, item) in items.iter().enumerate() {
            let row = i as u16;
            if row >= inner.height {
                break;
            }
            let y = inner.y + row;
            if item.is_separator() {
                let rule = "─".repeat(inner.width as usize);
                put(buf, inner.x, y, &rule, Style::default().fg(pal.accent).bg(pal.surface), inner.width);
                continue;
            }
            let hint = format!("{}{}", " ".repeat((hint_w - text_w(item.hint)) as usize), item.hint);
            let text = format!(" {}   {} ", pad_width(item.label, label_w as usize), hint);
            let style = if self.menu_selected == i { pal.selected() } else { pal.fill() };
            put(buf, inner.x, y, &pad_width(&text, inner.width as usize), style, inner.width);
            layout.dropdown.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, i));
        }
    }

    fn render_dialogs(&mut self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut LayoutCache) {
        let fill = pal.fill();
        let italic = fill.add_modifier(Modifier::ITALIC);
        if let Some(info) = &self.info {
            let w = info.lines.iter().map(|l| text_w(l)).max().unwrap_or(0) + 4;
            let h = info.lines.len() as u16 + 4;
            let d = Dialog::open(frame, area, info.title, w.max(24), h, pal, layout);
            let buf = frame.buffer_mut();
            for (i, line) in info.lines.iter().enumerate() {
                d.line(buf, i as u16, line, fill);
            }
            d.buttons(buf, info.lines.len() as u16 + 1, &[("OK", key(KeyCode::Enter))], pal, layout);
        } else if self.file_dialog.is_some() {
            self.render_file_dialog(frame, area, pal, layout);
        } else if self.awaiting_brush_char {
            let d = Dialog::open(frame, area, "Custom Brush", 44, 6, pal, layout);
            let buf = frame.buffer_mut();
            d.line(buf, 1, "Type the character to paint with.", fill);
            d.buttons(buf, 3, &[("Esc Cancel", key(KeyCode::Esc))], pal, layout);
        } else if self.palette_picker.is_some() {
            self.render_palette_picker(frame, area, pal, layout);
        } else if self.pending_unsaved_action.is_some() {
            let msg = format!("Save changes to {}?", self.file_label());
            let d = Dialog::open(frame, area, "Unsaved Changes", (text_w(&msg) + 6).max(40), 6, pal, layout);
            let buf = frame.buffer_mut();
            d.line(buf, 1, &msg, fill);
            let items = [
                ("S Save", key(KeyCode::Char('s'))),
                ("D Discard", key(KeyCode::Char('d'))),
                ("Esc Cancel", key(KeyCode::Esc)),
            ];
            d.buttons(buf, 3, &items, pal, layout);
        } else if let Some(draft) = &self.save_as_draft {
            let d = Dialog::open(frame, area, "Save Painting As", 60, 9, pal, layout);
            let tw = d.text_width() as usize;
            let buf = frame.buffer_mut();
            let folder = self.save_dir().display().to_string();
            d.line(buf, 0, &format!("Folder: {}", tail_fit(&folder, tw.saturating_sub(8))), fill);
            if let Some(y) = d.row_y(2) {
                let x = d.inner.x + 1;
                put(buf, x, y, "Name: ", pal.label(), d.text_width());
                let field_w = d.text_width().saturating_sub(6);
                let shown = tail_fit(draft, field_w.saturating_sub(1) as usize);
                put(buf, x + 6, y, &pad_width(&shown, field_w as usize), pal.selected(), field_w);
                if field_w > 0 {
                    frame.set_cursor((x + 6 + text_w(&shown)).min(x + 5 + field_w), y);
                }
            }
            let buf = frame.buffer_mut();
            d.line(buf, 4, "Formats: .ans .txt .html .svg .irc (default .ans)", italic);
            d.buttons(buf, 6, &[("Enter Save", key(KeyCode::Enter)), ("Esc Cancel", key(KeyCode::Esc))], pal, layout);
        } else if let Some(path) = &self.overwrite_confirm {
            let msg = format!("{} already exists.", super::file_name(path));
            let d = Dialog::open(frame, area, "Confirm", (text_w(&msg) + 6).max(36), 7, pal, layout);
            let buf = frame.buffer_mut();
            d.line(buf, 1, &msg, fill);
            d.line(buf, 2, "Replace it?", fill);
            d.buttons(buf, 4, &[("Y Yes", key(KeyCode::Char('y'))), ("N No", key(KeyCode::Char('n')))], pal, layout);
        } else if let Some((w, h)) = self.resize_draft {
            self.render_resize_dialog(frame, area, (w, h), pal, layout);
        } else if let Some(draft) = &self.color_draft {
            let d = Dialog::open(frame, area, "Add Color", 44, 10, pal, layout);
            let buf = frame.buffer_mut();
            render_composer(&d, buf, 0, draft, pal, layout);
            let items = [
                ("Enter Add", key(KeyCode::Enter)),
                ("# Hex", key(KeyCode::Char('#'))),
                ("Esc Cancel", key(KeyCode::Esc)),
            ];
            d.buttons(buf, 7, &items, pal, layout);
        }
    }

    fn render_resize_dialog(&self, frame: &mut Frame, area: Rect, (w, h): (u16, u16), pal: &Palette, layout: &mut LayoutCache) {
        let new = self.resize_is_new;
        let title = if new { "New Image" } else { "Image Attributes" };
        let d = Dialog::open(frame, area, title, 44, if new { 7 } else { 9 }, pal, layout);
        let buf = frame.buffer_mut();
        let mut row = 0;
        if !new {
            d.spinner(buf, 0, "Mode:", pal.label(), self.resize_mode.label(), key(KeyCode::Tab), key(KeyCode::Tab), pal, layout);
            row = 2;
        }
        let mode = if new { ResizeMode::Resize } else { self.resize_mode };
        let (l1, v1, l2, v2) = match mode {
            ResizeMode::Resize => ("Width:", w.to_string(), "Height:", h.to_string()),
            ResizeMode::Stretch => ("Horizontal:", format!("{w}%"), "Vertical:", format!("{h}%")),
            ResizeMode::Skew => ("Horizontal:", format!("{}°", self.skew_degrees.0), "Vertical:", format!("{}°", self.skew_degrees.1)),
        };
        d.spinner(buf, row, l1, pal.label(), &v1, key(KeyCode::Left), key(KeyCode::Right), pal, layout);
        d.spinner(buf, row + 1, l2, pal.label(), &v2, key(KeyCode::Up), key(KeyCode::Down), pal, layout);
        let apply = if new { "Enter Create" } else { "Enter Apply" };
        d.buttons(buf, row + 3, &[(apply, key(KeyCode::Enter)), ("Esc Cancel", key(KeyCode::Esc))], pal, layout);
    }

    fn render_palette_picker(&mut self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut LayoutCache) {
        let Some(picker) = &self.palette_picker else { return };
        let count = self.palette.len() as u16;
        let cols = (area.width.saturating_sub(6) / 4).clamp(1, 16);
        let grid_rows = count.div_ceil(cols);
        let editing = picker.editing.is_some();
        let h = grid_rows + 2 + if editing { 6 } else { 0 } + 2 + 2;
        let d = Dialog::open(frame, area, "Palette", (cols * 4 + 4).max(44), h, pal, layout);
        self.palette_grid_cols = cols;
        let buf = frame.buffer_mut();
        let grid_x = d.inner.x + d.inner.width.saturating_sub(cols * 4) / 2;
        for (i, &color) in self.palette.colors().iter().enumerate() {
            let (row, col) = (i as u16 / cols, i as u16 % cols);
            let Some(y) = d.row_y(row) else { break };
            let x = grid_x + col * 4;
            if x + 4 > d.inner.right() {
                continue;
            }
            let style = Style::default().bg(color.into());
            let mark = style.fg(contrast_mark(color)).add_modifier(Modifier::BOLD);
            put(buf, x, y, "    ", style, 4);
            if i == picker.selected {
                put(buf, x, y, "[", mark, 1);
                put(buf, x + 3, y, "]", mark, 1);
            }
            if i == self.fg_idx {
                put(buf, x + 1, y, "F", mark, 1);
            }
            if i == self.bg_idx {
                put(buf, x + 2, y, "B", mark, 1);
            }
            layout.palette_cells.push((Rect { x, y, width: 4, height: 1 }, i));
        }
        let c = self.palette.color_at(picker.selected);
        let info = format!("Swatch {}  #{:02X}{:02X}{:02X}", picker.selected, c.0, c.1, c.2);
        d.line(buf, grid_rows + 1, &info, pal.label());
        let mut row = grid_rows + 3;
        if let Some(editing) = &picker.editing {
            render_composer(&d, buf, grid_rows + 2, editing, pal, layout);
            row = grid_rows + 8;
            let items = [
                ("Enter Apply", key(KeyCode::Enter)),
                ("# Hex", key(KeyCode::Char('#'))),
                ("Esc Back", key(KeyCode::Esc)),
            ];
            d.buttons(buf, row, &items, pal, layout);
        } else {
            let items = [
                ("Enter Edit", key(KeyCode::Enter)),
                ("F Set Fg", key(KeyCode::Char('f'))),
                ("B Set Bg", key(KeyCode::Char('b'))),
                ("Esc Close", key(KeyCode::Esc)),
            ];
            d.buttons(buf, row, &items, pal, layout);
        }
    }

    fn render_file_dialog(&mut self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut LayoutCache) {
        if let Some(dialog) = &mut self.file_dialog {
            draw_file_dialog(dialog, frame, area, pal, layout);
        }
    }

    // ----- mouse -----

    /// All mouse events (down, drag, up, scroll). Coordinates are matched
    /// against the layout recorded by the last `render`.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PaintResult {
        let (x, y) = (mouse.column, mouse.row);
        if !hit(area, x, y) && !self.mouse_capture {
            return PaintResult::Continue;
        }
        // A canvas gesture in progress owns drags and the release.
        if self.mouse_capture && matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
            self.canvas_mouse(mouse);
            return PaintResult::Continue;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let found = self.layout.buttons.iter().rev().find(|(r, _)| hit(*r, x, y)).map(|(_, c)| *c);
            if let Some(click) = found {
                return self.click(click);
            }
        }
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        if self.info.is_some() {
            if down {
                self.info = None;
            }
            return PaintResult::Continue;
        }
        if self.file_dialog.is_some() {
            self.file_dialog_mouse(mouse);
            return PaintResult::Continue;
        }
        if self.palette_picker.is_some() {
            self.palette_picker_mouse(mouse);
            return PaintResult::Continue;
        }
        if self.is_modal() {
            return PaintResult::Continue;
        }
        if down {
            if let Some(&(_, idx)) = self.layout.menu_bar.iter().find(|(r, _)| hit(*r, x, y)) {
                if self.menu_open == Some(idx) {
                    self.close_menu();
                } else {
                    self.open_menu(idx);
                }
                return PaintResult::Continue;
            }
        }
        if let Some(menu_idx) = self.menu_open {
            if down {
                if let Some(&(_, item)) = self.layout.dropdown.iter().find(|(r, _)| hit(*r, x, y)) {
                    return self.activate_menu_item(menu_idx, item);
                }
                self.close_menu();
            }
            return PaintResult::Continue;
        }
        if let MouseEventKind::Down(button) = mouse.kind {
            if let Some(&(_, tool)) = self.layout.tools.iter().find(|(r, _)| hit(*r, x, y)) {
                self.tool = tool;
                self.abort_gesture();
                self.status = Some(format!("Tool: {}", tool.label()));
                return PaintResult::Continue;
            }
            if hit(self.layout.brush, x, y) {
                self.begin_brush_char_capture();
                return PaintResult::Continue;
            }
            if hit(self.layout.fg_swatch, x, y) || hit(self.layout.bg_swatch, x, y) {
                if button == MouseButton::Right {
                    self.swap_colors();
                } else {
                    self.begin_palette_picker();
                }
                return PaintResult::Continue;
            }
            if let Some(&(_, idx)) = self.layout.swatches.iter().find(|(r, _)| hit(*r, x, y)) {
                if button == MouseButton::Right {
                    self.bg_idx = idx;
                    self.status = Some(format!("Bg color: swatch {idx}"));
                } else {
                    self.fg_idx = idx;
                    self.status = Some(format!("Fg color: swatch {idx}"));
                }
                return PaintResult::Continue;
            }
        }
        self.canvas_mouse(mouse);
        PaintResult::Continue
    }

    fn click(&mut self, click: Click) -> PaintResult {
        match click {
            Click::Key(event) => self.handle_key(event),
            Click::Channel(channel) | Click::Adjust(channel, _) => {
                let composer = match &mut self.color_draft {
                    Some(c) => Some(c),
                    None => self.palette_picker.as_mut().and_then(|p| p.editing.as_mut()),
                };
                if let Some(composer) = composer {
                    composer.channel = channel;
                    if let Click::Adjust(_, delta) = click {
                        composer.adjust(delta);
                    }
                    self.status = Some(composer.status());
                }
                PaintResult::Continue
            }
        }
    }

    fn file_dialog_mouse(&mut self, mouse: MouseEvent) {
        let (x, y) = (mouse.column, mouse.row);
        let row = self.layout.file_rows.iter().find(|(r, _)| hit(*r, x, y)).map(|(_, i)| *i);
        let Some(dialog) = &mut self.file_dialog else { return };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = row {
                    let result = dialog.click(idx);
                    self.finish_file_dialog(result);
                }
            }
            MouseEventKind::ScrollUp => dialog.move_selection(-3),
            MouseEventKind::ScrollDown => dialog.move_selection(3),
            _ => {}
        }
    }

    fn palette_picker_mouse(&mut self, mouse: MouseEvent) {
        let MouseEventKind::Down(button) = mouse.kind else { return };
        let (x, y) = (mouse.column, mouse.row);
        let Some(&(_, idx)) = self.layout.palette_cells.iter().find(|(r, _)| hit(*r, x, y)) else { return };
        if button == MouseButton::Right {
            self.bg_idx = idx;
            self.status = Some(format!("Bg color: swatch {idx}"));
            return;
        }
        let Some(picker) = &mut self.palette_picker else { return };
        if picker.editing.is_some() {
            return;
        }
        if picker.selected == idx {
            self.begin_palette_edit();
        } else {
            picker.selected = idx;
        }
    }

    /// Mouse on the canvas viewport: painting gestures and wheel scrolling.
    fn canvas_mouse(&mut self, mouse: MouseEvent) {
        let vp = self.layout.viewport;
        let (x, y) = (mouse.column, mouse.row);
        if vp.width == 0 || vp.height == 0 {
            self.mouse_capture = false;
            return;
        }
        const WHEEL_STEP: i32 = 3;
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                if hit(vp, x, y) {
                    let (dx, dy) = match mouse.kind {
                        MouseEventKind::ScrollUp if shift => (-WHEEL_STEP, 0),
                        MouseEventKind::ScrollDown if shift => (WHEEL_STEP, 0),
                        MouseEventKind::ScrollUp => (0, -WHEEL_STEP),
                        MouseEventKind::ScrollDown => (0, WHEEL_STEP),
                        MouseEventKind::ScrollLeft => (-WHEEL_STEP, 0),
                        _ => (WHEEL_STEP, 0),
                    };
                    self.scroll_viewport(dx, dy);
                }
                return;
            }
            MouseEventKind::Moved => return,
            MouseEventKind::Down(_) if !hit(vp, x, y) => return,
            MouseEventKind::Drag(_) | MouseEventKind::Up(_) if !self.mouse_capture => return,
            _ => {}
        }
        // Clamp so a drag that leaves the viewport keeps tracking its edge.
        let lx = x.clamp(vp.x, vp.right() - 1) - vp.x;
        let ly = y.clamp(vp.y, vp.bottom() - 1) - vp.y;
        let (cx, cy) = self.viewport.to_canvas(lx, ly);
        if matches!(mouse.kind, MouseEventKind::Down(_)) && !self.canvas.in_bounds(cx, cy) {
            return;
        }
        let cx = cx.min(self.canvas.width.saturating_sub(1));
        let cy = cy.min(self.canvas.height.saturating_sub(1));
        match mouse.kind {
            MouseEventKind::Down(button) => {
                self.mouse_capture = true;
                self.cursor = (cx, cy);
                self.stroke_primary = button != MouseButton::Right;
                match self.tool.interaction() {
                    Interaction::Freehand => {
                        self.last_drag = Some((cx, cy));
                        self.begin_gesture();
                        self.apply_tool(cx, cy);
                    }
                    Interaction::OneShot => self.apply_tool(cx, cy),
                    Interaction::Shape => {
                        if self.shape == DragState::None {
                            self.shape = DragState::Shape { start: (cx, cy) };
                        }
                    }
                    Interaction::Curve => {
                        if self.shape == DragState::None {
                            self.shape = DragState::CurveLine { start: (cx, cy) };
                        }
                    }
                    Interaction::Polygon => {
                        let mut points = match std::mem::take(&mut self.shape) {
                            DragState::Polygon { points } => points,
                            _ => Vec::new(),
                        };
                        points.push((cx, cy));
                        self.shape = DragState::Polygon { points };
                    }
                    Interaction::Select => {
                        if self.selection.as_ref().is_some_and(|s| s.contains(cx, cy)) {
                            self.lift_selection((cx, cy));
                        } else {
                            self.selection = None;
                            self.shape = match self.tool {
                                Tool::SelectFree => DragState::Lasso { points: vec![(cx, cy)] },
                                _ => DragState::Marquee { start: (cx, cy) },
                            };
                        }
                    }
                    Interaction::Text => {
                        // Clicking away from a text edit commits it, then
                        // starts a new one here.
                        if matches!(self.shape, DragState::Text { .. }) {
                            self.end_gesture();
                        }
                        self.begin_gesture();
                        self.shape = DragState::Text { origin: (cx, cy) };
                    }
                    Interaction::Zoom => self.zoom_at((cx, cy), self.stroke_primary),
                }
            }
            MouseEventKind::Drag(_) => match self.tool.interaction() {
                Interaction::Freehand => {
                    let from = self.last_drag.unwrap_or((cx, cy));
                    for (ix, iy) in tools::line_cells(from.0, from.1, cx, cy) {
                        self.apply_tool(ix, iy);
                    }
                    self.cursor = (cx, cy);
                    self.last_drag = Some((cx, cy));
                }
                Interaction::OneShot | Interaction::Text => {}
                Interaction::Select => {
                    if let DragState::Lasso { points } = &mut self.shape {
                        if let Some(&last) = points.last() {
                            points.extend(tools::line_cells(last.0, last.1, cx, cy).into_iter().skip(1));
                        }
                    }
                    self.cursor = (cx, cy);
                }
                _ => self.cursor = (cx, cy),
            },
            MouseEventKind::Up(_) => {
                self.mouse_capture = false;
                match self.tool.interaction() {
                    Interaction::Freehand => {
                        self.end_gesture();
                        self.last_drag = None;
                    }
                    Interaction::Shape => {
                        if let DragState::Shape { start } = self.shape {
                            let cells = self.shape_cells(start, (cx, cy));
                            self.commit_cells(&cells);
                            self.shape = DragState::None;
                        }
                    }
                    Interaction::Curve => match self.shape {
                        DragState::CurveLine { start } => self.shape = DragState::CurveBend { start, end: (cx, cy) },
                        DragState::CurveBend { start, end } => {
                            let cells = tools::quad_bezier_cells(start, (cx, cy), end);
                            self.commit_cells(&cells);
                            self.shape = DragState::None;
                        }
                        _ => {}
                    },
                    Interaction::Select => match std::mem::take(&mut self.shape) {
                        DragState::Marquee { start } => self.selection = Some(Selection::rect(start, (cx, cy))),
                        DragState::Lasso { points } => self.finish_lasso(points),
                        DragState::MovingFloating { anchor } => self.commit_move(anchor, (cx, cy)),
                        other => self.shape = other,
                    },
                    Interaction::OneShot | Interaction::Polygon | Interaction::Text | Interaction::Zoom => {}
                }
            }
            _ => {}
        }
    }
}

/// The color composer rows (shared by Add Color and Edit Colors), starting
/// at `row`: preview, R/G/B spinners (click a label to pick the channel),
/// and the hex row (click to type a hex code).
fn render_composer(d: &Dialog, buf: &mut Buffer, row: u16, composer: &ColorComposer, pal: &Palette, layout: &mut LayoutCache) {
    let c = composer.color;
    if let Some(y) = d.row_y(row) {
        let x = d.inner.x + 1;
        put(buf, x, y, "Preview:", pal.label(), d.text_width());
        put(buf, x + 13, y, "            ", Style::default().bg(c.into()), d.inner.right().saturating_sub(x + 13));
    }
    for (i, (channel, name)) in [(RgbChannel::R, "Red"), (RgbChannel::G, "Green"), (RgbChannel::B, "Blue")].into_iter().enumerate() {
        let r = row + 1 + i as u16;
        let active = composer.channel == channel && composer.hex_input.is_none();
        let label = format!("{} {name}", if active { '►' } else { ' ' });
        let style = if active { pal.selected() } else { pal.label() };
        d.spinner(
            buf,
            r,
            &label,
            style,
            &composer.channel_value(channel).to_string(),
            Click::Adjust(channel, -1),
            Click::Adjust(channel, 1),
            pal,
            layout,
        );
        if let Some(y) = d.row_y(r) {
            layout.buttons.push((Rect { x: d.inner.x + 1, y, width: 12.min(d.text_width()), height: 1 }, Click::Channel(channel)));
        }
    }
    let hex = match &composer.hex_input {
        Some(buf) => format!("Hex: #{buf}_   (type 6 digits, Enter)"),
        None => format!("Hex: #{:02X}{:02X}{:02X}   (# to type one)", c.0, c.1, c.2),
    };
    d.line(buf, row + 4, &hex, pal.fill());
    if let Some(y) = d.row_y(row + 4) {
        if composer.hex_input.is_none() {
            layout.buttons.push((Rect { x: d.inner.x + 1, y, width: d.text_width(), height: 1 }, key(KeyCode::Char('#'))));
        }
    }
}

/// Draws the Open / Import / Icon file dialog and records its hit areas.
fn draw_file_dialog(dialog: &mut FileDialog, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut LayoutCache) {
    let w = area.width.saturating_sub(4).clamp(20, 72);
    let h = area.height.saturating_sub(2).clamp(8, 24);
    let d = Dialog::open(frame, area, dialog.purpose.title(), w, h, pal, layout);
    let tw = d.text_width();
    let fill = pal.fill();
    let folder = dialog.dir.display().to_string();
    let buf = frame.buffer_mut();
    d.line(buf, 0, &format!("Folder: {}", tail_fit(&folder, (tw as usize).saturating_sub(8))), pal.label());
    let looks_like_path = dialog.input.contains('/') || dialog.input.starts_with('~');
    let prompt = if looks_like_path { "Path: " } else { "Find: " };
    if let Some(y) = d.row_y(1) {
        let x = d.inner.x + 1;
        put(buf, x, y, prompt, pal.label(), tw);
        let field_w = tw.saturating_sub(6);
        let shown = tail_fit(&dialog.input, field_w.saturating_sub(1) as usize);
        put(buf, x + 6, y, &pad_width(&shown, field_w as usize), pal.selected(), field_w);
        if field_w > 0 {
            frame.set_cursor((x + 6 + text_w(&shown)).min(x + 5 + field_w), y);
        }
    }
    let buf = frame.buffer_mut();
    if let Some(y) = d.row_y(2) {
        put(buf, d.inner.x, y, &"─".repeat(d.inner.width as usize), Style::default().fg(pal.accent).bg(pal.surface), d.inner.width);
    }

    let list_h = d.inner.height.saturating_sub(5) as usize;
    dialog.page = list_h.max(1);
    let visible = dialog.visible();
    if dialog.selected >= visible.len() {
        dialog.selected = visible.len().saturating_sub(1);
    }
    if dialog.selected < dialog.scroll {
        dialog.scroll = dialog.selected;
    } else if list_h > 0 && dialog.selected >= dialog.scroll + list_h {
        dialog.scroll = dialog.selected + 1 - list_h;
    }
    for (row, (vis_idx, &entry_idx)) in visible.iter().enumerate().skip(dialog.scroll).take(list_h).enumerate() {
        let y = d.inner.y + 3 + row as u16;
        let entry = &dialog.entries[entry_idx];
        let text = if entry.name == ".." {
            " ..  (parent folder)".to_string()
        } else if entry.is_dir {
            format!(" {}/", entry.name)
        } else {
            format!(" {}", entry.name)
        };
        let style = if vis_idx == dialog.selected {
            pal.selected()
        } else if entry.is_dir {
            pal.label()
        } else {
            fill
        };
        put(buf, d.inner.x, y, &pad_width(&text, d.inner.width as usize), style, d.inner.width);
        layout.file_rows.push((Rect { x: d.inner.x, y, width: d.inner.width, height: 1 }, vis_idx));
    }
    if visible.is_empty() || (visible.len() == 1 && dialog.entries.first().is_some_and(|e| e.name == "..")) {
        d.line(buf, 4, "(no matching files)", fill.add_modifier(Modifier::ITALIC));
    }
    let footer_row = d.inner.height.saturating_sub(2);
    let footer = match &dialog.error {
        Some(err) => fit_width(err, tw as usize),
        None if dialog.purpose == DialogPurpose::Folder => {
            "O or Enter confirms the selected folder".to_string()
        }
        None => "Type to filter, or a path starting with / or ~".to_string(),
    };
    let footer_style = if dialog.error.is_some() { Style::default().fg(Color::Red).bg(pal.surface) } else { fill.add_modifier(Modifier::ITALIC) };
    d.line(buf, footer_row, &footer, footer_style);
    let items = match dialog.purpose {
        DialogPurpose::Folder => [
            ("O Confirm", key(KeyCode::Char('o'))),
            ("Bksp Up", key(KeyCode::Backspace)),
            ("Esc Cancel", key(KeyCode::Esc)),
        ],
        _ => [
            ("Enter Open", key(KeyCode::Enter)),
            ("Bksp Up", key(KeyCode::Backspace)),
            ("Esc Cancel", key(KeyCode::Esc)),
        ],
    };
    d.buttons(buf, footer_row + 1, &items, pal, layout);
}

/// What a [`FilePicker`] key press or click did.
pub enum PickResult {
    Continue,
    Cancel,
    Picked(PathBuf),
}

/// Paint's DeskMate-style file dialog on its own, for choosing a drawing
/// outside Paint (e.g. a desktop shortcut's icon picture).
pub struct FilePicker {
    dialog: FileDialog,
    layout: LayoutCache,
}

impl FilePicker {
    /// Choose a drawing (`.ans`/`.txt`), starting in `dir`.
    pub fn icon(dir: PathBuf) -> Self {
        let mut dialog = FileDialog::new(DialogPurpose::Icon, dir);
        // Start on the first drawing rather than "..".
        if dialog.entries.first().is_some_and(|e| e.name == "..") && dialog.entries.len() > 1 {
            dialog.move_selection(1);
        }
        Self { dialog, layout: LayoutCache::default() }
    }

    /// Choose a folder, starting in `dir` (`o`/Enter confirms it).
    pub fn folder(dir: PathBuf) -> Self {
        let dialog = FileDialog::new(DialogPurpose::Folder, dir);
        Self { dialog, layout: LayoutCache::default() }
    }

    /// Choose a single data file whose extension is in `file_exts`
    /// (e.g. `[".vcf"]` for an address book, `[".json"]` for a calendar),
    /// starting in `dir`. Enter opens it.
    pub fn data_file(dir: PathBuf, file_exts: Vec<String>) -> Self {
        let mut dialog = FileDialog::new_with_exts(DialogPurpose::DataFile, dir, file_exts);
        // Start on the first file rather than ".." when one exists.
        if let Some(file_pos) = dialog.entries.iter().position(|e| !e.is_dir) {
            dialog.selected = file_pos;
        }
        Self { dialog, layout: LayoutCache::default() }
    }

    /// Draws the dialog centered over `area`.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let mut layout = LayoutCache::default();
        draw_file_dialog(&mut self.dialog, frame, area, pal, &mut layout);
        self.layout = layout;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickResult {
        Self::result(self.dialog.handle_key(key))
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PickResult {
        let (x, y) = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let button = self.layout.buttons.iter().rev().find(|(r, _)| hit(*r, x, y)).map(|(_, c)| *c);
                if let Some(Click::Key(key)) = button {
                    return self.handle_key(key);
                }
                let row = self.layout.file_rows.iter().find(|(r, _)| hit(*r, x, y)).map(|(_, i)| *i);
                match row {
                    Some(idx) => Self::result(self.dialog.click(idx)),
                    None => PickResult::Continue,
                }
            }
            MouseEventKind::ScrollUp => {
                self.dialog.move_selection(-3);
                PickResult::Continue
            }
            MouseEventKind::ScrollDown => {
                self.dialog.move_selection(3);
                PickResult::Continue
            }
            _ => PickResult::Continue,
        }
    }

    fn result(result: DialogResult) -> PickResult {
        match result {
            DialogResult::Continue => PickResult::Continue,
            DialogResult::Cancel => PickResult::Cancel,
            DialogResult::Open(path) => PickResult::Picked(path),
        }
    }
}
