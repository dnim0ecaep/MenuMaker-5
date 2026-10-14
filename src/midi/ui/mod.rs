//! Terminal user interface components (miditui's `ui` module), drawn
//! DeskMate-style: every panel is a theme-colored double box with a centered
//! `◄ TITLE ►` band (focused panel = highlighted band), and the whole screen
//! sits inside `dmui::screen_chrome` with a function-key menu bar.
//!
//! `MidiApp::render` records every clickable rectangle in [`HostLayout`]
//! (absolute screen coordinates) and the app's panel rectangles in
//! `App::layout` (miditui's `LayoutRegions`), and the mouse handling
//! hit-tests against exactly those.

mod combined;
mod dialogs;
mod help;
mod keyboard;
mod piano_roll;
mod project_timeline;
mod timeline;
mod tracks;

use super::app::{App, FocusedPanel, LayoutRegions, ViewMode, PIANO_KEY_WIDTH};
use super::model::{contains_beat, contains_measure, TICKS_PER_BEAT};
use super::{MidiApp, MENU_BAR};
use crate::dmui::{self, dm_box, screen_chrome, Palette};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

pub use combined::render_combined;
pub use keyboard::render_keyboard;
pub use piano_roll::render_piano_roll;
pub use project_timeline::{render_project_timeline, render_project_timeline_compact};
pub use timeline::render_timeline;
pub use tracks::render_track_list;

/// Hit-test rectangles for the host chrome from the last render.
#[derive(Default)]
pub struct HostLayout {
    /// Shortcut chips and dialog buttons (later entries are on top); a
    /// click behaves like the key.
    pub buttons: Vec<(Rect, KeyEvent)>,
    pub menu_bar: Vec<(Rect, usize)>,
    pub dropdown: Vec<(Rect, usize)>,
    /// File dialog rows -> index into `FileDialog::visible()`.
    pub file_rows: Vec<(Rect, usize)>,
    /// The area of the topmost dialog, if one is open.
    pub dialog: Option<Rect>,
}

pub fn hit(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

pub fn text_w(s: &str) -> u16 {
    UnicodeWidthStr::width(s).min(u16::MAX as usize) as u16
}

/// Writes `text` at `(x, y)`, clipped to `max_w` columns and the buffer.
pub fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, max_w: u16) {
    let a = buf.area;
    if y < a.y || y >= a.bottom() || x < a.x || x >= a.right() || max_w == 0 {
        return;
    }
    let w = max_w.min(a.right() - x);
    buf.set_stringn(x, y, text, w as usize, style);
}

/// Replaces one cell if it lies inside both `clip` and the buffer.
pub fn put_cell(buf: &mut Buffer, clip: Rect, x: u16, y: u16, ch: char, style: Style) {
    if !hit(clip, x, y) || !hit(buf.area, x, y) {
        return;
    }
    let cell = buf.get_mut(x, y);
    cell.reset();
    cell.set_char(ch);
    cell.set_style(style);
}

/// A DeskMate panel; returns the inner area.
pub fn panel(frame: &mut Frame, area: Rect, title: &str, pal: &Palette, focused: bool) -> Rect {
    dm_box(frame, area, title, pal, pal.fill(), focused)
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Renders a time ruler showing measure and beat markers.
///
/// Shared between Piano Roll and Project Timeline views.
pub fn render_time_ruler(buf: &mut Buffer, area: Rect, scroll_x: u32, zoom: u32, pal: &Palette) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let base = pal.fill();
    put(buf, area.x, area.y, &" ".repeat(area.width as usize), base, area.width);
    let measure_style = Style::default().fg(pal.accent).bg(pal.surface).add_modifier(Modifier::BOLD);
    let mut col = 0u16;
    while col < area.width {
        let tick = scroll_x + (col as u32 * zoom);
        let is_measure = contains_measure(tick, zoom);
        let is_beat = contains_beat(tick, zoom);
        let x = area.x + col;

        if is_measure {
            let measure_ticks = TICKS_PER_BEAT * 4;
            let measure_tick = if tick.is_multiple_of(measure_ticks) {
                tick
            } else {
                ((tick / measure_ticks) + 1) * measure_ticks
            };
            let measure_str = format!("{}", measure_tick / measure_ticks + 1);
            let chars_remaining = (area.width - col) as usize;
            if measure_str.len() <= chars_remaining {
                put(buf, x, area.y, &measure_str, measure_style, measure_str.len() as u16);
                col += measure_str.len() as u16;
                continue;
            }
            put(buf, x, area.y, "|", measure_style, 1);
        } else if is_beat {
            put(buf, x, area.y, ".", base, 1);
        }
        col += 1;
    }
}

