//! Piano roll editor rendering.
//!
//! Displays notes on a grid with pitch on the Y-axis and time on the X-axis.
//! Similar to a DAW piano roll interface. Includes visual indicators for
//! notes that are scrolled off-screen.
//!
//! Port note: cells are written straight into the buffer, and only the notes
//! overlapping the visible window are scanned per cell.

use super::{panel, put, put_cell};
use crate::dmui::Palette;
use crate::midi::app::{App, EditMode};
use crate::midi::model::{contains_beat, contains_measure, note_to_name, Note};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

/// Tracks which edges of the piano roll have notes scrolled off-screen.
#[derive(Debug, Default, Clone, Copy)]
struct OffScreenIndicators {
    /// Notes exist above the visible pitch range.
    above: bool,
    /// Notes exist below the visible pitch range.
    below: bool,
    /// Notes extend to the left of the visible tick range.
    left: bool,
    /// Notes extend to the right of the visible tick range.
    right: bool,
}

impl OffScreenIndicators {
    /// Calculates which edges have off-screen notes based on the current viewport.
    fn calculate(notes: &[Note], scroll_x: u32, scroll_y: u8, visible_ticks: u64, visible_pitches: u8) -> Self {
        let mut indicators = Self::default();

        let pitch_min = scroll_y;
        let pitch_max = (scroll_y as u16 + visible_pitches as u16).min(128);
        let tick_min = scroll_x;
        let tick_max = scroll_x.saturating_add(visible_ticks.min(u32::MAX as u64) as u32);

        for note in notes {
            if note.pitch as u16 >= pitch_max {
                indicators.above = true;
            }
            if note.pitch < pitch_min {
                indicators.below = true;
            }
            if note.start_tick < tick_min && note.end_tick() > tick_min {
                indicators.left = true;
            }
            if note.start_tick < tick_max && note.end_tick() > tick_max {
                indicators.right = true;
            }
            if indicators.above && indicators.below && indicators.left && indicators.right {
                break;
            }
        }

        indicators
    }
}

/// Builds a compact indicator string for the title showing off-screen notes,
/// like "[^v<>]"; empty when nothing is off-screen.
fn build_title_indicator(indicators: &OffScreenIndicators) -> String {
    let mut parts = String::new();
    if indicators.above {
        parts.push('^');
    }
    if indicators.below {
        parts.push('v');
    }
    if indicators.left {
        parts.push('<');
    }
    if indicators.right {
        parts.push('>');
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts)
    }
}

