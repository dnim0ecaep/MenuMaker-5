//! Dialog rendering (miditui's `dialogs.rs`, redrawn as DeskMate boxes):
//! Save Project, Open / Load SoundFont browser, New Project confirmation,
//! the unsaved-changes prompt, About, and the scrollable help overlay.
//! Every button is recorded in the host layout, so a click acts like its key.

use super::help::help_lines;
use super::{key, put, text_w, HostLayout};
use crate::dmui::{dm_box, fit_width, pad_width, Palette};
use crate::midi::app::SaveFormat;
use crate::midi::filedialog::{DialogPurpose, FileDialog};
use crate::midi::MidiApp;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

fn centered(screen: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(screen.width);
    let h = h.min(screen.height);
    Rect { x: screen.x + (screen.width - w) / 2, y: screen.y + (screen.height - h) / 2, width: w, height: h }
}

fn button_style(pal: &Palette) -> Style {
    Style::default().fg(pal.surface).bg(pal.accent).add_modifier(Modifier::BOLD)
}

/// Keeps the end of `text` (e.g. a long folder path) within `width`.
fn tail_fit(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut used = 1; // the leading ellipsis
    for ch in text.chars().rev() {
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
    fn open(frame: &mut Frame, screen: Rect, title: &str, w: u16, h: u16, pal: &Palette, layout: &mut HostLayout) -> Self {
        let rect = centered(screen, w, h);
        layout.buttons.retain(|(r, _)| r.intersection(rect).area() == 0);
        layout.dialog = Some(rect);
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
    fn buttons(&self, buf: &mut Buffer, row: u16, items: &[(&str, KeyEvent)], pal: &Palette, layout: &mut HostLayout) {
        let Some(y) = self.row_y(row) else { return };
        let total: u16 = items.iter().map(|(l, _)| text_w(l) + 2).sum::<u16>() + 2 * items.len().saturating_sub(1) as u16;
        let mut x = self.inner.x + self.inner.width.saturating_sub(total) / 2;
        for (label, event) in items {
            let w = text_w(label) + 2;
            if x >= self.inner.right() {
                break;
            }
            let shown = w.min(self.inner.right() - x);
            put(buf, x, y, &format!(" {label} "), button_style(pal), shown);
            layout.buttons.push((Rect { x, y, width: shown, height: 1 }, *event));
            x = x.saturating_add(w + 2);
        }
    }

    /// `Label: [value]` text field with the terminal cursor at its end.
    fn field(&self, frame: &mut Frame, row: u16, label: &str, value: &str, suffix: &str, pal: &Palette) {
        let Some(y) = self.row_y(row) else { return };
        let x = self.inner.x + 1;
        let tw = self.text_width();
        let label_w = text_w(label);
        let buf = frame.buffer_mut();
        put(buf, x, y, label, pal.label(), tw);
        let field_w = tw.saturating_sub(label_w + text_w(suffix));
        let shown = tail_fit(value, field_w.saturating_sub(1) as usize);
        put(buf, x + label_w, y, &pad_width(&shown, field_w as usize), pal.selected(), field_w);
        put(buf, x + label_w + field_w, y, suffix, pal.fill(), tw.saturating_sub(label_w + field_w));
        if field_w > 0 {
            frame.set_cursor((x + label_w + text_w(&shown)).min(x + label_w + field_w - 1), y);
        }
    }
}

impl MidiApp {
    pub(in crate::midi) fn render_dialogs(&mut self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut HostLayout) {
        if area.width < 4 || area.height < 3 {
            return;
        }
        let fill = pal.fill();
        let italic = fill.add_modifier(Modifier::ITALIC);
        if let Some((title, lines)) = &self.info {
            let w = lines.iter().map(|l| text_w(l)).max().unwrap_or(0) + 4;
            let h = lines.len() as u16 + 4;
            let d = Dialog::open(frame, area, title, w.max(24), h, pal, layout);
            let buf = frame.buffer_mut();
            for (i, line) in lines.iter().enumerate() {
                d.line(buf, i as u16, line, fill);
            }
            d.buttons(buf, lines.len() as u16 + 1, &[("OK", key(KeyCode::Enter))], pal, layout);
        } else if self.show_help {
            self.render_help(frame, area, pal, layout);
        } else if self.app.file_dialog.is_some() {
            let first_load = self.app.soundfont_first_load;
            if let Some(dialog) = &mut self.app.file_dialog {
                draw_file_dialog(dialog, first_load, frame, area, pal, layout);
            }
        } else if self.unsaved.is_some() {
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
        } else if self.app.save_dialog.open {
            if let Some(path) = &self.app.save_dialog.overwrite {
                let msg = format!("{} already exists.", crate::midi::file_name(path));
                let d = Dialog::open(frame, area, "Confirm", (text_w(&msg) + 6).max(36), 7, pal, layout);
                let buf = frame.buffer_mut();
                d.line(buf, 1, &msg, fill);
                d.line(buf, 2, "Replace it?", fill);
                d.buttons(buf, 4, &[("Y Yes", key(KeyCode::Char('y'))), ("N No", key(KeyCode::Char('n')))], pal, layout);
                return;
            }
            let d = Dialog::open(frame, area, "Save Project", 60, 11, pal, layout);
            let tw = d.text_width() as usize;
            let folder = self.app.save_dir().display().to_string();
            d.line(frame.buffer_mut(), 0, &format!("Folder: {}", tail_fit(&folder, tw.saturating_sub(8))), fill);
            let ext = format!(".{}", self.app.save_dialog.format.extension());
            d.field(frame, 2, "Filename: ", &self.app.save_dialog.filename, &ext, pal);
            let buf = frame.buffer_mut();
            if let Some(y) = d.row_y(4) {
                let x = d.inner.x + 1;
                put(buf, x, y, "Format:   ", pal.label(), d.text_width());
                let mut fx = x + 10;
                for (label, format) in [("JSON", SaveFormat::Json), ("OXM", SaveFormat::Oxm), ("MIDI", SaveFormat::Midi)] {
                    let on = self.app.save_dialog.format == format;
                    let text = format!("[{}] {label}", if on { 'X' } else { ' ' });
                    let style = if on { pal.selected() } else { fill };
                    put(buf, fx, y, &text, style, d.inner.right().saturating_sub(fx));
                    fx = fx.saturating_add(text_w(&text) + 2);
                }
            }
            d.line(buf, 6, "Tab toggles the format; MIDI drops mute/solo.", italic);
            let items = [
                ("Enter Save", key(KeyCode::Enter)),
                ("Tab Format", key(KeyCode::Tab)),
                ("Esc Cancel", key(KeyCode::Esc)),
            ];
            d.buttons(buf, 8, &items, pal, layout);
        } else if self.app.new_project_dialog.open {
            let d = Dialog::open(frame, area, "New Project", 44, 8, pal, layout);
            let buf = frame.buffer_mut();
            d.line(buf, 1, "Create a new project?", fill.add_modifier(Modifier::BOLD));
            d.line(buf, 2, "Unsaved changes will be lost.", italic);
            // Yes / No with the current choice highlighted (miditui's buttons).
            if let Some(y) = d.row_y(4) {
                let total = 5 + 5 + 4;
                let x = d.inner.x + d.inner.width.saturating_sub(total) / 2;
                let sel = self.app.new_project_dialog.selected;
                let style = |on: bool| if on { pal.selected() } else { button_style(pal) };
                put(buf, x, y, " Yes ", style(sel == 0), d.inner.right().saturating_sub(x));
                layout.buttons.push((Rect { x, y, width: 5, height: 1 }, key(KeyCode::Char('y'))));
                let nx = x + 10;
                put(buf, nx, y, " No ", style(sel == 1), d.inner.right().saturating_sub(nx));
                layout.buttons.push((Rect { x: nx, y, width: 4, height: 1 }, key(KeyCode::Char('n'))));
            }
            d.line(buf, 5, "←/→ select · Enter confirm · Esc cancel", italic);
        }
    }

    /// The scrollable help overlay (any click outside the scroll area closes it).
    fn render_help(&mut self, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut HostLayout) {
        let w = (area.width * 7 / 10).max(60).min(area.width);
        let h = (area.height * 8 / 10).max(10).min(area.height);
        let d = Dialog::open(frame, area, "Help - Keyboard Shortcuts", w, h, pal, layout);
        let content_h = d.inner.height.saturating_sub(1);
        let lines = help_lines(pal);
        let max_scroll = (lines.len() as u16).saturating_sub(content_h);
        self.app.help_scroll = self.app.help_scroll.min(max_scroll);
        if content_h > 0 && d.inner.width > 2 {
            let text_area = Rect { x: d.inner.x + 1, y: d.inner.y, width: d.inner.width - 2, height: content_h };
            frame.render_widget(
                Paragraph::new(lines).style(pal.fill()).scroll((self.app.help_scroll, 0)),
                text_area,
            );
        }
        let footer = "Scroll: Up/Down/j/k/Mouse  |  Close: ?/Esc/Click";
        d.line(frame.buffer_mut(), d.inner.height.saturating_sub(1), footer, pal.fill().add_modifier(Modifier::ITALIC));
    }
}

/// Draws the Open / Load SoundFont dialog and records its hit areas.
fn draw_file_dialog(dialog: &mut FileDialog, first_load: bool, frame: &mut Frame, area: Rect, pal: &Palette, layout: &mut HostLayout) {
    let w = area.width.saturating_sub(4).clamp(20, 72);
    let h = area.height.saturating_sub(2).clamp(8, 24);
    let title = if first_load { "Select a SoundFont" } else { dialog.purpose.title() };
    let d = Dialog::open(frame, area, title, w, h, pal, layout);
    let tw = d.text_width();
    let fill = pal.fill();
    let folder = dialog.dir.display().to_string();
    let buf = frame.buffer_mut();
    d.line(buf, 0, &format!("Folder: {}", tail_fit(&folder, (tw as usize).saturating_sub(8))), pal.label());
    let looks_like_path = dialog.input.contains('/') || dialog.input.starts_with('~');
    let prompt = if looks_like_path { "Path: " } else { "Find: " };
    d.field(frame, 1, prompt, &dialog.input.clone(), "", pal);
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
        let empty = if dialog.purpose == DialogPurpose::SoundFont { "(no SoundFont files here)" } else { "(no matching files)" };
        d.line(buf, 4, empty, fill.add_modifier(Modifier::ITALIC));
    }
    let footer_row = d.inner.height.saturating_sub(2);
    let footer = match &dialog.error {
        Some(err) => fit_width(err, tw as usize),
        None if first_load => "A SoundFont (.sf2) is needed for sound; Esc = no sound".to_string(),
        None => "Type to filter, or a path starting with / or ~".to_string(),
    };
    let footer_style = if dialog.error.is_some() { Style::default().fg(Color::Red).bg(pal.surface) } else { fill.add_modifier(Modifier::ITALIC) };
    d.line(buf, footer_row, &footer, footer_style);
    let items = [
        ("Enter Open", key(KeyCode::Enter)),
        ("Bksp Up", key(KeyCode::Backspace)),
        ("Esc Cancel", key(KeyCode::Esc)),
    ];
    d.buttons(buf, footer_row + 1, &items, pal, layout);
}
