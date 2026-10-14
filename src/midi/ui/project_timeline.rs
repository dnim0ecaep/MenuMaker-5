//! Project timeline view rendering.
//!
//! Displays all tracks on a combined timeline, showing note blocks for each track
//! and visual feedback for tracks that are currently playing audio.

use super::{panel, put, put_cell};
use crate::dmui::{fit_width, pad_width, Palette};
use crate::midi::app::App;
use crate::midi::model::{contains_beat, contains_measure, Note, Track};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

/// Track row height in the project timeline.
const TRACK_ROW_HEIGHT: u16 = 2;

/// Default width reserved for track labels on the left.
const DEFAULT_LABEL_WIDTH: u16 = 12;

/// Compact label width for combined view (matches piano key width).
pub const COMPACT_LABEL_WIDTH: u16 = 5;

/// Renders the project timeline view showing all tracks.
///
/// # Returns
///
/// The time ruler region for mouse hit testing, or None if too small to render.
pub fn render_project_timeline(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) -> Option<Rect> {
    render_project_timeline_with_label_width(frame, area, app, focused, DEFAULT_LABEL_WIDTH, pal)
}

/// Renders the project timeline with a compact label width for combined view,
/// aligned with the piano roll's key column.
pub fn render_project_timeline_compact(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) -> Option<Rect> {
    render_project_timeline_with_label_width(frame, area, app, focused, COMPACT_LABEL_WIDTH, pal)
}

