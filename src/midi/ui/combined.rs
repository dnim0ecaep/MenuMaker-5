//! Combined view rendering.
//!
//! Displays both the Piano Roll and Project Timeline simultaneously,
//! split horizontally. The Project Timeline uses a compact label width to
//! align with the Piano Roll's key column for visual consistency of
//! playhead indicators.

use super::{render_piano_roll, render_project_timeline_compact};
use crate::dmui::Palette;
use crate::midi::app::App;
use ratatui::layout::Rect;
use ratatui::Frame;

/// Renders the combined view with Piano Roll on top and Project Timeline below.
///
/// Both views share the same horizontal scroll position (`scroll_x`).
///
/// # Returns
///
/// A tuple of (piano_roll_ruler, project_timeline_ruler) regions for mouse hit testing.
pub fn render_combined(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) -> (Option<Rect>, Option<Rect>) {
    // 55% for the Piano Roll (note editing), the rest for the Project
    // Timeline; computed exactly like the hit-test layout in `calculate_layout`.
    let top = (area.height as u32 * 55 / 100) as u16;
    let chunks = [
        Rect { height: top, ..area },
        Rect { y: area.y + top, height: area.height - top, ..area },
    ];

    let piano_roll_ruler = render_piano_roll(frame, chunks[0], app, focused, pal);
    let project_timeline_ruler = render_project_timeline_compact(frame, chunks[1], app, focused, pal);

    (piano_roll_ruler, project_timeline_ruler)
}
