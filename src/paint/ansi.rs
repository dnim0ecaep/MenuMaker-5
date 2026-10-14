//! ANSI art I/O: the painted-cell type, `.ans` decoding (CP437 + bare-LF
//! fix-up, fed through the `vt100` parser) and plain `.txt` loading. Ported
//! from Tui-Desktop's `wallpaper.rs` (only the pieces Paint needs).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::palette::RgbColor;

#[derive(Debug, Clone, PartialEq)]
pub struct PaintCellSer {
    pub ch: char,
    pub fg: RgbColor,
    pub bg: RgbColor,
}

pub type Cells = HashMap<(u16, u16), PaintCellSer>;

/// Converts a `vt100` cell color to RGB. Real-world `.ans` art almost never
/// uses truecolor SGR codes (`38;2;r;g;b`) — it uses the classic 16-color
/// indexed palette (codes 30-37/40-47, 90-97/100-107) or, less often, the
/// xterm 256-color cube. Previously only `Color::Rgb` was handled and every
/// indexed/default color collapsed to a flat gray, so imported art lost all
/// of its color and read as garbled gray blocks.
pub fn conv_ans_color(c: vt100::Color, bold: bool, is_bg: bool) -> RgbColor {
    match c {
        vt100::Color::Rgb(r, g, b) => RgbColor::new(r, g, b),
        vt100::Color::Idx(i) => idx_to_rgb(i, bold),
        // No SGR color was set for this cell: classic ANSI art defaults to
        // light gray text on a black background.
        vt100::Color::Default => {
            if is_bg {
                RgbColor::new(0, 0, 0)
            } else {
                RgbColor::new(170, 170, 170)
            }
        }
    }
}

/// The classic 16-color DOS/CGA text-mode palette that `.ans` art scene
/// files are authored against (what PabloDraw/Moebius/ACiDDraw render), not
/// the slightly different default xterm 16-color palette.
const DOS_PALETTE: [RgbColor; 16] = [
    RgbColor::new(0, 0, 0),
    RgbColor::new(170, 0, 0),
    RgbColor::new(0, 170, 0),
    RgbColor::new(170, 85, 0),
    RgbColor::new(0, 0, 170),
    RgbColor::new(170, 0, 170),
    RgbColor::new(0, 170, 170),
    RgbColor::new(170, 170, 170),
    RgbColor::new(85, 85, 85),
    RgbColor::new(255, 85, 85),
    RgbColor::new(85, 255, 85),
    RgbColor::new(255, 255, 85),
    RgbColor::new(85, 85, 255),
    RgbColor::new(255, 85, 255),
    RgbColor::new(85, 255, 255),
    RgbColor::new(255, 255, 255),
];

/// Resolves an indexed terminal color (0-255) to RGB: 0-7 are the low DOS
/// colors (bumped to their bright 8-15 counterpart when bold — `ESC[1;3Xm`
/// is how bright foreground colors are written in `.ans` art, since the
/// `90-97` bright SGR codes are a later, less universally supported
/// extension), 8-15 are already bright, 16-231 are the xterm 6x6x6 color
/// cube, and 232-255 are the grayscale ramp.
fn idx_to_rgb(idx: u8, bold: bool) -> RgbColor {
    match idx {
        0..=7 => DOS_PALETTE[if bold { idx as usize + 8 } else { idx as usize }],
        8..=15 => DOS_PALETTE[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            let r = level(i / 36);
            let g = level((i / 6) % 6);
            let b = level(i % 6);
            RgbColor::new(r, g, b)
        }
        232..=255 => {
            let v = 8 + (idx - 232) * 10;
            RgbColor::new(v, v, v)
        }
    }
}

/// The upper half (0x80-0xFF) of code page 437, the encoding virtually all
/// `.ans` art from the BBS/scene era is authored in — box-drawing, block
/// shading and line-art glyphs live there. Bytes 0x00-0x7F are identical to
/// ASCII/UTF-8 so only this half needs a table.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{00A0}',
];

