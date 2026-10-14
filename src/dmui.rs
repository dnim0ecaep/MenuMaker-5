//! Shared DeskMate-style drawing: theme-colored double-line boxes with a
//! centered `◄ TITLE ►` band, the app's header/shortcut/status bars, and a
//! small labelled-field dialog used by the Address Book and Calendar.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// The theme colors the box drawing needs.
#[derive(Clone, Copy)]
pub struct Palette {
    pub primary: Color,
    pub accent: Color,
    pub highlight: Color,
    pub background: Color,
    pub surface: Color,
    pub text: Color,
    pub bar_bg: Color,
    pub bar_key: Color,
    pub bar_label: Color,
    pub selection_text: Color,
    /// Focused title band background, when the theme sets one.
    pub focus_band: Option<Color>,
}

impl Palette {
    pub fn fill(&self) -> Style {
        Style::default().bg(self.surface).fg(self.text)
    }

    pub fn selected(&self) -> Style {
        Style::default()
            .fg(self.selection_text)
            .bg(self.highlight)
            .add_modifier(Modifier::BOLD)
    }

    fn focused_band(&self) -> Style {
        match self.focus_band {
            Some(band) => Style::default()
                .fg(self.accent)
                .bg(band)
                .add_modifier(Modifier::BOLD),
            None => self.selected(),
        }
    }

    pub fn label(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .bg(self.surface)
            .add_modifier(Modifier::BOLD)
    }
}

/// Truncate `text` to at most `width` display columns.
pub fn fit_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out
}

/// Pad (or cut) `text` to exactly `width` display columns.
pub fn pad_width(text: &str, width: usize) -> String {
    let fitted = fit_width(text, width);
    let used = UnicodeWidthStr::width(fitted.as_str());
    format!("{fitted}{}", " ".repeat(width.saturating_sub(used)))
}

/// Draw a DeskMate box: double-line frame in the accent color, filled with
/// `fill`, and the title centered on the top border as `◄ TITLE ►`. The band
/// uses the selection colors when `focused`. Returns the inner area.
pub fn dm_box(frame: &mut Frame, area: Rect, title: &str, pal: &Palette, fill: Style, focused: bool) -> Rect {
    if area.width < 2 || area.height < 2 {
        return Rect::default();
    }
    let fill_bg = fill.bg.unwrap_or(pal.surface);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(pal.accent).bg(fill_bg))
            .style(fill),
        area,
    );
    if area.width > 6 && !title.is_empty() {
        let band = fit_width(&format!(" ◄ {title} ► "), area.width as usize - 4);
        let band_width = UnicodeWidthStr::width(band.as_str()) as u16;
        let band_style = if focused {
            pal.focused_band()
        } else {
            Style::default()
                .fg(fill_bg)
                .bg(pal.accent)
                .add_modifier(Modifier::BOLD)
        };
        frame.render_widget(
            Paragraph::new(band).style(band_style),
            Rect {
                x: area.x + (area.width - band_width) / 2,
                y: area.y,
                width: band_width,
                height: 1,
            },
        );
    }
    area.inner(&Margin {
        horizontal: 1,
        vertical: 1,
    })
}

/// Draw one line of text into row `row` of `area`, padded to its width.
pub fn put_line(frame: &mut Frame, area: Rect, row: u16, text: &str, style: Style) {
    if row >= area.height || area.width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(pad_width(text, area.width as usize)).style(style),
        Rect {
            x: area.x,
            y: area.y + row,
            width: area.width,
            height: 1,
        },
    );
}

/// Draw the app chrome used by every full screen: title bar, shortcut bar,
/// and status bar. Returns the desktop area between them, painted with the
/// theme background.
pub fn screen_chrome(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    shortcuts: &[(&str, &str)],
    status: &str,
    pal: &Palette,
) -> Rect {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let bar = Style::default()
        .bg(pal.primary)
        .fg(pal.text)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(title.to_string()).alignment(Alignment::Center).style(bar), chunks[0]);

    let key_style = Style::default()
        .fg(pal.bar_key)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(pal.bar_label);
    let mut spans = Vec::new();
    for (idx, (key, label)) in shortcuts.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" | ", label_style));
        }
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::styled(format!(" {label}"), label_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center)
            .style(Style::default().bg(pal.bar_bg)),
        chunks[1],
    );
    frame.render_widget(Block::default().style(Style::default().bg(pal.background)), chunks[2]);
    frame.render_widget(Paragraph::new(status.to_string()).alignment(Alignment::Center).style(bar), chunks[3]);
    chunks[2].inner(&Margin {
        horizontal: 1,
        vertical: 1,
    })
}

