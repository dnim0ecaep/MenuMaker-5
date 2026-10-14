//! Piano keyboard display.
//!
//! Shows the computer keyboard to MIDI note mapping and recently added notes.
//! Also displays contextual key bindings based on the current edit mode.

use super::{panel, put};
use crate::dmui::Palette;
use crate::midi::app::{App, EditMode, KEYBOARD_MAP};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

/// Draws a keyboard row: each key with its note's black/white styling.
fn render_keyboard_row(buf: &mut Buffer, area: Rect, y: u16, keys: &[char], app: &App, pal: &Palette) {
    let mut x = area.x;
    for &key in keys {
        if x >= area.right() {
            break;
        }
        let base_note = KEYBOARD_MAP.iter().find(|(k, _)| k.to_ascii_uppercase() == key).map(|(_, n)| *n);
        let style = match base_note {
            Some(base) => {
                let note = (base as i16 + app.octave_offset as i16 * 12).clamp(0, 127) as u8;
                let is_black = matches!(note % 12, 1 | 3 | 6 | 8 | 10);
                if app.is_recently_added_pitch(note) {
                    Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
                } else if is_black {
                    Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD)
                }
            }
            None => pal.fill(),
        };
        put(buf, x, y, &format!(" {} ", key), style, area.right() - x);
        x += 3;
    }
}

/// Renders the piano keyboard at the bottom of the screen.
pub fn render_keyboard(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) {
    let octave_str = if app.octave_offset >= 0 {
        format!("+{}", app.octave_offset)
    } else {
        format!("{}", app.octave_offset)
    };
    let inner = panel(frame, area, &format!("Keyboard (Octave: {})", octave_str), pal, focused);
    if inner.height < 3 || inner.width == 0 {
        return;
    }

    // Keyboard layout: upper row (Q-I) and lower row (Z-M)
    const UPPER_KEYS: &[char] = &['Q', '2', 'W', '3', 'E', 'R', '5', 'T', '6', 'Y', '7', 'U', 'I'];
    const LOWER_KEYS: &[char] = &['Z', 'S', 'X', 'D', 'C', 'V', 'G', 'B', 'H', 'N', 'J', 'M'];

    let buf = frame.buffer_mut();
    render_keyboard_row(buf, inner, inner.y, UPPER_KEYS, app, pal);
    render_keyboard_row(buf, inner, inner.y + 1, LOWER_KEYS, app, pal);

    // Contextual help text based on current mode
    let mut x = inner.x;
    for (text, style) in contextual_help(app.edit_mode, pal) {
        if x >= inner.right() {
            break;
        }
        put(buf, x, inner.y + 2, text, style, inner.right() - x);
        x = x.saturating_add(super::text_w(text));
    }
}

/// The contextual help line for the current edit mode.
fn contextual_help(mode: EditMode, pal: &Palette) -> Vec<(&'static str, Style)> {
    let key = pal.label();
    let desc = pal.fill();
    let k = |text| (text, key);
    let d = |text| (text, desc);
    match mode {
        EditMode::Normal => vec![
            d("["), k("i"), d("]Ins "),
            d("["), k("v"), d("]Sel "),
            d("["), k("Space"), d("]Play "),
            d("["), k("^S"), d("]Save "),
            d("["), k("?"), d("]Help "),
            d("["), k("Esc"), d("]Close"),
        ],
        EditMode::Insert => vec![
            ("INSERT MODE  ", desc.fg(Color::Green).add_modifier(Modifier::BOLD)),
            d("["), k("Z-M"), d("] Play+Add  "),
            d("["), k(",/"), d("] Octave  "),
            d("["), k("Arrows"), d("] Move  "),
            d("["), k("Esc"), d("] Exit"),
        ],
        EditMode::Select => vec![
            ("SELECT MODE  ", desc.fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            d("["), k("Enter"), d("] Toggle  "),
            d("["), k("x"), d("] Delete  "),
            d("["), k("hjkl"), d("] Move  "),
            d("["), k("Esc"), d("] Exit"),
        ],
    }
}