/// Transcodes CP437 bytes to UTF-8 before handing them to the `vt100`
/// parser, which expects a UTF-8 stream, and normalizes bare `\n` line
/// endings to `\r\n`. Fed raw, every CP437 byte >= 0x80 (i.e. almost every
/// visible glyph in a `.ans` piece — box-drawing lines, block shading, etc.)
/// looks like an invalid or wrongly-continued UTF-8 sequence and comes out
/// as mojibake. Files that are already valid UTF-8 (Paint's own `.ans`
/// exports, or art explicitly saved in Unicode mode) skip that step so this
/// never double-converts them.
///
/// Separately, `.ans` art is near-universally authored with bare `\n` line
/// breaks (no `\r`), relying on a viewer/terminal to also return the cursor
/// to column 0. `vt100` emulates a real terminal strictly: a bare LF only
/// moves the cursor down a row, leaving its column untouched. Fed the raw
/// file, each successive source line then starts one line's-width further
/// right than the last, staircasing the whole image diagonally across the
/// (wider, to tolerate this) parsing canvas instead of stacking cleanly —
/// this is what actually produced the garbled, offset-looking renders, not
/// color or glyph loss. Inserting the `\r` before feeding `vt100` restores
/// the column-0 reset every real `.ans` viewer performs.
pub fn decode_ans_bytes(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let needs_cp437 = std::str::from_utf8(bytes).is_err();
    let needs_crlf = bytes.iter().enumerate().any(|(i, &b)| b == b'\n' && (i == 0 || bytes[i - 1] != b'\r'));
    if !needs_cp437 && !needs_crlf {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut prev = 0u8;
    for &b in bytes {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        if needs_cp437 && b >= 0x80 {
            let mut buf = [0u8; 4];
            out.extend_from_slice(CP437_HIGH[(b - 0x80) as usize].encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(b);
        }
        prev = b;
    }
    std::borrow::Cow::Owned(out)
}

/// The smallest rect `(min_x, min_y, max_x, max_y)` containing every cell.
pub fn bounds_of(cells: &Cells) -> Option<(u16, u16, u16, u16)> {
    let min_x = cells.keys().map(|(x, _)| *x).min()?;
    let max_x = cells.keys().map(|(x, _)| *x).max()?;
    let min_y = cells.keys().map(|(_, y)| *y).min()?;
    let max_y = cells.keys().map(|(_, y)| *y).max()?;
    Some((min_x, min_y, max_x, max_y))
}

/// Parses `.ans` bytes into cells (blank cells are left out).
pub fn parse_ans(bytes: &[u8]) -> Cells {
    let bytes = decode_ans_bytes(bytes);
    let (width, height) = (200u16, 80u16);
    let mut parser = vt100::Parser::new(height, width, 0);
    parser.process(&bytes);
    let screen = parser.screen();
    let mut cells = HashMap::new();
    for row in 0..height {
        for col in 0..width {
            let Some(cell) = screen.cell(row, col) else { continue };
            let s = cell.contents();
            if s.is_empty() || s == " " {
                continue;
            }
            if let Some(ch) = s.chars().next() {
                let fg = conv_ans_color(cell.fgcolor(), cell.bold(), false);
                let bg = conv_ans_color(cell.bgcolor(), cell.bold(), true);
                cells.insert((col, row), PaintCellSer { ch, fg, bg });
            }
        }
    }
    cells
}

pub fn load_ans(path: &Path) -> anyhow::Result<Cells> {
    Ok(parse_ans(&fs::read(path)?))
}

/// Imports a plain-text file as monochrome art: every non-space character
/// becomes a cell in `fg` on black, rows split on `\n`.
pub fn load_txt(path: &Path, fg: RgbColor) -> anyhow::Result<Cells> {
    Ok(parse_txt(&fs::read_to_string(path)?, fg))
}

pub fn parse_txt(text: &str, fg: RgbColor) -> Cells {
    let mut cells = HashMap::new();
    for (row, line) in text.lines().enumerate() {
        for (col, ch) in line.trim_end_matches('\r').chars().enumerate() {
            if ch == ' ' || row > u16::MAX as usize || col > u16::MAX as usize {
                continue;
            }
            cells.insert((col as u16, row as u16), PaintCellSer { ch, fg, bg: RgbColor::new(0, 0, 0) });
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_leaves_clean_utf8_crlf_untouched() {
        let bytes = "ab\r\ncd".as_bytes();
        assert!(matches!(decode_ans_bytes(bytes), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn decode_transcodes_cp437_and_fixes_bare_lf() {
        let bytes = [0xDBu8, b'\n', 0xB0];
        let out = decode_ans_bytes(&bytes);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "█\r\n░");
    }

    #[test]
    fn indexed_colors_use_dos_palette_and_bold_brightens() {
        assert_eq!(conv_ans_color(vt100::Color::Idx(1), false, false), RgbColor::new(170, 0, 0));
        assert_eq!(conv_ans_color(vt100::Color::Idx(1), true, false), RgbColor::new(255, 85, 85));
        assert_eq!(conv_ans_color(vt100::Color::Default, false, true), RgbColor::new(0, 0, 0));
        assert_eq!(conv_ans_color(vt100::Color::Rgb(1, 2, 3), false, false), RgbColor::new(1, 2, 3));
    }

    #[test]
    fn parse_ans_reads_truecolor_cells() {
        let cells = parse_ans(b"\x1b[38;2;1;2;3m\x1b[48;2;4;5;6mX\x1b[0m \x1b[31mY\r\n");
        assert_eq!(cells.get(&(0, 0)), Some(&PaintCellSer { ch: 'X', fg: RgbColor::new(1, 2, 3), bg: RgbColor::new(4, 5, 6) }));
        assert_eq!(cells.get(&(1, 0)), None);
        assert_eq!(cells.get(&(2, 0)).map(|c| c.ch), Some('Y'));
    }

    #[test]
    fn parse_txt_skips_spaces() {
        let fg = RgbColor::new(9, 9, 9);
        let cells = parse_txt("a b\r\n c", fg);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells.get(&(2, 0)).map(|c| c.ch), Some('b'));
        assert_eq!(cells.get(&(1, 1)).map(|c| c.ch), Some('c'));
        assert_eq!(bounds_of(&cells), Some((0, 0, 2, 1)));
    }
}