/// Where [`screen_chrome`] draws each shortcut chip on its centered
/// shortcut bar, as `(x_offset, width)` relative to the bar's left edge, so
/// callers can make the chips clickable. Returns `None` when the chips
/// don't all fit in `width` (the bar would then be cut off).
pub fn shortcut_spans(width: u16, shortcuts: &[(&str, &str)]) -> Option<Vec<(u16, u16)>> {
    let mut spans = Vec::with_capacity(shortcuts.len());
    let mut x = 0usize;
    for (idx, (key, label)) in shortcuts.iter().enumerate() {
        if idx > 0 {
            x += 3; // " | "
        }
        let w = UnicodeWidthStr::width(*key) + 1 + UnicodeWidthStr::width(*label);
        spans.push((x, w));
        x += w;
    }
    if x > width as usize {
        return None;
    }
    // Same centering as `Paragraph` with `Alignment::Center`.
    let offset = (width / 2).saturating_sub(x as u16 / 2);
    Some(spans.into_iter().map(|(sx, w)| (offset + sx as u16, w as u16)).collect())
}

/// One field of a [`FieldForm`].
pub struct FormField {
    pub label: String,
    pub value: String,
    /// When set, the field picks one of these with ←/→/Space instead of typing.
    pub choices: Vec<String>,
}

impl FormField {
    pub fn text(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.to_string(),
            choices: Vec::new(),
        }
    }

    pub fn choice(label: &str, value: &str, choices: &[&str]) -> Self {
        let choices: Vec<String> = choices.iter().map(|c| c.to_string()).collect();
        let value = choices
            .iter()
            .find(|c| c.eq_ignore_ascii_case(value))
            .cloned()
            .unwrap_or_else(|| choices.first().cloned().unwrap_or_default());
        Self {
            label: label.to_string(),
            value,
            choices,
        }
    }

    fn cycle(&mut self, forward: bool) {
        if self.choices.is_empty() {
            return;
        }
        let len = self.choices.len();
        let pos = self.choices.iter().position(|c| *c == self.value).unwrap_or(0);
        let next = if forward { (pos + 1) % len } else { (pos + len - 1) % len };
        self.value = self.choices[next].clone();
    }
}

pub enum FormResult {
    Continue,
    Submit,
    Cancel,
}

/// A centered dialog of labelled single-line fields.
pub struct FieldForm {
    pub title: String,
    pub fields: Vec<FormField>,
    pub selected: usize,
    pub error: Option<String>,
}

impl FieldForm {
    pub fn new(title: &str, fields: Vec<FormField>) -> Self {
        Self {
            title: title.to_string(),
            fields,
            selected: 0,
            error: None,
        }
    }