/// Renders the piano roll editor.
///
/// # Returns
///
/// The time ruler region for mouse hit testing, or None if too small to render.
pub fn render_piano_roll(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) -> Option<Rect> {
    let track_notes = app.selected_track().map(|t| t.notes()).unwrap_or(&[]);
    let zoom = app.zoom.max(1);

    // Rough estimate of the visible window for the title indicators.
    let estimated_grid_width = area.width.saturating_sub(7); // 5 piano + 2 borders
    let estimated_visible_ticks = zoom as u64 * estimated_grid_width as u64;
    let estimated_visible_pitches = area.height.saturating_sub(3).min(127) as u8;
    let indicators =
        OffScreenIndicators::calculate(track_notes, app.scroll_x, app.scroll_y, estimated_visible_ticks, estimated_visible_pitches);

    let track_name = app.selected_track().map(|t| t.name.as_str()).unwrap_or("No Track");
    let instrument_name = app.selected_track().map(|t| app.get_instrument_name(t.program)).unwrap_or("Unknown");
    let title = format!(
        "Piano Roll - {} ({}){}",
        track_name,
        instrument_name,
        build_title_indicator(&indicators)
    );

    let inner = panel(frame, area, &title, pal, focused);

    if inner.width < 10 || inner.height < 6 {
        return None; // Too small to render (need room for ruler + at least 5 pitch rows)
    }

    // Layout: [piano keys (5 cols)] [time ruler + grid]
    let piano_width = 5u16;
    let grid_width = inner.width.saturating_sub(piano_width);
    let ruler_height = 1u16;
    let grid_height = inner.height.saturating_sub(ruler_height);
    let visible_pitches = grid_height.min(127) as u8;
    let visible_ticks = zoom as u64 * grid_width as u64;
    let top_pitch = (app.scroll_y as u16 + visible_pitches as u16).saturating_sub(1).min(127);

    let buf = frame.buffer_mut();
    let ruler_rect = Rect::new(inner.x + piano_width, inner.y, grid_width, ruler_height);
    super::render_time_ruler(buf, ruler_rect, app.scroll_x, zoom, pal);
    put(buf, inner.x, inner.y, "     ", Style::default().bg(Color::Rgb(20, 20, 20)), piano_width);

    let indicators = OffScreenIndicators::calculate(track_notes, app.scroll_x, app.scroll_y, visible_ticks, visible_pitches);
    let indicator_style = Style::default().fg(Color::Yellow).bg(Color::Rgb(60, 50, 0)).add_modifier(Modifier::BOLD);

    // Only notes overlapping the visible window matter for the grid.
    let window_end = app.scroll_x.saturating_add(visible_ticks.min(u32::MAX as u64) as u32);
    let visible_notes: Vec<&Note> = track_notes
        .iter()
        .filter(|n| n.end_tick() > app.scroll_x && n.start_tick < window_end)
        .collect();

    let playing = app.audio.is_playing();
    let display_pos = app.display_position_ticks();
    let insert_indicator_tick = if app.edit_mode == EditMode::Insert {
        Some(app.get_insert_indicator_tick().unwrap_or(app.cursor_tick))
    } else {
        None
    };
    let clip = Rect::new(inner.x + piano_width, inner.y + ruler_height, grid_width, grid_height);

    for row in 0..grid_height {
        let pitch = top_pitch.saturating_sub(row) as u8;
        let y = inner.y + ruler_height + row;
        let is_top_row = row == 0;
        let is_bottom_row = row == grid_height - 1;

        // Note name label (piano key column)
        let note_name = note_to_name(pitch);
        let is_black_key = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
        let is_c = pitch.is_multiple_of(12);
        let show_key_indicator = (is_top_row && indicators.above) || (is_bottom_row && indicators.below);
        let key_style = if show_key_indicator {
            indicator_style
        } else if pitch == app.cursor_pitch {
            pal.selected()
        } else if is_black_key {
            Style::default().bg(Color::DarkGray).fg(Color::White)
        } else if is_c {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default().bg(Color::Gray).fg(Color::Black)
        };
        let key_text = if is_top_row && indicators.above {
            format!("{:>3}^ ", note_name)
        } else if is_bottom_row && indicators.below {
            format!("{:>3}v ", note_name)
        } else {
            format!("{:>4} ", note_name)
        };
        put(buf, inner.x, y, &key_text, key_style, piano_width);

        let row_notes: Vec<&&Note> = visible_notes.iter().filter(|n| n.pitch == pitch).collect();

        for col in 0..grid_width {
            let tick = app.scroll_x + (col as u32 * zoom);
            let is_left_col = col == 0;
            let is_right_col = col == grid_width - 1;

            let is_cursor = tick / zoom == app.cursor_tick / zoom && pitch == app.cursor_pitch;
            let is_beat = contains_beat(tick, zoom);
            let is_measure = contains_measure(tick, zoom);
            let is_playhead = playing && tick / zoom == app.cursor_tick / zoom;
            let is_insert_indicator = insert_indicator_tick.is_some_and(|t| tick / zoom == t / zoom);

            let note_here = row_notes.iter().find(|n| n.start_tick <= tick && n.end_tick() > tick);

            let (ch, style) = if let Some(note) = note_here {
                let is_start = note.start_tick <= tick && note.start_tick + zoom > tick;
                let is_selected = app.selected_notes.contains(&note.id);
                let is_recently_added = app.is_recently_added_note(note.id, note.start_tick);
                let is_note_active = note.start_tick <= display_pos && note.end_tick() > display_pos;
                let should_highlight = is_note_active && playing && app.highlight_piano_roll();

                // Priority: insert indicator > playback highlight > recently added > selected > cursor > default
                let bg = if is_insert_indicator {
                    Color::Red
                } else if should_highlight {
                    Color::White
                } else if is_recently_added {
                    Color::Blue
                } else if is_selected {
                    Color::Magenta
                } else if is_cursor {
                    Color::Cyan
                } else {
                    Color::Green
                };
                (if is_start { '[' } else { '=' }, Style::default().fg(Color::Black).bg(bg))
            } else if is_insert_indicator {
                ('|', Style::default().fg(Color::Red).bg(Color::Rgb(40, 40, 40)).add_modifier(Modifier::BOLD))
            } else if is_cursor {
                ('_', Style::default().fg(Color::Cyan).bg(Color::DarkGray))
            } else if is_playhead {
                ('|', Style::default().fg(Color::Red).bg(Color::Rgb(40, 40, 40)).add_modifier(Modifier::BOLD))
            } else if is_left_col && indicators.left {
                ('<', indicator_style)
            } else if is_right_col && indicators.right {
                ('>', indicator_style)
            } else if (is_top_row && indicators.above) || (is_bottom_row && indicators.below) {
                (if is_top_row && indicators.above { '^' } else { 'v' }, indicator_style)
            } else {
                let bg = if is_black_key { Color::Rgb(30, 30, 30) } else { Color::Rgb(40, 40, 40) };
                let (ch, fg) = if is_measure {
                    ('|', Color::White)
                } else if is_beat {
                    (':', Color::DarkGray)
                } else {
                    ('.', Color::Rgb(60, 60, 60))
                };
                (ch, Style::default().fg(fg).bg(bg))
            };
            put_cell(buf, clip, clip.x + col, y, ch, style);
        }
    }

    Some(ruler_rect)
}
