//! Paint's color palette: 16 built-in swatches plus user-added ones, and the
//! `ColorComposer` used by "Add Color" and "Edit Colors". Unlike the
//! original (a process-wide static), the palette is owned by `PaintApp` and
//! persisted to a small hidden JSON file next to the paintings.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RgbColor(pub u8, pub u8, pub u8);

impl RgbColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b)
    }
}

impl From<RgbColor> for Color {
    fn from(c: RgbColor) -> Self {
        Color::Rgb(c.0, c.1, c.2)
    }
}

/// A small fixed palette chosen to look good on light and dark terminals.
pub const PAINT_PALETTE: [RgbColor; 16] = [
    RgbColor::new(20, 20, 24),    // black
    RgbColor::new(230, 80, 80),   // red
    RgbColor::new(100, 220, 120), // green
    RgbColor::new(235, 220, 80),  // yellow
    RgbColor::new(90, 150, 240),  // blue
    RgbColor::new(170, 120, 240), // purple
    RgbColor::new(80, 220, 220),  // cyan
    RgbColor::new(235, 235, 240), // white
    RgbColor::new(90, 90, 100),   // dark gray
    RgbColor::new(250, 130, 130), // light red
    RgbColor::new(150, 240, 160), // light green
    RgbColor::new(245, 235, 140), // light yellow
    RgbColor::new(140, 180, 250), // light blue
    RgbColor::new(210, 170, 250), // light purple
    RgbColor::new(150, 240, 240), // light cyan
    RgbColor::new(255, 255, 255), // bright white
];

/// The palette only ever grows (no remove), so every index handed out
/// (fg/bg) stays valid; it is capped at this many entries.
const MAX_PALETTE_LEN: usize = 64;

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    base_overrides: HashMap<usize, RgbColor>,
    #[serde(default)]
    extra: Vec<RgbColor>,
}

pub struct ColorPalette {
    colors: Vec<RgbColor>,
    /// Where additions/edits are persisted; `None` keeps them in memory.
    path: Option<PathBuf>,
}

impl ColorPalette {
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut colors = PAINT_PALETTE.to_vec();
        let stored: Stored = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        for (idx, c) in stored.base_overrides {
            if let Some(slot) = colors.get_mut(idx) {
                *slot = c;
            }
        }
        for c in stored.extra {
            if colors.len() < MAX_PALETTE_LEN && !colors.contains(&c) {
                colors.push(c);
            }
        }
        Self { colors, path }
    }

    pub fn colors(&self) -> &[RgbColor] {
        &self.colors
    }

    pub fn len(&self) -> usize {
        self.colors.len()
    }

    /// Falls back to black for an out-of-range index rather than panicking.
    pub fn color_at(&self, idx: usize) -> RgbColor {
        self.colors.get(idx).copied().unwrap_or(RgbColor::new(0, 0, 0))
    }

    /// Appends `color` (a no-op if already present or the palette is full)
    /// and persists it. Returns the entry's index either way.
    pub fn add_color(&mut self, color: RgbColor) -> usize {
        if let Some(idx) = self.colors.iter().position(|&c| c == color) {
            return idx;
        }
        if self.colors.len() >= MAX_PALETTE_LEN {
            return self.colors.len() - 1;
        }
        self.colors.push(color);
        self.save();
        self.colors.len() - 1
    }

    /// Overwrites an existing slot in place ("Edit Colors"). Cells already
    /// painted keep their exact RGB. A no-op if `idx` is out of range.
    pub fn set_color_at(&mut self, idx: usize, color: RgbColor) {
        let Some(slot) = self.colors.get_mut(idx) else { return };
        *slot = color;
        self.save();
    }

    pub fn nearest_index(&self, color: RgbColor) -> usize {
        self.colors
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| color_distance(**c, color))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let end = PAINT_PALETTE.len().min(self.colors.len());
        let stored = Stored {
            base_overrides: (0..end)
                .filter(|&i| self.colors[i] != PAINT_PALETTE[i])
                .map(|i| (i, self.colors[i]))
                .collect(),
            extra: self.colors[end..].to_vec(),
        };
        if let Ok(text) = serde_json::to_string_pretty(&stored) {
            let _ = fs::write(path, text);
        }
    }
}

pub fn color_distance(a: RgbColor, b: RgbColor) -> i32 {
    let dr = a.0 as i32 - b.0 as i32;
    let dg = a.1 as i32 - b.1 as i32;
    let db = a.2 as i32 - b.2 as i32;
    dr * dr + dg * dg + db * db
}

/// Which channel `ColorComposer::adjust` nudges.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RgbChannel {
    R,
    G,
    B,
}

impl RgbChannel {
    pub fn next(self) -> Self {
        match self {
            RgbChannel::R => RgbChannel::G,
            RgbChannel::G => RgbChannel::B,
            RgbChannel::B => RgbChannel::R,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RgbChannel::R => "R",
            RgbChannel::G => "G",
            RgbChannel::B => "B",
        }
    }
}

/// "Compose a color" state machine: channel cycling, ±1/±10 nudges and
/// manual hex entry.
pub struct ColorComposer {
    pub color: RgbColor,
    pub channel: RgbChannel,
    /// `Some(buffer)` while typing a hex code — overrides the channel
    /// controls until applied or cancelled.
    pub hex_input: Option<String>,
}