    pub fn value(&self, index: usize) -> &str {
        self.fields.get(index).map(|f| f.value.trim()).unwrap_or("")
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormResult {
        self.error = None;
        let count = self.fields.len().max(1);
        let field = &mut self.fields[self.selected];
        match key.code {
            KeyCode::Esc => return FormResult::Cancel,
            KeyCode::Enter => return FormResult::Submit,
            KeyCode::Tab | KeyCode::Down => self.selected = (self.selected + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.selected = (self.selected + count - 1) % count,
            KeyCode::Left if !field.choices.is_empty() => field.cycle(false),
            KeyCode::Right | KeyCode::Char(' ') if !field.choices.is_empty() => field.cycle(true),
            KeyCode::Backspace if field.choices.is_empty() => {
                field.value.pop();
            }
            KeyCode::Delete if field.choices.is_empty() => field.value.clear(),
            KeyCode::Char(c)
                if field.choices.is_empty() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                field.value.push(c)
            }
            _ => {}
        }
        FormResult::Continue
    }

    /// Where the dialog sits on `screen`: (outer box, inner area, label width).
    fn geometry(&self, screen: Rect) -> (Rect, Rect, u16) {
        let label_width = self
            .fields
            .iter()
            .map(|f| UnicodeWidthStr::width(f.label.as_str()))
            .max()
            .unwrap_or(0) as u16;
        let width = (label_width + 46).min(screen.width.saturating_sub(4)).max(20);
        let height = (self.fields.len() as u16 + 6).min(screen.height);
        let area = Rect {
            x: screen.x + screen.width.saturating_sub(width) / 2,
            y: screen.y + screen.height.saturating_sub(height) / 2,
            width,
            height,
        };
        let inner = area
            .inner(&Margin { horizontal: 1, vertical: 1 })
            .inner(&Margin { horizontal: 1, vertical: 0 });
        (area, inner, label_width)
    }

    /// The clickable [ Save ] and [ Cancel ] buttons (zero-sized if no room).
    fn buttons(&self, screen: Rect) -> (Rect, Rect) {
        let (_, inner, _) = self.geometry(screen);
        let row = self.fields.len() as u16 + 2;
        let (sw, cw) = (SAVE_BUTTON.len() as u16, CANCEL_BUTTON.len() as u16);
        if row + 1 >= inner.height || inner.width < sw + cw + 3 {
            return (Rect::default(), Rect::default());
        }
        let x = inner.x + (inner.width - (sw + cw + 3)) / 2;
        let y = inner.y + row;
        (
            Rect { x, y, width: sw, height: 1 },
            Rect { x: x + sw + 3, y, width: cw, height: 1 },
        )
    }

    /// Mouse on the dialog: click a field to select it (a choice field
    /// cycles when clicked again), click [ Save ] / [ Cancel ], scroll to
    /// move between fields.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, screen: Rect) -> FormResult {
        let (x, y) = (mouse.column, mouse.row);
        let count = self.fields.len().max(1);
        match mouse.kind {
            MouseEventKind::ScrollDown => self.selected = (self.selected + 1) % count,
            MouseEventKind::ScrollUp => self.selected = (self.selected + count - 1) % count,
            MouseEventKind::Down(MouseButton::Left) => {
                let (save, cancel) = self.buttons(screen);
                if contains(save, x, y) {
                    return FormResult::Submit;
                }
                if contains(cancel, x, y) {
                    return FormResult::Cancel;
                }
                let (_, inner, _) = self.geometry(screen);
                if contains(inner, x, y) && y > inner.y {
                    let idx = (y - inner.y - 1) as usize;
                    if idx < self.fields.len() {
                        self.error = None;
                        if idx == self.selected && !self.fields[idx].choices.is_empty() {
                            self.fields[idx].cycle(true);
                        }
                        self.selected = idx;
                    }
                }
            }
            _ => {}
        }
        FormResult::Continue
    }

    pub fn render(&self, frame: &mut Frame, screen: Rect, pal: &Palette) {
        let label_width = self
            .fields
            .iter()
            .map(|f| UnicodeWidthStr::width(f.label.as_str()))
            .max()
            .unwrap_or(0) as u16;
        let width = (label_width + 46).min(screen.width.saturating_sub(4)).max(20);
        let height = (self.fields.len() as u16 + 6).min(screen.height);
        let area = Rect {
            x: screen.x + screen.width.saturating_sub(width) / 2,
            y: screen.y + screen.height.saturating_sub(height) / 2,
            width,
            height,
        };
        let inner = dm_box(frame, area, &self.title, pal, pal.fill(), true);
        let inner = inner.inner(&Margin {
            horizontal: 1,
            vertical: 0,
        });
        let value_width = inner.width.saturating_sub(label_width + 2) as usize;
        for (idx, field) in self.fields.iter().enumerate() {
            let row = idx as u16 + 1;
            if row >= inner.height {
                break;
            }
            let selected = idx == self.selected;
            let shown = if field.choices.is_empty() {
                // Show the end of long values so the cursor stays visible.
                let chars: Vec<char> = field.value.chars().collect();
                let skip = chars.len().saturating_sub(value_width.saturating_sub(1));
                chars[skip..].iter().collect::<String>()
            } else {
                format!("◄ {} ►", field.value)
            };
            let label = format!("{:>w$}: ", field.label, w = label_width as usize);
            let value_style = if selected { pal.selected() } else { pal.fill() };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(label, pal.label()),
                    Span::styled(pad_width(&shown, value_width), value_style),
                ])),
                Rect {
                    x: inner.x,
                    y: inner.y + row,
                    width: inner.width,
                    height: 1,
                },
            );
            if selected && field.choices.is_empty() {
                let cursor = UnicodeWidthStr::width(shown.as_str()) as u16;
                frame.set_cursor(
                    inner.x + label_width + 2 + cursor.min(value_width as u16),
                    inner.y + row,
                );
            }
        }
        let (save, cancel) = self.buttons(screen);
        if save.width > 0 {
            frame.render_widget(Paragraph::new(SAVE_BUTTON).style(pal.selected()), save);
            frame.render_widget(Paragraph::new(CANCEL_BUTTON).style(pal.label().add_modifier(Modifier::REVERSED)), cancel);
        }
        let footer_row = inner.height.saturating_sub(1);
        let footer = match &self.error {
            Some(err) => Span::styled(err.clone(), Style::default().fg(Color::Red).bg(pal.surface)),
            None => Span::styled(
                "Tab/click move · ←/→ or click choose · Enter save · Esc cancel",
                pal.fill().add_modifier(Modifier::ITALIC),
            ),
        };
        frame.render_widget(
            Paragraph::new(Line::from(footer)).alignment(Alignment::Center),
            Rect {
                x: inner.x,
                y: inner.y + footer_row,
                width: inner.width,
                height: 1,
            },
        );
    }
}

