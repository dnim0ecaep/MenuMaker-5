//! Timeline and transport controls rendering.
//!
//! Displays the current position, tempo, time signature, and playback status.

use super::panel;
use crate::dmui::Palette;
use crate::midi::app::{App, EditMode};
use crate::midi::audio::PlaybackState;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Renders the timeline/transport bar at the top of the screen.
pub fn render_timeline(frame: &mut Frame, area: Rect, app: &App, focused: bool, pal: &Palette) {
    let inner = panel(frame, area, "Transport", pal, focused);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let base = pal.fill();
    let dim = Style::default().fg(pal.accent);
    let strong = Style::default().fg(pal.text).add_modifier(Modifier::BOLD);

    // Divide into sections
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(20), // Playback controls
            Constraint::Length(20), // Position
            Constraint::Length(15), // Tempo
            Constraint::Length(10), // Time sig
            Constraint::Min(20),    // Status/mode
        ])
        .split(inner);

    // Playback controls
    let play_status = match app.audio.playback_state() {
        PlaybackState::Playing => Span::styled(" [>] PLAY ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        PlaybackState::Paused => Span::styled(" [||] PAUSE ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        PlaybackState::Stopped => Span::styled(" [.] STOP ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
    };
    frame.render_widget(Paragraph::new(Line::from(play_status)).style(base), chunks[0]);

    // Position display (measure:beat:tick)
    let position_widget = Paragraph::new(Line::from(vec![
        Span::styled("Pos: ", dim),
        Span::styled(app.position_string(), strong),
    ]))
    .style(base);
    frame.render_widget(position_widget, chunks[1]);

    // Tempo display
    let tempo_widget = Paragraph::new(Line::from(vec![
        Span::styled("BPM: ", dim),
        Span::styled(format!("{}", app.project().tempo), Style::default().fg(pal.text)),
    ]))
    .style(base);
    frame.render_widget(tempo_widget, chunks[2]);

    // Time signature
    let time_sig = format!("{}/{}", app.project().time_sig_numerator, app.project().time_sig_denominator);
    frame.render_widget(Paragraph::new(Span::styled(time_sig, Style::default().fg(pal.text))).style(base), chunks[3]);

    // Status message or mode indicator
    let status_line = if let Some((msg, _)) = &app.status_message {
        Line::from(Span::styled(msg.as_str(), Style::default().fg(pal.accent).add_modifier(Modifier::ITALIC)))
    } else {
        let (mode_str, mode_color) = match app.edit_mode {
            EditMode::Normal => ("NORMAL", Color::Blue),
            EditMode::Insert => ("INSERT", Color::Green),
            EditMode::Select => ("SELECT", Color::Magenta),
        };
        Line::from(Span::styled(
            format!("-- {} --", mode_str),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ))
    };
    frame.render_widget(Paragraph::new(status_line).style(base), chunks[4]);
}