impl ColorComposer {
    pub fn new(seed: RgbColor) -> Self {
        Self { color: seed, channel: RgbChannel::R, hex_input: None }
    }

    pub fn channel_value(&self, channel: RgbChannel) -> u8 {
        match channel {
            RgbChannel::R => self.color.0,
            RgbChannel::G => self.color.1,
            RgbChannel::B => self.color.2,
        }
    }

    pub fn adjust(&mut self, delta: i32) {
        let RgbColor(mut r, mut g, mut b) = self.color;
        let chan = match self.channel {
            RgbChannel::R => &mut r,
            RgbChannel::G => &mut g,
            RgbChannel::B => &mut b,
        };
        *chan = (*chan as i32 + delta).clamp(0, 255) as u8;
        self.color = RgbColor::new(r, g, b);
    }

    pub fn cycle_channel(&mut self) {
        self.channel = self.channel.next();
    }

    pub fn begin_hex_input(&mut self) {
        self.hex_input = Some(String::new());
    }

    pub fn push_hex_char(&mut self, c: char) {
        if let Some(buf) = &mut self.hex_input {
            if buf.len() < 6 && c.is_ascii_hexdigit() {
                buf.push(c.to_ascii_uppercase());
            }
        }
    }

    pub fn backspace_hex(&mut self) {
        if let Some(buf) = &mut self.hex_input {
            buf.pop();
        }
    }

    /// Applies the typed hex code if it is exactly 6 digits; otherwise
    /// leaves `hex_input` alone so the user can keep typing or Esc out.
    pub fn commit_hex(&mut self) {
        let Some(buf) = &self.hex_input else { return };
        if buf.len() != 6 {
            return;
        }
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&buf[0..2], 16),
            u8::from_str_radix(&buf[2..4], 16),
            u8::from_str_radix(&buf[4..6], 16),
        ) {
            self.color = RgbColor::new(r, g, b);
            self.hex_input = None;
        }
    }

    pub fn cancel_hex_input(&mut self) {
        self.hex_input = None;
    }

    pub fn status(&self) -> String {
        if let Some(buf) = &self.hex_input {
            format!("Hex: #{buf}_  (0-9/A-F, Enter applies, Esc cancels)")
        } else {
            let RgbColor(r, g, b) = self.color;
            format!(
                "Color: R:{r} G:{g} B:{b}  #{r:02X}{g:02X}{b:02X}  ({} active, # for hex)",
                self.channel.label()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_adjust_clamps_and_cycles_channels() {
        let mut c = ColorComposer::new(RgbColor::new(250, 0, 10));
        c.adjust(10);
        assert_eq!(c.color, RgbColor::new(255, 0, 10));
        c.cycle_channel();
        c.adjust(-5);
        assert_eq!(c.color, RgbColor::new(255, 0, 10));
        c.cycle_channel();
        c.adjust(-10);
        assert_eq!(c.color, RgbColor::new(255, 0, 0));
        c.cycle_channel();
        assert_eq!(c.channel, RgbChannel::R);
    }

    #[test]
    fn composer_hex_input_only_applies_six_hex_digits() {
        let mut c = ColorComposer::new(RgbColor::new(0, 0, 0));
        c.begin_hex_input();
        for ch in "12zab".chars() {
            c.push_hex_char(ch); // 'z' is ignored
        }
        c.commit_hex();
        assert!(c.hex_input.is_some(), "4 digits must not apply");
        for ch in "cde".chars() {
            c.push_hex_char(ch); // 'e' is ignored, already 6 digits
        }
        c.commit_hex();
        assert_eq!(c.hex_input, None);
        assert_eq!(c.color, RgbColor::new(0x12, 0xAB, 0xCD));
    }

    #[test]
    fn palette_add_dedupes_and_persists() {
        let dir = std::env::temp_dir().join(format!("mm-paint-pal-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("palette.json");
        let _ = fs::remove_file(&path);
        let mut pal = ColorPalette::load(Some(path.clone()));
        assert_eq!(pal.len(), 16);
        let idx = pal.add_color(RgbColor::new(1, 2, 3));
        assert_eq!(idx, 16);
        assert_eq!(pal.add_color(RgbColor::new(1, 2, 3)), 16);
        assert_eq!(pal.add_color(PAINT_PALETTE[3]), 3);
        pal.set_color_at(0, RgbColor::new(9, 9, 9));
        let reloaded = ColorPalette::load(Some(path));
        assert_eq!(reloaded.len(), 17);
        assert_eq!(reloaded.color_at(16), RgbColor::new(1, 2, 3));
        assert_eq!(reloaded.color_at(0), RgbColor::new(9, 9, 9));
        assert_eq!(reloaded.color_at(999), RgbColor::new(0, 0, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nearest_index_finds_closest_swatch() {
        let pal = ColorPalette::load(None);
        assert_eq!(pal.nearest_index(RgbColor::new(229, 81, 79)), 1);
        assert_eq!(pal.nearest_index(RgbColor::new(0, 0, 0)), 0);
    }
}