/// A centered yes/no question in a DeskMate box.
pub fn render_confirm(frame: &mut Frame, screen: Rect, message: &str, pal: &Palette) {
    let width = (UnicodeWidthStr::width(message) as u16 + 6)
        .max(36)
        .min(screen.width.saturating_sub(4));
    let area = Rect {
        x: screen.x + screen.width.saturating_sub(width) / 2,
        y: screen.y + screen.height.saturating_sub(6) / 2,
        width,
        height: 6.min(screen.height),
    };
    let inner = dm_box(frame, area, "Confirm", pal, pal.fill(), true);
    put_line(frame, inner, 1, &format!(" {message}"), pal.fill());
    put_line(frame, inner, 3, "", pal.fill());
    let (yes, no) = confirm_buttons(screen, message);
    if yes.width > 0 {
        frame.render_widget(Paragraph::new(YES_BUTTON).style(pal.selected()), yes);
        frame.render_widget(Paragraph::new(NO_BUTTON).style(pal.label().add_modifier(Modifier::REVERSED)), no);
    }
}

fn confirm_area(screen: Rect, message: &str) -> Rect {
    let width = (UnicodeWidthStr::width(message) as u16 + 6)
        .max(36)
        .min(screen.width.saturating_sub(4));
    Rect {
        x: screen.x + screen.width.saturating_sub(width) / 2,
        y: screen.y + screen.height.saturating_sub(6) / 2,
        width,
        height: 6.min(screen.height),
    }
}

fn confirm_buttons(screen: Rect, message: &str) -> (Rect, Rect) {
    let area = confirm_area(screen, message);
    let (yw, nw) = (YES_BUTTON.chars().count() as u16, NO_BUTTON.chars().count() as u16);
    if area.height < 6 || area.width < yw + nw + 6 {
        return (Rect::default(), Rect::default());
    }
    let x = area.x + (area.width - (yw + nw + 4)) / 2;
    let y = area.y + 4;
    (Rect { x, y, width: yw, height: 1 }, Rect { x: x + yw + 4, y, width: nw, height: 1 })
}

/// Mouse on a [`render_confirm`] dialog: the key a click on Yes / No means.
pub fn confirm_click(screen: Rect, message: &str, mouse: MouseEvent) -> Option<KeyEvent> {
    if !is_left_click(mouse) {
        return None;
    }
    let (yes, no) = confirm_buttons(screen, message);
    if contains(yes, mouse.column, mouse.row) {
        Some(key(KeyCode::Char('y')))
    } else if contains(no, mouse.column, mouse.row) {
        Some(key(KeyCode::Char('n')))
    } else {
        None
    }
}

// ---------------------------------------------------------------- mouse

const SAVE_BUTTON: &str = "[ Save ]";
const CANCEL_BUTTON: &str = "[ Cancel ]";
const YES_BUTTON: &str = "[ Yes (y) ]";
const NO_BUTTON: &str = "[ No (n) ]";

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

pub fn is_left_click(mouse: MouseEvent) -> bool {
    matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Click targets recorded while drawing (render only gets `&self`).
/// The last target pushed wins, so draw popups after the screen.
pub struct Hits<T>(RefCell<Vec<(Rect, T)>>);

impl<T> Default for Hits<T> {
    fn default() -> Self {
        Self(RefCell::new(Vec::new()))
    }
}

impl<T: Clone> Hits<T> {
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    pub fn push(&self, rect: Rect, target: T) {
        self.0.borrow_mut().push((rect, target));
    }

    pub fn at(&self, x: u16, y: u16) -> Option<T> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find(|(r, _)| contains(*r, x, y))
            .map(|(_, t)| t.clone())
    }
}

