//! Enlarged glyph icons for Shortcut boxes: a Nerd Font icon (or any
//! character) is rasterized from the installed font's outline and drawn with
//! half-block characters, two "pixels" per cell, filling the box.
//!
//! A glyph icon is stored in a box's `icon` field as `glyph:<hex>` or
//! `glyph:<hex>:#RRGGBB` (e.g. `glyph:f07b:#BDBD00`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::dmui::{dm_box, fit_width, put_line, Palette};

const PREFIX: &str = "glyph:";

/// Named icons offered by the picker (Nerd Font code points). Entries the
/// installed font lacks are hidden.
pub const GALLERY: &[(&str, char)] = &[
    ("folder", '\u{f07b}'),
    ("folder open", '\u{f07c}'),
    ("file", '\u{f15b}'),
    ("file text", '\u{f15c}'),
    ("terminal", '\u{f120}'),
    ("code", '\u{f121}'),
    ("gear", '\u{f013}'),
    ("gears", '\u{f085}'),
    ("wrench", '\u{f0ad}'),
    ("home", '\u{f015}'),
    ("desktop", '\u{f108}'),
    ("laptop", '\u{f109}'),
    ("keyboard", '\u{f11c}'),
    ("microchip", '\u{f2db}'),
    ("server", '\u{f233}'),
    ("database", '\u{f1c0}'),
    ("network", '\u{f0e8}'),
    ("cloud", '\u{f0c2}'),
    ("download", '\u{f019}'),
    ("upload", '\u{f093}'),
    ("globe", '\u{f0ac}'),
    ("wifi", '\u{f1eb}'),
    ("envelope", '\u{f0e0}'),
    ("comments", '\u{f086}'),
    ("phone", '\u{f095}'),
    ("bell", '\u{f0f3}'),
    ("calendar", '\u{f073}'),
    ("clock", '\u{f017}'),
    ("calculator", '\u{f1ec}'),
    ("music", '\u{f001}'),
    ("headphones", '\u{f025}'),
    ("film", '\u{f008}'),
    ("image", '\u{f03e}'),
    ("camera", '\u{f030}'),
    ("paint brush", '\u{f1fc}'),
    ("pencil", '\u{f040}'),
    ("book", '\u{f02d}'),
    ("bookmark", '\u{f02e}'),
    ("archive", '\u{f187}'),
    ("print", '\u{f02f}'),
    ("search", '\u{f002}'),
    ("chart", '\u{f080}'),
    ("shopping cart", '\u{f07a}'),
    ("gamepad", '\u{f11b}'),
    ("rocket", '\u{f135}'),
    ("bug", '\u{f188}'),
    ("star", '\u{f005}'),
    ("heart", '\u{f004}'),
    ("user", '\u{f007}'),
    ("users", '\u{f0c0}'),
    ("lock", '\u{f023}'),
    ("key", '\u{f084}'),
    ("power", '\u{f011}'),
    ("trash", '\u{f1f8}'),
    ("bolt", '\u{f0e7}'),
    ("fire", '\u{f06d}'),
    ("coffee", '\u{f0f4}'),
    ("info", '\u{f05a}'),
    ("warning", '\u{f071}'),
    ("check", '\u{f00c}'),
    ("git", '\u{f1d3}'),
    ("github", '\u{f09b}'),
    ("linux", '\u{f17c}'),
    ("tux", '\u{f31a}'),
    ("debian", '\u{f306}'),
    ("ubuntu", '\u{f31b}'),
    ("arch linux", '\u{f303}'),
    ("apple", '\u{f179}'),
    ("windows", '\u{f17a}'),
    ("docker", '\u{e7b0}'),
    ("python", '\u{e73c}'),
    ("rust", '\u{e7a8}'),
    ("javascript", '\u{e74e}'),
    ("firefox", '\u{f269}'),
    ("chrome", '\u{f268}'),
];

/// Colors the picker cycles through (`None` = the box's border color).
const COLORS: &[(&str, Option<&str>)] = &[
    ("Border color", None),
    ("Yellow", Some("#BDBD00")),
    ("White", Some("#FFFFFF")),
    ("Light gray", Some("#BDBDBD")),
    ("Red", Some("#FF4040")),
    ("Maroon", Some("#7B0000")),
    ("Green", Some("#40C040")),
    ("Cyan", Some("#40C0C0")),
    ("Blue", Some("#4060FF")),
    ("Magenta", Some("#C040C0")),
    ("Orange", Some("#FF9020")),
    ("Black", Some("#000000")),
];