/// Internal render function with configurable label width.
fn render_project_timeline_with_label_width(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    focused: bool,
    label_width: u16,
    pal: &Palette,
) -> Option<Rect> {
    let inner = panel(frame, area, "Project Timeline - All Tracks", pal, focused);

    if inner.width < 20 || inner.height < 3 {
        return None; // Too small to render
    }

    let zoom = app.zoom.max(1);
    let timeline_width = inner.width.saturating_sub(label_width);
    let max_tracks_visible = ((inner.height - 1) / TRACK_ROW_HEIGHT) as usize;

    // Calculate which tracks to show (scrolled view if many tracks)
    let track_count = app.project().track_count();
    let start_track = if track_count > max_tracks_visible {
        app.selected_track_index
            .saturating_sub(max_tracks_visible / 2)
            .min(track_count.saturating_sub(max_tracks_visible))
    } else {
        0
    };
    let end_track = (start_track + max_tracks_visible).min(track_count);

    let buf = frame.buffer_mut();
    let ruler_rect = Rect::new(inner.x + label_width, inner.y, timeline_width, 1);
    super::render_time_ruler(buf, ruler_rect, app.scroll_x, zoom, pal);

    for (display_idx, track_idx) in (start_track..end_track).enumerate() {
        let track = &app.project().tracks()[track_idx];
        let track_y = inner.y + 1 + (display_idx as u16 * TRACK_ROW_HEIGHT);
        if track_y + TRACK_ROW_HEIGHT > inner.y + inner.height {
            break;
        }

        let is_selected = track_idx == app.selected_track_index;
        let is_active = app.active_tracks.contains(&track_idx);

        let label_style = if is_active {
            pal.fill().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else if is_selected {
            pal.selected()
        } else if track.muted {
            pal.fill().add_modifier(Modifier::DIM)
        } else {
            pal.fill()
        };
        let label_text = build_track_label(track, is_active, track.muted, track.solo, label_width);
        for r in 0..TRACK_ROW_HEIGHT {
            let text = if r == 0 { label_text.as_str() } else { "" };
            put(buf, inner.x, track_y + r, &pad_width(text, label_width as usize), label_style, label_width);
        }

        render_track_content(
            buf,
            Rect::new(inner.x + label_width, track_y, timeline_width, TRACK_ROW_HEIGHT),
            app,
            track_idx,
            is_selected,
            is_active,
            track.muted,
        );
    }

    // Playhead: same column as the piano roll's (tick / zoom == cursor_tick / zoom).
    if app.audio.is_playing() {
        let cursor_col = app.cursor_tick / zoom;
        let start_col = app.scroll_x / zoom;
        if cursor_col >= start_col {
            let screen_col = cursor_col - start_col;
            if screen_col < timeline_width as u32 {
                let playhead_x = inner.x + label_width + screen_col as u16;
                for row in 0..inner.height.saturating_sub(1) {
                    let y = inner.y + 1 + row;
                    let bg = buf.get(playhead_x, y).bg;
                    put_cell(buf, inner, playhead_x, y, '|', Style::default().fg(Color::Red).bg(bg).add_modifier(Modifier::BOLD));
                }
            }
        }
    }

    Some(ruler_rect)
}

/// Builds track label text adapted to the available width.
fn build_track_label(track: &Track, is_active: bool, is_muted: bool, is_solo: bool, label_width: u16) -> String {
    let active_char = if is_active { '*' } else { ' ' };

    if label_width <= COMPACT_LABEL_WIDTH {
        // Compact mode: just show activity indicator and truncated name
        let max_name = (label_width as usize).saturating_sub(2);
        let name = if unicode_width::UnicodeWidthStr::width(track.name.as_str()) > max_name {
            format!("{}.", fit_width(&track.name, max_name.saturating_sub(1)))
        } else {
            track.name.clone()
        };
        format!("{} {}", active_char, name)
    } else {
        // Full mode: show all indicators and longer name
        let mute_char = if is_muted { 'M' } else { ' ' };
        let solo_char = if is_solo { 'S' } else { ' ' };
        let max_name = (label_width as usize).saturating_sub(4);
        let name = if unicode_width::UnicodeWidthStr::width(track.name.as_str()) > max_name {
            format!("{}...", fit_width(&track.name, max_name.saturating_sub(3)))
        } else {
            track.name.clone()
        };
        format!("{}{}{} {}", mute_char, solo_char, active_char, name)
    }
}

/// Renders the content of a single track (note blocks).
fn render_track_content(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    track_idx: usize,
    is_selected: bool,
    is_active: bool,
    is_muted: bool,
) {
    let track = &app.project().tracks()[track_idx];
    let zoom = app.zoom.max(1);

    // Different colors for different tracks for visual distinction
    let track_colors = [
        Color::Blue,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::Cyan,
        Color::Red,
        Color::LightBlue,
        Color::LightGreen,
    ];
    let base_color = track_colors[track_idx % track_colors.len()];
    let note_color = if is_muted {
        Color::DarkGray
    } else if is_active && app.highlight_timeline() {
        Color::White
    } else {
        base_color
    };

    let window_end = app.scroll_x.saturating_add(zoom.saturating_mul(area.width as u32));
    let notes: Vec<&Note> = track
        .notes()
        .iter()
        .filter(|n| n.end_tick() > app.scroll_x && n.start_tick < window_end)
        .collect();
    let grid_bg = Color::Rgb(20, 20, 24);

    for col in 0..area.width {
        let tick = app.scroll_x + (col as u32 * zoom);
        let tick_end = tick + zoom;
        let has_note = notes.iter().any(|n| n.start_tick < tick_end && n.end_tick() > tick);
        let is_cursor = is_selected && (tick / zoom == app.cursor_tick / zoom);
        let is_measure = contains_measure(tick, zoom);
        let is_beat = contains_beat(tick, zoom);

        for row in 0..area.height {
            let (ch, style) = if has_note {
                let bg = if is_cursor { Color::Cyan } else { note_color };
                ('=', Style::default().fg(Color::Black).bg(bg))
            } else if is_cursor && row == 0 {
                ('_', Style::default().fg(Color::Cyan).bg(grid_bg))
            } else {
                let ch = if is_measure {
                    '|'
                } else if is_beat && row == 0 {
                    ':'
                } else {
                    ' '
                };
                let fg = if is_measure { Color::DarkGray } else { Color::Rgb(60, 60, 60) };
                (ch, Style::default().fg(fg).bg(grid_bg))
            };
            put_cell(buf, area, area.x + col, area.y + row, ch, style);
        }
    }
}