/// Parse one key name as shown on a shortcut bar (`Esc`, `Enter`, `F9`,
/// `^s`, `PgUp`, `↑`, `a`, ...).
fn parse_key_name(name: &str) -> Option<KeyEvent> {
    let name = name.trim();
    let code = match name {
        "Esc" => KeyCode::Esc,
        "Enter" | "↵" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "Del" | "Delete" => KeyCode::Delete,
        "Space" => KeyCode::Char(' '),
        "PgUp" => KeyCode::PageUp,
        "PgDn" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "↑" => KeyCode::Up,
        "↓" => KeyCode::Down,
        "←" => KeyCode::Left,
        "→" => KeyCode::Right,
        _ => {
            if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
                KeyCode::F(n)
            } else if let Some(c) = name.strip_prefix('^').filter(|c| c.chars().count() == 1) {
                let ch = c.chars().next()?.to_ascii_lowercase();
                return Some(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
            } else if name.chars().count() == 1 {
                KeyCode::Char(name.chars().next()?)
            } else {
                return None;
            }
        }
    };
    Some(key(code))
}

/// The key a click on the [`screen_chrome`] shortcut bar of `area` means.
/// A chip listing several keys (`↑/↓`, `PgUp/PgDn`, `[ ]`) gives the one
/// clicked; a click on its label gives the first.
pub fn shortcut_click(area: Rect, shortcuts: &[(&str, &str)], mouse: MouseEvent) -> Option<KeyEvent> {
    if !is_left_click(mouse) || area.height < 2 || mouse.row != area.y + 1 {
        return None;
    }
    let spans = shortcut_spans(area.width, shortcuts)?;
    let x = mouse.column.checked_sub(area.x)?;
    let idx = spans.iter().position(|(sx, w)| x >= *sx && x < sx + w)?;
    let key_text = shortcuts[idx].0;
    let offset = (x - spans[idx].0) as usize;
    let parts: Vec<&str> = key_text.split(['/', ' ']).filter(|p| !p.is_empty()).collect();
    if parts.len() <= 1 {
        return parse_key_name(key_text);
    }
    // Which part of the key text holds the clicked column?
    let mut pos = 0usize;
    for part in &parts {
        let start = key_text[pos..].find(part).map(|i| i + pos).unwrap_or(pos);
        let end = start + part.len();
        let (s, e) = (
            UnicodeWidthStr::width(&key_text[..start]),
            UnicodeWidthStr::width(&key_text[..end]),
        );
        if offset < e {
            let _ = s;
            return parse_key_name(part);
        }
        pos = end;
    }
    parse_key_name(parts[0])
}

/// Turn a scroll-wheel event into the arrow key it stands for.
pub fn wheel_key(mouse: MouseEvent) -> Option<KeyEvent> {
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(key(KeyCode::Up)),
        MouseEventKind::ScrollDown => Some(key(KeyCode::Down)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn shortcut_chips_are_clickable() {
        let area = Rect { x: 0, y: 0, width: 40, height: 10 };
        let bar: &[(&str, &str)] = &[("Esc", "Back"), ("↑/↓", "Move"), ("^s", "Save")];
        let spans = shortcut_spans(40, bar).unwrap();
        let at = |i: usize, dx: u16| shortcut_click(area, bar, click(spans[i].0 + dx, 1));
        assert_eq!(at(0, 0), Some(key(KeyCode::Esc)));
        assert_eq!(at(1, 0), Some(key(KeyCode::Up)));
        assert_eq!(at(1, 2), Some(key(KeyCode::Down)));
        assert_eq!(at(2, 4), Some(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        assert_eq!(shortcut_click(area, bar, click(spans[0].0, 2)), None);
    }

    #[test]
    fn form_buttons_and_fields() {
        let screen = Rect { x: 0, y: 0, width: 100, height: 30 };
        let mut form = FieldForm::new("T", vec![FormField::text("A", ""), FormField::choice("B", "x", &["x", "y"])]);
        let (_, inner, _) = form.geometry(screen);
        assert!(matches!(form.handle_mouse(click(inner.x + 2, inner.y + 2), screen), FormResult::Continue));
        assert_eq!(form.selected, 1);
        form.handle_mouse(click(inner.x + 2, inner.y + 2), screen);
        assert_eq!(form.fields[1].value, "y");
        let (save, cancel) = form.buttons(screen);
        assert!(matches!(form.handle_mouse(click(save.x, save.y), screen), FormResult::Submit));
        assert!(matches!(form.handle_mouse(click(cancel.x + 1, cancel.y), screen), FormResult::Cancel));
        let (yes, no) = confirm_buttons(screen, "Delete?");
        assert_eq!(confirm_click(screen, "Delete?", click(yes.x, yes.y)), Some(key(KeyCode::Char('y'))));
        assert_eq!(confirm_click(screen, "Delete?", click(no.x, no.y)), Some(key(KeyCode::Char('n'))));
    }
}