/// A glyph icon: the character and an optional fixed color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphIcon {
    pub ch: char,
    pub color: Option<Color>,
}

impl GlyphIcon {
    /// Parse `glyph:<hex>[:#RRGGBB]`.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.trim().strip_prefix(PREFIX)?;
        let mut parts = rest.split(':');
        let ch = parse_code(parts.next()?)?;
        let color = parts.next().and_then(hex_color);
        Some(Self { ch, color })
    }

    pub fn to_value(self) -> String {
        match self.color {
            Some(Color::Rgb(r, g, b)) => format!("{PREFIX}{:x}:#{r:02X}{g:02X}{b:02X}", self.ch as u32),
            _ => format!("{PREFIX}{:x}", self.ch as u32),
        }
    }

    /// A readable name for the box editor.
    pub fn describe(self) -> String {
        let name = GALLERY
            .iter()
            .find(|(_, c)| *c == self.ch)
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| format!("U+{:04X}", self.ch as u32));
        format!("glyph {} ({name})", self.ch)
    }
}

/// `f07b`, `U+F07B`, `0xf07b`, or a single literal character.
fn parse_code(text: &str) -> Option<char> {
    let text = text.trim();
    // A single character is itself; hex codes are at least two digits.
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(c);
    }
    let hex = text
        .trim_start_matches("U+")
        .trim_start_matches("u+")
        .trim_start_matches("0x");
    if hex.is_empty() || hex.len() > 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
}

fn hex_color(text: &str) -> Option<Color> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

// ---------------------------------------------------------------------------
// Font loading and rasterizing
// ---------------------------------------------------------------------------

/// Font files to try, best first: `$MENUMAKER_ICON_FONT`, then a Nerd Font
/// (Mono, Regular preferred), then DejaVu Sans as a plain fallback.
fn font_candidates() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/local/share/fonts"),
        PathBuf::from("/usr/share/fonts"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
        dirs.push(home.join("Library/Fonts"));
    }
    dirs.push(PathBuf::from("/Library/Fonts"));

    let mut files = Vec::new();
    for dir in &dirs {
        collect_fonts(dir, &mut files, 4);
    }
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let rank = |p: &PathBuf| -> u8 {
        let n = name(p);
        let nerd = n.contains("nerdfont") || n.contains("nerd font") || n.contains("symbolsnerd");
        match (nerd, n.contains("mono-regular"), n.contains("-regular")) {
            (true, true, _) => 0,
            (true, _, true) => 1,
            (true, _, _) => 2,
            _ if n == "dejavusans.ttf" => 3,
            _ => 9,
        }
    };
    files.retain(|p| rank(p) < 9);
    files.sort_by_key(rank);
    if let Ok(custom) = std::env::var("MENUMAKER_ICON_FONT") {
        files.insert(0, PathBuf::from(custom));
    }
    files
}

fn collect_fonts(dir: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            collect_fonts(&path, out, depth - 1);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
        {
            out.push(path);
        }
    }
}

/// The icon font, loaded once.
fn font() -> Option<&'static fontdue::Font> {
    static FONT: OnceLock<Option<fontdue::Font>> = OnceLock::new();
    FONT.get_or_init(|| {
        font_candidates().into_iter().find_map(|path| {
            let bytes = std::fs::read(&path).ok()?;
            fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()
        })
    })
    .as_ref()
}

pub fn font_available() -> bool {
    font().is_some()
}

/// Whether the icon font has a real glyph for `ch`.
pub fn has_glyph(ch: char) -> bool {
    font().is_some_and(|f| f.lookup_glyph_index(ch) != 0)
}

/// A rasterized glyph: `width` x `height` square pixels, row-major.
struct Bitmap {
    width: usize,
    height: usize,
    pixels: Vec<bool>,
}

