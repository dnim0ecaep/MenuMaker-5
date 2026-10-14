//! Track list rendering.
//!
//! Displays all tracks with their names, instruments, volume, pan, and mute/solo states.
//! Supports both compact (single-line) and expanded (two-line) views,
//! and the rename input.
//!
//! Port note: the list is drawn directly (no ratatui `List`) with the
//! selected track in the theme's selection colors instead of a `> ` marker,
//! so the `M`/`S` indicators sit in the first two columns that the click
//! handler toggles.

use super::{panel, put};
use crate::dmui::{fit_width, Palette};
use crate::midi::app::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

/// Height reserved for the control hints at the bottom.
const CONTROLS_HEIGHT: u16 = 2;

/// Returns the display color for a volume value.
///
/// Red for clipping (>100), yellow for hot (>80), green otherwise.
#[inline]
fn volume_color(volume: u8) -> Color {
    if volume > 100 {
        Color::Red
    } else if volume > 80 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Formats a pan value (0-127) as a display string.
///
/// Returns "L##" for left, "R##" for right, "C  " for center.
#[inline]
fn format_pan(pan: u8) -> String {
    if pan < 64 {
        format!("L{:2}", 64 - pan)
    } else if pan > 64 {
        format!("R{:2}", pan - 64)
    } else {
        "C  ".to_string()
    }
}

/// Shortens `text` to `max` columns, ending with "..." when cut.
fn ellipsize(text: &str, max: usize) -> String {
    if unicode_width::UnicodeWidthStr::width(text) <= max {
        text.to_string()
    } else {
        format!("{}...", fit_width(text, max.saturating_sub(3)))
    }
}

/// Writes a run of styled segments on one row, clipped to `area`.
fn put_segments(buf: &mut Buffer, area: Rect, y: u16, segments: &[(String, Style)]) {
    let mut x = area.x;
    for (text, style) in segments {
        if x >= area.right() {
            break;
        }
        let w = super::text_w(text);
        put(buf, x, y, text, *style, area.right() - x);
        x = x.saturating_add(w);
    }
}

/// Renders the track list panel on the left side.
pub fn render_track_list(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) {
    let inner = panel(frame, area, "Tracks", pal, focused);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let list_height = inner.height.saturating_sub(CONTROLS_HEIGHT);
    let list_area = Rect { height: list_height, ..inner };
    let base = pal.fill();
    let buf = frame.buffer_mut();

    let rows_per_track: usize = if app.expanded_tracks { 2 } else { 1 };
    let visible_items = (list_height as usize / rows_per_track).max(1);
    let selected = app.selected_track_index;
    // Same scroll rule as the click handler: keep the selection visible.
    let scroll_offset = if selected >= visible_items { selected - visible_items + 1 } else { 0 };

    for (i, track) in app.project().tracks().iter().enumerate().skip(scroll_offset) {
        let row = ((i - scroll_offset) * rows_per_track) as u16;
        if row + rows_per_track as u16 > list_height {
            break;
        }
        let y = list_area.y + row;
        let is_selected = i == selected;
        let is_renaming = app.renaming_track && is_selected;
        let is_active = app.active_tracks.contains(&i);
        let row_base = if is_selected { pal.selected() } else { base };
        for r in 0..rows_per_track as u16 {
            put(buf, list_area.x, y + r, &" ".repeat(list_area.width as usize), row_base, list_area.width);
        }

        let mute = if track.muted {
            ("M".to_string(), row_base.fg(Color::Red).add_modifier(Modifier::BOLD))
        } else {
            (".".to_string(), row_base)
        };
        let solo = if track.solo {
            ("S".to_string(), row_base.fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else {
            (".".to_string(), row_base)
        };
        let activity = if is_active {
            ("*".to_string(), row_base.fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            (" ".to_string(), row_base)
        };
        let name_style = if is_active {
            row_base.fg(Color::Green).add_modifier(Modifier::BOLD)
        } else if is_selected {
            row_base.add_modifier(Modifier::BOLD)
        } else {
            row_base
        };
        let rename_style = Style::default().fg(pal.selection_text).bg(pal.accent).add_modifier(Modifier::BOLD);

        if app.expanded_tracks {
            // Line 1: indicators + track name; Line 2: volume + pan + instrument
            let max_name_len = area.width.saturating_sub(6) as usize;
            let mut line1 = vec![mute, solo, activity, (" ".to_string(), row_base)];
            if is_renaming {
                line1.push((format!("{}_", ellipsize(&app.rename_buffer, max_name_len.saturating_sub(1))), rename_style));
            } else {
                line1.push((ellipsize(&track.name, max_name_len), name_style));
            }
            put_segments(buf, list_area, y, &line1);

            let instrument = app.get_instrument_name(track.program);
            let max_inst_len = area.width.saturating_sub(14) as usize;
            let line2 = vec![
                ("  ".to_string(), row_base),
                (format!("V{:3}", track.volume), row_base.fg(volume_color(track.volume))),
                (" ".to_string(), row_base),
                (format_pan(track.pan), row_base.fg(Color::Cyan)),
                (" ".to_string(), row_base),
                (ellipsize(instrument, max_inst_len), row_base),
            ];
            put_segments(buf, list_area, y + 1, &line2);
        } else {
            // Compact view: single line per track
            let mut spans = vec![mute, solo, activity, (" ".to_string(), row_base)];
            let mut extra_chars = 0u16;
            if track.volume != 100 {
                spans.push((format!("V{:3}", track.volume), row_base.fg(volume_color(track.volume))));
                spans.push((" ".to_string(), row_base));
                extra_chars += 5;
            }
            if track.pan != 64 {
                spans.push((format_pan(track.pan), row_base.fg(Color::Cyan)));
                spans.push((" ".to_string(), row_base));
                extra_chars += 4;
            }
            let max_name_len = area.width.saturating_sub(10 + extra_chars) as usize;
            if is_renaming {
                spans.push((format!("{}_", ellipsize(&app.rename_buffer, max_name_len.saturating_sub(1))), rename_style));
            } else {
                spans.push((ellipsize(&track.name, max_name_len), name_style));
            }
            put_segments(buf, list_area, y, &spans);
        }
    }

    // Control hints
    let key_style = pal.label();
    let controls = [
        vec![
            ("[".to_string(), base),
            (";/'".to_string(), key_style),
            ("]Vol ".to_string(), base),
            ("[".to_string(), base),
            ("(/)".to_string(), key_style),
            ("]Pan ".to_string(), base),
            ("[".to_string(), base),
            ("</>".to_string(), key_style),
            ("]Inst".to_string(), base),
        ],
        vec![
            ("[".to_string(), base),
            ("m".to_string(), key_style),
            ("]Mute ".to_string(), base),
            ("[".to_string(), base),
            ("s".to_string(), key_style),
            ("]Solo ".to_string(), base),
            ("[".to_string(), base),
            ("x".to_string(), key_style),
            ("]Del".to_string(), base),
        ],
    ];
    for (i, line) in controls.iter().enumerate() {
        let row = list_height + i as u16;
        if row < inner.height {
            put_segments(buf, inner, inner.y + row, line);
        }
    }
}