/// Body height below which the keyboard panel is left out.
const KEYBOARD_MIN_BODY: u16 = 22;

/// Content height below which the Combined view shows only the Piano Roll
/// (both halves would be too small to edit in).
const COMBINED_MIN_CONTENT: u16 = 16;

/// The view actually drawn for `view_mode` in a body of `size`, and whether
/// the keyboard panel fits.
fn effective_view(size: Rect, view_mode: ViewMode) -> (ViewMode, bool) {
    let keyboard = size.height >= KEYBOARD_MIN_BODY;
    let content = size.height.saturating_sub(3 + if keyboard { 5 } else { 0 });
    let view = if view_mode == ViewMode::Combined && content < COMBINED_MIN_CONTENT {
        ViewMode::PianoRoll
    } else {
        view_mode
    };
    (view, keyboard)
}

/// Calculates the layout regions for the given body area and view mode.
///
/// This is called during rendering to update the layout regions used
/// for mouse hit testing and auto-scroll calculations. On short screens the
/// keyboard panel is dropped and Combined falls back to the Piano Roll
/// (see [`effective_view`]); `view_mode` must already be the effective one.
fn calculate_layout(size: Rect, view_mode: ViewMode, keyboard: bool) -> (LayoutRegions, [Rect; 3], [Rect; 2]) {
    // Main vertical layout: timeline, content, keyboard
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                             // Timeline/transport
            Constraint::Min(if keyboard { 10 } else { 0 }),    // Content area
            Constraint::Length(if keyboard { 5 } else { 0 }), // Keyboard
        ])
        .split(size);

    // Content area: track list on left, piano roll on right
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30), // Track list
            Constraint::Min(40),    // Piano roll
        ])
        .split(main_chunks[1]);

    // Calculate grid area based on view mode
    // - PianoRoll: 5 columns for piano keys
    // - ProjectTimeline: 12 columns for track labels
    // - Combined: use piano roll width (it's in the top half, 55% of content area)
    let piano_roll = content_chunks[1];
    let left_content_width = match view_mode {
        ViewMode::PianoRoll | ViewMode::Combined => PIANO_KEY_WIDTH,
        ViewMode::ProjectTimeline => 12, // DEFAULT_LABEL_WIDTH from project_timeline
    };

    // For Combined view, the piano roll only takes 55% of the content area height
    let actual_piano_roll_height = match view_mode {
        ViewMode::Combined => (piano_roll.height as u32 * 55 / 100) as u16,
        _ => piano_roll.height,
    };

    let piano_roll_grid = Rect {
        x: piano_roll.x + 1 + left_content_width,
        y: piano_roll.y + 1,
        width: piano_roll.width.saturating_sub(2 + left_content_width),
        height: actual_piano_roll_height.saturating_sub(2),
    };

    // Visible pitches: grid height minus the time ruler row, capped at 127.
    let visible_pitches = piano_roll_grid.height.saturating_sub(1).min(127) as u8;

    let layout = LayoutRegions {
        timeline: main_chunks[0],
        track_list: content_chunks[0],
        piano_roll,
        piano_roll_grid,
        keyboard: main_chunks[2],
        // Ruler regions are set during rendering
        piano_roll_ruler: Rect::default(),
        project_timeline_ruler: Rect::default(),
        visible_pitches,
    };

    (layout, [main_chunks[0], main_chunks[1], main_chunks[2]], [content_chunks[0], content_chunks[1]])
}