/// Rasterize `ch` to fit within `max_w` x `max_h` pixels, keeping its shape.
fn rasterize(ch: char, max_w: usize, max_h: usize) -> Option<Bitmap> {
    let font = font()?;
    if max_w == 0 || max_h == 0 {
        return None;
    }
    let probe = font.metrics(ch, 100.0);
    if probe.width == 0 || probe.height == 0 {
        return None;
    }
    let scale = (max_w as f32 / probe.width as f32).min(max_h as f32 / probe.height as f32);
    let mut px = 100.0 * scale;
    // Rounding can overshoot by a pixel; shrink until it fits.
    for _ in 0..8 {
        let (metrics, coverage) = font.rasterize(ch, px);
        if metrics.width <= max_w && metrics.height <= max_h {
            if metrics.width == 0 || metrics.height == 0 {
                return None;
            }
            return Some(Bitmap {
                width: metrics.width,
                height: metrics.height,
                pixels: coverage.iter().map(|c| *c >= 110).collect(),
            });
        }
        px *= 0.94;
    }
    None
}

/// Draw `icon` as large as fits in `area`, centered, with half-block
/// characters. Only characters and the foreground color are written, so the
/// box's own background shows through.
pub fn draw(icon: GlyphIcon, default_color: Color, buf: &mut Buffer, area: Rect) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(bitmap) = rasterize(icon.ch, area.width as usize, area.height as usize * 2) else {
        return;
    };
    let rows = bitmap.height.div_ceil(2);
    let left = area.x + (area.width - bitmap.width as u16) / 2;
    let top = area.y + (area.height - rows as u16) / 2;
    let on = |x: usize, y: usize| y < bitmap.height && bitmap.pixels[y * bitmap.width + x];
    let style = Style::default().fg(icon.color.unwrap_or(default_color));
    for row in 0..rows {
        for x in 0..bitmap.width {
            let ch = match (on(x, row * 2), on(x, row * 2 + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => continue,
            };
            let cell = buf.get_mut(left + x as u16, top + row as u16);
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

// ---------------------------------------------------------------------------
// The glyph picker
// ---------------------------------------------------------------------------

pub enum GlyphPickResult {
    Continue,
    Cancel,
    /// The `glyph:...` value to store in the box's icon field.
    Picked(String),
}

/// Choose a glyph from the gallery (or by code point) and its color, with a
/// large live preview.
pub struct GlyphPicker {
    filter: String,
    selected: usize,
    color: usize,
}

impl GlyphPicker {
    /// Start on `current` (a `glyph:` value) when there is one.
    pub fn new(current: &str) -> Self {
        let mut picker = Self {
            filter: String::new(),
            selected: 0,
            color: 0,
        };
        if let Some(icon) = GlyphIcon::parse(current) {
            if let Some(pos) = picker.items().iter().position(|(_, c)| *c == icon.ch) {
                picker.selected = pos;
            }
            if let Some(Color::Rgb(r, g, b)) = icon.color {
                let hex = format!("#{r:02X}{g:02X}{b:02X}");
                picker.color = COLORS
                    .iter()
                    .position(|(_, c)| c.is_some_and(|c| c.eq_ignore_ascii_case(&hex)))
                    .unwrap_or(0);
            }
        }
        picker
    }

    /// Gallery entries matching the filter, plus the typed code point (or
    /// character) when it names a glyph the font has.
    fn items(&self) -> Vec<(String, char)> {
        let needle = self.filter.trim().to_lowercase();
        let mut items: Vec<(String, char)> = Vec::new();
        if !needle.is_empty() {
            if let Some(ch) = parse_code(&self.filter) {
                if has_glyph(ch) {
                    items.push((format!("U+{:04X}", ch as u32), ch));
                }
            }
        }
        for (name, ch) in GALLERY {
            if (needle.is_empty() || name.contains(&needle)) && has_glyph(*ch) && !items.iter().any(|(_, c)| c == ch) {
                items.push((name.to_string(), *ch));
            }
        }
        items
    }

    fn current(&self) -> Option<GlyphIcon> {
        let items = self.items();
        let (_, ch) = items.get(self.selected.min(items.len().saturating_sub(1)))?;
        let color = COLORS[self.color].1.and_then(hex_color);
        Some(GlyphIcon { ch: *ch, color })
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> GlyphPickResult {
        let count = self.items().len();
        match key.code {
            KeyCode::Esc => return GlyphPickResult::Cancel,
            KeyCode::Enter => {
                if let Some(icon) = self.current() {
                    return GlyphPickResult::Picked(icon.to_value());
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = count.saturating_sub(1),
            KeyCode::Char('[') => self.color = (self.color + COLORS.len() - 1) % COLORS.len(),
            KeyCode::Char(']') => self.color = (self.color + 1) % COLORS.len(),
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        GlyphPickResult::Continue
    }

    pub fn render(&self, frame: &mut Frame, screen: Rect, pal: &Palette, border: Color) {
        let width = screen.width.saturating_sub(4).clamp(30, 90);
        let height = screen.height.saturating_sub(2).clamp(12, 28);
        let area = Rect {
            x: screen.x + screen.width.saturating_sub(width) / 2,
            y: screen.y + screen.height.saturating_sub(height) / 2,
            width: width.min(screen.width),
            height: height.min(screen.height),
        };
        let inner = dm_box(frame, area, "Choose Icon Glyph", pal, pal.fill(), true);
        if inner.width < 20 || inner.height < 6 {
            return;
        }
        if !font_available() {
            put_line(frame, inner, 1, " No icon font found. Install a Nerd Font, or set", pal.fill());
            put_line(frame, inner, 2, " MENUMAKER_ICON_FONT to a .ttf file. Esc closes.", pal.fill());
            return;
        }
        put_line(frame, inner, 0, &format!(" Find: {}", self.filter), pal.label());
        frame.set_cursor(inner.x + 7 + self.filter.chars().count() as u16, inner.y);
        let color_name = COLORS[self.color].0;
        put_line(frame, inner, 1, &format!(" Color: ◄ {color_name} ►   ([ / ] change)"), pal.label());

        let list_w = (inner.width / 3).max(18).min(inner.width - 10);
        let list = Rect {
            x: inner.x,
            y: inner.y + 3,
            width: list_w,
            height: inner.height.saturating_sub(5),
        };
        let items = self.items();
        let selected = self.selected.min(items.len().saturating_sub(1));
        let top = if list.height > 0 && selected >= list.height as usize { selected + 1 - list.height as usize } else { 0 };
        if items.is_empty() {
            put_line(frame, list, 0, " (no matching glyphs)", pal.fill().add_modifier(Modifier::ITALIC));
        }
        for (row, (idx, (name, ch))) in items.iter().enumerate().skip(top).take(list.height as usize).enumerate() {
            let style = if idx == selected { pal.selected() } else { pal.fill() };
            put_line(frame, list, row as u16, &fit_width(&format!(" {ch}  {name}"), list_w as usize), style);
        }

        // Large preview on the box's own colors.
        let preview = Rect {
            x: inner.x + list_w + 1,
            y: inner.y + 3,
            width: inner.width - list_w - 1,
            height: inner.height.saturating_sub(5),
        };
        let preview_inner = dm_box(frame, preview, "PREVIEW", pal, pal.fill(), false);
        if let Some(icon) = self.current() {
            draw(icon, border, frame.buffer_mut(), preview_inner);
        }
        put_line(
            frame,
            inner,
            inner.height - 1,
            " ↑/↓ choose · type a name or code (f07b) · Enter use · Esc cancel",
            pal.fill().add_modifier(Modifier::ITALIC),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_values() {
        let icon = GlyphIcon::parse("glyph:f07b:#BDBD00").unwrap();
        assert_eq!(icon.ch, '\u{f07b}');
        assert_eq!(icon.color, Some(Color::Rgb(0xBD, 0xBD, 0)));
        assert_eq!(icon.to_value(), "glyph:f07b:#BDBD00");
        let plain = GlyphIcon::parse("glyph:U+F120").unwrap();
        assert_eq!((plain.ch, plain.color), ('\u{f120}', None));
        assert_eq!(plain.to_value(), "glyph:f120");
        assert!(GlyphIcon::parse("calc-icon.ans").is_none());
        assert!(GlyphIcon::parse(" glyph:41").is_some());
        assert_eq!(parse_code("A"), Some('A'));
        assert_eq!(parse_code("41"), Some('A'));
    }

    #[test]
    fn draws_within_the_area() {
        if !font_available() {
            return;
        }
        let area = Rect::new(2, 1, 12, 6);
        let mut buf = Buffer::empty(Rect::new(0, 0, 16, 8));
        draw(GlyphIcon { ch: 'A', color: None }, Color::White, &mut buf, area);
        let mut inked = 0;
        for y in 0..8 {
            for x in 0..16 {
                let sym = buf.get(x, y).symbol();
                if sym != " " {
                    inked += 1;
                    assert!(x >= 2 && x < 14 && y >= 1 && y < 7, "ink outside area at {x},{y}");
                }
            }
        }
        assert!(inked > 10);
    }
}