/// Renders the miditui panels into `body` and updates layout regions.
///
/// - Top: Timeline with transport controls and position display
/// - Left: Track list with mute/solo controls
/// - Center: Piano roll editor OR project timeline (based on view mode)
/// - Bottom: Piano keyboard for live input
fn render_panels(frame: &mut Frame, body: Rect, app: &mut App, pal: &Palette, active: bool) {
    let (view_mode, keyboard) = effective_view(body, app.view_mode);
    let (layout, main_chunks, content_chunks) = calculate_layout(body, view_mode, keyboard);
    app.layout = layout;
    let focus = |p: FocusedPanel| active && app.focused_panel == p;

    render_timeline(frame, main_chunks[0], app, focus(FocusedPanel::Timeline), pal);
    render_track_list(frame, content_chunks[0], app, focus(FocusedPanel::TrackList), pal);

    let is_focused = focus(FocusedPanel::PianoRoll);
    let (piano_roll_ruler, project_timeline_ruler) = match view_mode {
        ViewMode::Combined => render_combined(frame, content_chunks[1], app, is_focused, pal),
        ViewMode::PianoRoll => (render_piano_roll(frame, content_chunks[1], app, is_focused, pal), None),
        ViewMode::ProjectTimeline => (None, render_project_timeline(frame, content_chunks[1], app, is_focused, pal)),
    };
    app.layout.piano_roll_ruler = piano_roll_ruler.unwrap_or_default();
    app.layout.project_timeline_ruler = project_timeline_ruler.unwrap_or_default();

    if keyboard {
        render_keyboard(frame, main_chunks[2], app, focus(FocusedPanel::Keyboard), pal);
    }
}

impl MidiApp {
    /// Full screen: `dmui::screen_chrome` (title, clickable shortcut chips,
    /// status bar), the DeskMate menu bar, the miditui panels, then any
    /// dropdown / dialog / help overlay on top.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, title: &str, pal: &Palette) {
        let mut layout = HostLayout::default();

        // Only as many chips as fit, so every visible chip is clickable.
        let chips = self.chips();
        let mut count = chips.len();
        let spans = loop {
            let pairs: Vec<(&str, &str)> = chips[..count].iter().map(|c| (c.0, c.1)).collect();
            if let Some(spans) = dmui::shortcut_spans(area.width, &pairs) {
                break spans;
            }
            count -= 1;
        };
        let pairs: Vec<(&str, &str)> = chips[..count].iter().map(|c| (c.0, c.1)).collect();
        let heading = format!("{title} - Music - {}", self.file_label());
        let status = self.status_text();
        let desk = screen_chrome(frame, area, &heading, &pairs, &status, pal);
        if area.height >= 4 {
            for (chip, (x, w)) in chips.iter().zip(spans) {
                layout.buttons.push((Rect { x: area.x + x, y: area.y + 1, width: w, height: 1 }, chip.2));
            }
        }

        if desk.width >= 4 && desk.height >= 2 {
            self.render_menu_bar(frame.buffer_mut(), Rect { height: 1, ..desk }, pal, &mut layout);
            let body = Rect { y: desk.y + 1, height: desk.height - 1, ..desk };
            let active = !self.is_modal() && self.menu_open.is_none();
            render_panels(frame, body, &mut self.app, pal, active);
        } else {
            self.app.layout = LayoutRegions::default();
        }

        self.render_dropdown(frame, area, pal, &mut layout);
        // Dialogs sit between the shortcut bar and the status bar when
        // there's room, so the chips stay visible.
        let dialog_area = if area.height > 12 { Rect { y: area.y + 2, height: area.height - 3, ..area } } else { area };
        self.render_dialogs(frame, dialog_area, pal, &mut layout);
        self.layout = layout;
    }

    /// The DeskMate command bar: `File F2  Edit F3 …`.
    fn render_menu_bar(&self, buf: &mut Buffer, row: Rect, pal: &Palette, layout: &mut HostLayout) {
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

    fn render_dropdown(&self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut HostLayout) {
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
            let text = format!(" {}   {} ", dmui::pad_width(item.label, label_w as usize), hint);
            let style = if self.menu_selected == i { pal.selected() } else { pal.fill() };
            put(buf, inner.x, y, &dmui::pad_width(&text, inner.width as usize), style, inner.width);
            layout.dropdown.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, i));
        }
    }
}
