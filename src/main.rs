use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Margin};
use ratatui::prelude::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod box_style;
use box_style::BoxStyle;
mod boxes;
mod calendar;
mod calculator;
mod contacts;
mod dmui;
mod glyph_icon;
mod paint;
mod midi;
mod pages;
mod rss;
mod spreadsheet;
mod todo;
mod weather;
use rss::RssApp;
use spreadsheet::SpreadsheetApp;
use boxes::{BoxKind, NotesEditor, NotesKeyResult};
use calendar::CalendarApp;
use calculator::Calculator;
use contacts::ContactsApp;
use dmui::{fit_width, pad_width, Palette};
use todo::TodoApp;
use weather::WeatherApp;
use glyph_icon::{GlyphIcon, GlyphPickResult, GlyphPicker};
use midi::{MidiApp, MidiResult};
use paint::{FilePicker, PaintApp, PaintResult, PickResult, Picture};
use pages::{PageBar, PageBarAction, PageBarStyles};
use std::cell::RefCell;
use std::rc::Rc;


/// The Folder-box app value that opens files in the built-in Paint.
const BUILTIN_PAINT_APP: &str = "@paint";
/// The Folder-box app value that opens files in the built-in music app.
const BUILTIN_MIDI_APP: &str = "@midi";

/// The Notes editor's shortcut bar (clickable).
const NOTES_SHORTCUTS: &[(&str, &str)] = &[
    ("Esc", "Save & Close"),
    ("^s", "Save & Close"),
    ("^x", "Discard Changes"),
];

const MAX_COLUMNS: u16 = 6;
const CUSTOM_THEME_KEY: &str = "custom";
const SAVED_THEME_PREFIX: &str = "saved:";

fn main() -> Result<()> {
    let mut app = AppState::new()?;
    run_app(&mut app)
}

fn run_app(app: &mut AppState) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let result = event_loop(&mut terminal, app);

    restore_terminal(&mut terminal)?;
    result
}

fn event_loop<B>(terminal: &mut Terminal<B>, app: &mut AppState) -> Result<()>
where
    B: ratatui::backend::Backend + Write,
{
    loop {
        // The music app animates playback, so it gets a faster loop and a tick.
        let music_open = matches!(app.active_popup, Some(PopupState::Midi(_)));
        let tick_rate = Duration::from_millis(if music_open { 30 } else { 200 });
        if let Some(PopupState::Midi(music)) = app.active_popup.as_mut() {
            music.get_mut().tick();
        }
        // The RSS reader syncs feeds on background threads.
        if let Some(PopupState::Rss(reader)) = app.active_popup.as_mut() {
            reader.poll();
        }
        // So does the Weather box (and it refreshes after 10 minutes).
        if let Some(PopupState::Weather(weather)) = app.active_popup.as_mut() {
            weather.poll();
        }
        terminal.draw(|frame| render(frame, app))?;

        if event::poll(tick_rate)? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key),
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    app.handle_mouse(mouse, size);
                }
                Event::Resize(_, _) => {}
                Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
            };
        }

        if let Some(pending) = app.take_pending_command() {
            match run_command(terminal, &pending) {
                Ok(code) => {
                    let status_text = match code {
                        Some(value) => value.to_string(),
                        None => "unknown".to_string(),
                    };
                    app.set_status(Some(format!("Command exited with status {status_text}")));
                }
                Err(err) => app.set_status(Some(format!("Command failed: {err}"))),
            }
        }

        if let Some(action) = app.take_pending_action() {
            app.execute_deferred_action(terminal, action)?;
        }

        if app.should_quit {
            // Keeps what each Paint box was last showing for next time.
            let _ = app.save_menu();
            break;
        }
    }
    Ok(())
}

fn restore_terminal<B>(terminal: &mut Terminal<B>) -> Result<()>
where
    B: ratatui::backend::Backend + Write,
{
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn render(frame: &mut Frame, app: &AppState) {
    let size = frame.size();
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.background)),
        size,
    );

    let areas = screen_areas(size, app.pages_enabled);

    let header = Paragraph::new(app.title.clone())
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(app.theme.text)
                .bg(app.theme.primary)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(header, areas.title);

    let shortcuts_bg = match app.theme.extras {
        Some(extras) => extras.bar_bg,
        None => color_from_hex("#76B3C5").unwrap_or(app.theme.highlight),
    };
    let shortcuts = Paragraph::new(app.footer_line())
        .alignment(Alignment::Center)
        .style(Style::default().bg(shortcuts_bg));
    frame.render_widget(shortcuts, areas.shortcuts);

    // Pages: the tab bar for switching desktops, under the shortcuts.
    if let Some(page_area) = areas.pages {
        let bar = app.page_bar(page_area.width);
        frame.render_widget(
            Paragraph::new(bar.line).style(Style::default().bg(app.theme.surface)),
            page_area,
        );
    }

    // The box area is the "desktop": theme background, with boxes drawn on
    // the surface color like DeskMate's panels on its teal desktop.
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.background)),
        areas.desktop,
    );
    let desktop = boxes_area(size, app.pages_enabled);
    render_columns(frame, desktop, app);
    if app.pages_enabled && app.categories.is_empty() && desktop.height > 0 {
        let name = &app.pages[app.current_page].name;
        let hint = Paragraph::new(format!(
            "\"{name}\" has no boxes yet - press a to add one, or [ ] to switch pages"
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(app.theme.text).bg(app.theme.background));
        frame.render_widget(
            hint,
            Rect {
                y: desktop.y + desktop.height / 2,
                height: 1,
                ..desktop
            },
        );
    }
    render_box_drag(frame, app);

    let status = Paragraph::new(app.status_text())
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .bg(app.theme.primary)
                .fg(app.theme.text)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(status, areas.status);

    if let Some(popup) = &app.active_popup {
        render_popup(frame, popup, app);
    }
}

/// Geometry for one box on the desktop. Shared by `render_columns` (drawing)
/// and `box_hit_at` (mouse hit-test) so the two can never desync.
struct BoxLayout {
    category_index: usize,
    outer: Rect,
    /// (display_entries index, clickable Rect). The first entry is always the
    /// box itself (its Rect is the title / top-border row); the rest are the
    /// visible rows in order.
    rows: Vec<(usize, Rect)>,
    /// Non-selectable text inside the box (notes, calendar, placeholders),
    /// with an optional highlighted (start, len) character range.
    text_lines: Vec<(Rect, String, Option<(usize, usize)>)>,
    /// Paint boxes: the drawing shown under the list, and where.
    picture: Option<(Rect, Rc<Picture>)>,
    more_above: bool,
    more_below: bool,
}

/// What a mouse click inside a box landed on.
enum BoxHit {
    /// The title row (top line) of a box: click, or drag to another column.
    Title(usize),
    /// Another part of a box's frame.
    Border(usize),
    /// A selectable row.
    Row(usize),
    /// The body of a box that has no selectable rows (notes, shortcut).
    Body(usize),
    /// A button on a calculator box: (box entry, category, button label).
    CalcButton(usize, usize, &'static str),
}

/// The rows of the main screen, top to bottom.
struct ScreenAreas {
    title: Rect,
    shortcuts: Rect,
    /// The page bar, only while Pages is on in Settings.
    pages: Option<Rect>,
    desktop: Rect,
    status: Rect,
}

/// Split the screen into its bars and the desktop. Drawing and mouse
/// hit-testing both use this, so they always agree.
fn screen_areas(terminal_area: Rect, page_bar: bool) -> ScreenAreas {
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    if page_bar {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    constraints.push(Constraint::Length(1));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(terminal_area);
    let offset = usize::from(page_bar);
    ScreenAreas {
        title: chunks[0],
        shortcuts: chunks[1],
        pages: page_bar.then(|| chunks[2]),
        desktop: chunks[2 + offset],
        status: chunks[3 + offset],
    }
}

/// The desktop area that holds the boxes, inside the title/shortcut/page/
/// status bars.
fn boxes_area(terminal_area: Rect, page_bar: bool) -> Rect {
    screen_areas(terminal_area, page_bar).desktop.inner(&Margin {
        vertical: 1,
        horizontal: 1,
    })
}

/// Split the desktop into columns, leaving a one-cell gutter between them.
fn column_areas(area: Rect, column_count: u16) -> Vec<Rect> {
    let column_count = column_count.max(1);
    let constraints = (0..column_count)
        .map(|_| Constraint::Ratio(1, column_count as u32))
        .collect::<Vec<_>>();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);
    let last = chunks.len().saturating_sub(1);
    chunks
        .iter()
        .enumerate()
        .map(|(idx, chunk)| {
            let mut chunk = *chunk;
            if idx < last && chunk.width > 1 {
                chunk.width -= 1;
            }
            chunk
        })
        .collect()
}

/// While a box is dragged, a copy of its title band follows the pointer.
fn render_box_drag(frame: &mut Frame, app: &AppState) {
    let Some(drag) = app.box_drag.as_ref().filter(|d| d.moved) else {
        return;
    };
    let screen = frame.size();
    let name = &app.categories[drag.category_index].name;
    let band = match drag.target_before {
        Some(before) => format!(
            " ◄ {name} ► → column {}, above \"{}\" ",
            drag.target_column, app.categories[before].name
        ),
        None => format!(" ◄ {name} ► → bottom of column {} ", drag.target_column),
    };
    let width = (UnicodeWidthStr::width(band.as_str()) as u16).min(screen.width);
    let x = drag.at.0.saturating_sub(2).min(screen.width.saturating_sub(width));
    let y = drag.at.1.min(screen.height.saturating_sub(1));
    let style = match app.theme.focus_band() {
        Some(band) => Style::default()
            .fg(app.theme.accent)
            .bg(band)
            .add_modifier(Modifier::BOLD),
        None => Style::default()
            .fg(app.theme.selection_text())
            .bg(app.theme.highlight)
            .add_modifier(Modifier::BOLD),
    };
    frame.render_widget(
        Paragraph::new(fit_width(&band, width as usize)).style(style),
        Rect { x, y, width, height: 1 },
    );
}

fn render_columns(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let columns = column_areas(area, app.column_count);
    for box_layout in app.all_box_layouts(&columns) {
        render_box(frame, app, &box_layout);
    }
}

/// The inside of a box, within its frame.
fn box_body(outer: Rect) -> Rect {
    outer.inner(&Margin {
        horizontal: 1,
        vertical: 1,
    })
}

/// A calculator box, drawn like DeskMate's Pocket Calculator: a single-line
/// frame with the name set into the top edge, a blue display and green
/// buttons with black shadows on the gray face.
fn render_calculator_box(
    frame: &mut Frame,
    category: &CategoryState,
    outer: Rect,
    fill: Style,
    border_style: Style,
    box_selected: bool,
    selected_style: Style,
) {
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(border_style)
            .style(fill),
        outer,
    );
    if outer.width > 6 {
        let max = outer.width.saturating_sub(4) as usize;
        let title = fit_width(&format!(" {} ", category.name), max);
        let width = UnicodeWidthStr::width(title.as_str()) as u16;
        let style = if box_selected {
            selected_style
        } else {
            fill.add_modifier(Modifier::BOLD)
        };
        frame.render_widget(
            Paragraph::new(title).style(style),
            Rect {
                x: outer.x + (outer.width - width) / 2,
                y: outer.y,
                width,
                height: 1,
            },
        );
    }
    if category.expanded {
        let face = fill.bg.unwrap_or(Color::Gray);
        calculator::draw(frame.buffer_mut(), box_body(outer), &category.calc.display(), face);
    }
}

/// One of a box's own Custom Theme colors, if it sets that one.
fn custom_color(category: &CategoryState, pick: fn(&ColorConfig) -> &Option<String>) -> Option<Color> {
    category.colors.as_ref().and_then(|c| pick(c).as_deref()).and_then(color_from_hex)
}

/// Draw one DeskMate-style box: a double-line frame (rounded for shortcuts)
/// in the theme accent color, filled with the surface color, and a centered
/// `◄ TITLE ►` band on the top border.
fn render_box(frame: &mut Frame, app: &AppState, box_layout: &BoxLayout) {
    let category = &app.categories[box_layout.category_index];
    let outer = box_layout.outer;

    // Box fill/text: the box's custom colors if set, else the theme.
    let mut fill = Style::default().bg(app.theme.surface).fg(app.theme.text);
    if let Some(colors) = &category.colors {
        if let Some(bg) = colors.background.as_ref().and_then(|hex| color_from_hex(hex)) {
            fill = fill.bg(bg);
        }
        if let Some(text) = colors.text.as_ref().and_then(|hex| color_from_hex(hex)) {
            fill = fill.fg(text);
        }
    }
    let fill_bg = fill.bg.unwrap_or(app.theme.surface);
    let border_color = category
        .colors
        .as_ref()
        .and_then(|colors| colors.border.as_deref())
        .and_then(color_from_hex)
        .unwrap_or(app.theme.accent);
    let border_style = Style::default().fg(border_color).bg(fill_bg);
    let selected_style = Style::default()
        .fg(app.theme.selection_text())
        .bg(app.theme.highlight)
        .add_modifier(Modifier::BOLD);
    let box_selected = box_layout
        .rows
        .first()
        .map(|(entry_index, _)| *entry_index == app.current_index)
        .unwrap_or(false);

    frame.render_widget(Clear, outer);

    let drawn = category.drawn_style(outer.width);
    if !drawn.is_classic() {
        let band_selected = match app.theme.focus_band() {
            Some(band) => Style::default()
                .fg(app.theme.accent)
                .bg(band)
                .add_modifier(Modifier::BOLD),
            None => selected_style,
        };
        let colors = box_style::StyleColors {
            desktop: app.theme.background,
            page: fill_bg,
            ink: fill.fg.unwrap_or(app.theme.text),
            accent: border_color,
            selected: band_selected,
        };
        match drawn {
            BoxStyle::SpiralCalendar => {
                let (title, weeks, today) = calendar::month_view();
                // Today's appointments follow the mini month in the preview.
                let extra: Vec<String> = category
                    .preview
                    .iter()
                    .skip(2 + weeks.len())
                    .map(|(line, _)| line.clone())
                    .filter(|line| !line.trim().is_empty())
                    .collect();
                box_style::draw_spiral_calendar(
                    frame.buffer_mut(),
                    outer,
                    &colors,
                    box_selected,
                    &title,
                    &weeks,
                    Some(today),
                    &extra,
                );
            }
            BoxStyle::TodoPad => {
                let header = format!("{}:", category.name.to_uppercase());
                let items = box_style::todo_items(&category.notes);
                box_style::draw_todo_pad(
                    frame.buffer_mut(),
                    outer,
                    &colors,
                    box_selected,
                    &header,
                    &items,
                );
            }
            BoxStyle::AddressBook => {
                let lines: Vec<(u16, String, String, bool)> = box_layout
                    .rows
                    .iter()
                    .skip(1)
                    .filter_map(|(entry_index, rect)| match app.display_entries.get(*entry_index) {
                        Some(DisplayEntry::Item { item_index, .. }) => Some((
                            rect.y,
                            category.row_label(*item_index),
                            category.phones.get(*item_index).cloned().unwrap_or_default(),
                            *entry_index == app.current_index,
                        )),
                        _ => None,
                    })
                    .collect();
                let lines: Vec<box_style::BookLine> = lines
                    .iter()
                    .map(|(y, name, phone, selected)| box_style::BookLine {
                        y: *y,
                        name,
                        phone,
                        selected: *selected,
                    })
                    .collect();
                // Its own colors unless the box sets some of its own.
                let book = box_style::BookColors::with_box_colors(
                    custom_color(category, |c| &c.border),
                    custom_color(category, |c| &c.background),
                    custom_color(category, |c| &c.text),
                );
                box_style::draw_address_book(
                    frame.buffer_mut(),
                    outer,
                    &book,
                    app.theme.background,
                    box_selected.then_some(colors.selected),
                    &category.name,
                    &lines,
                );
            }
            BoxStyle::MusicPlayer => {
                let songs: Vec<(Rect, String, bool)> = box_layout
                    .rows
                    .iter()
                    .skip(1)
                    .filter_map(|(entry_index, rect)| match app.display_entries.get(*entry_index) {
                        Some(DisplayEntry::Item { item_index, .. }) => Some((
                            *rect,
                            category.row_label(*item_index),
                            *entry_index == app.current_index,
                        )),
                        _ => None,
                    })
                    .collect();
                let songs: Vec<box_style::SongLine> = songs
                    .iter()
                    .map(|(rect, name, selected)| box_style::SongLine {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        name,
                        selected: *selected,
                    })
                    .collect();
                // Its own colors unless the box sets some of its own.
                let player = box_style::PlayerColors::with_box_colors(
                    custom_color(category, |c| &c.border),
                    custom_color(category, |c| &c.background),
                    custom_color(category, |c| &c.text),
                );
                box_style::draw_music_player(
                    frame.buffer_mut(),
                    outer,
                    &player,
                    app.theme.background,
                    box_selected.then_some(colors.selected),
                    &category.name,
                    &songs,
                );
            }
            BoxStyle::Clipboard => {
                let lines: Vec<String> = category.preview.iter().map(|(line, _)| line.clone()).collect();
                let items = box_style::todo_items(&lines.join("\n"));
                let board = box_style::ClipboardColors::with_box_colors(
                    custom_color(category, |c| &c.border),
                    custom_color(category, |c| &c.background),
                    custom_color(category, |c| &c.text),
                );
                box_style::draw_clipboard(
                    frame.buffer_mut(),
                    outer,
                    &board,
                    app.theme.background,
                    box_selected.then_some(colors.selected),
                    &category.name,
                    &items,
                );
            }
            BoxStyle::WeatherPanel => {
                let lines: Vec<String> = category.preview.iter().map(|(line, _)| line.clone()).collect();
                let panel = box_style::WeatherColors::with_box_colors(
                    custom_color(category, |c| &c.border),
                    custom_color(category, |c| &c.background),
                    custom_color(category, |c| &c.text),
                );
                box_style::draw_weather_panel(
                    frame.buffer_mut(),
                    outer,
                    &panel,
                    box_selected.then_some(colors.selected),
                    &category.name,
                    &lines,
                );
            }
            BoxStyle::Classic => {}
        }
        return;
    }

    if category.kind == BoxKind::Calculator {
        render_calculator_box(frame, category, outer, fill, border_style, box_selected, selected_style);
        return;
    }

    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(border_style)
            .style(fill),
        outer,
    );
    let inner_width = outer.width.saturating_sub(2);

    if category.kind == BoxKind::Shortcut && category.name_only {
        // A button: just the name, centered inside the frame.
        if outer.height >= 3 && inner_width > 0 {
            let label = fit_width(&category.name, inner_width as usize);
            let pad = (inner_width as usize).saturating_sub(UnicodeWidthStr::width(label.as_str())) / 2;
            let style = if box_selected {
                selected_style
            } else {
                fill.add_modifier(Modifier::BOLD)
            };
            let row = Rect {
                x: outer.x + 1,
                y: outer.y + outer.height / 2,
                width: inner_width,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(pad_width(&format!("{}{label}", " ".repeat(pad)), inner_width as usize))
                    .style(style),
                row,
            );
        }
        return;
    }

    // Title band centered on the top border, keeping the corner + one rule
    // cell visible on each side.
    if outer.width > 6 {
        let max = outer.width.saturating_sub(4) as usize;
        let band = fit_width(&format!(" ◄ {} ► ", category.name), max);
        let band_width = UnicodeWidthStr::width(band.as_str()) as u16;
        let band_style = if box_selected {
            match app.theme.focus_band() {
                Some(band) => Style::default()
                    .fg(app.theme.accent)
                    .bg(band)
                    .add_modifier(Modifier::BOLD),
                None => selected_style,
            }
        } else {
            Style::default()
                .fg(fill_bg)
                .bg(border_color)
                .add_modifier(Modifier::BOLD)
        };
        let band_rect = Rect {
            x: outer.x + (outer.width - band_width) / 2,
            y: outer.y,
            width: band_width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(band).style(band_style), band_rect);
    }

    if category.kind == BoxKind::Shortcut {
        // The body is the icon picture, or the command it runs.
        let body = Rect {
            x: outer.x + 1,
            y: outer.y + 1,
            width: inner_width,
            height: outer.height.saturating_sub(2),
        };
        match &category.icon_picture {
            Some(picture) => picture.draw(frame.buffer_mut(), body),
            None if category.icon_glyph.is_some() => {
                if let Some(icon) = category.icon_glyph {
                    glyph_icon::draw(icon, border_color, frame.buffer_mut(), body);
                }
            }
            None if body.height > 0 && body.width > 0 => {
                let label = fit_width(&format!("▶ {}", category.app.trim()), body.width as usize);
                let pad = (body.width as usize).saturating_sub(UnicodeWidthStr::width(label.as_str())) / 2;
                let row = Rect {
                    y: body.y + body.height / 2,
                    height: 1,
                    ..body
                };
                frame.render_widget(
                    Paragraph::new(pad_width(&format!("{}{label}", " ".repeat(pad)), body.width as usize))
                        .style(fill),
                    row,
                );
            }
            None => {}
        }
        return;
    }

    for (entry_index, row_rect) in box_layout.rows.iter().skip(1) {
        let (line, style) = app.entry_line(*entry_index);
        let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
        let style = if *entry_index == app.current_index {
            selected_style
        } else {
            style
        };
        if row_rect.height >= 2 {
            // An article: bold title, then the first line of its text.
            let detail = match app.display_entries.get(*entry_index) {
                Some(DisplayEntry::Item { item_index, .. }) => category.row_detail(*item_index),
                _ => String::new(),
            };
            let width = row_rect.width as usize;
            let title = pad_width(&fit_width(&text, width), width);
            let detail = pad_width(&fit_width(&format!("  {detail}"), width), width);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(title, style.add_modifier(Modifier::BOLD))),
                    Line::from(Span::styled(detail, style.remove_modifier(Modifier::BOLD))),
                ]),
                *row_rect,
            );
            continue;
        }
        frame.render_widget(
            Paragraph::new(pad_width(&text, row_rect.width as usize)).style(style),
            *row_rect,
        );
    }

    let placeholder = category.placeholder().is_some();
    for (rect, text, mark) in &box_layout.text_lines {
        let style = if placeholder {
            fill.add_modifier(Modifier::ITALIC)
        } else {
            fill
        };
        let line = pad_width(&format!(" {text}"), rect.width as usize);
        let spans = match mark {
            // Highlight e.g. today's date in the calendar preview (+1 for the
            // leading space).
            Some((start, len)) => {
                let chars: Vec<char> = line.chars().collect();
                let (a, b) = ((start + 1).min(chars.len()), (start + 1 + len).min(chars.len()));
                vec![
                    Span::styled(chars[..a].iter().collect::<String>(), style),
                    Span::styled(chars[a..b].iter().collect::<String>(), selected_style),
                    Span::styled(chars[b..].iter().collect::<String>(), style),
                ]
            }
            None => vec![Span::styled(line, style)],
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), *rect);
    }

    if let Some((area, picture)) = &box_layout.picture {
        picture.draw(frame.buffer_mut(), *area);
    }

    // Scroll markers on the right border, like DeskMate's list arrows.
    let right = outer.x + outer.width.saturating_sub(1);
    if box_layout.more_above && outer.height > 2 {
        frame.render_widget(
            Paragraph::new("▲").style(border_style),
            Rect { x: right, y: outer.y + 1, width: 1, height: 1 },
        );
    }
    if box_layout.more_below && outer.height > 2 {
        frame.render_widget(
            Paragraph::new("▼").style(border_style),
            Rect { x: right, y: outer.y + outer.height - 2, width: 1, height: 1 },
        );
    }
}

fn render_popup(frame: &mut Frame, popup: &PopupState, app: &AppState) {
    match popup {
        PopupState::Info(info) => {
            let area = centered_rect(frame.size(), 60, 40);
            frame.render_widget(Clear, area);
            let text = format!(
                "Label: {}\nCommand: {}\nCategory: {}\nDescription: {}\n\nPress Enter or Esc to close.",
                info.label, info.command, info.category, info.description
            );
            let block = Paragraph::new(text)
                .style(Style::default().bg(app.theme.surface).fg(app.theme.text))
                .block(
                    Block::default()
                        .title("Item Info")
                        .borders(Borders::ALL)
                        .style(Style::default().bg(app.theme.surface)),
                );
            frame.render_widget(block, area);
        }
        PopupState::Message(msg) => {
            let area = centered_rect(frame.size(), 50, 30);
            frame.render_widget(Clear, area);
            let block = Paragraph::new(format!("{msg}\n\nPress Enter or Esc to close."))
                .style(Style::default().bg(app.theme.surface).fg(app.theme.text))
                .block(
                    Block::default()
                        .title("Message")
                        .borders(Borders::ALL)
                        .style(Style::default().bg(app.theme.surface)),
                );
            frame.render_widget(block, area);
        }
        PopupState::ItemForm(form) => {
            let area = frame.size();
            frame.render_widget(Clear, area);
            render_item_form_popup(frame, area, app, form);
        }
        PopupState::CategoryForm(form) => {
            let area = frame.size();
            frame.render_widget(Clear, area);
            render_category_form_popup(frame, area, app, form);
            if let Some(picker) = &form.icon_picker {
                picker.borrow_mut().render(frame, area, &app.palette());
            }
            if let Some(picker) = &form.folder_picker {
                picker.borrow_mut().render(frame, area, &app.palette());
            }
            if let Some(picker) = &form.glyph_picker {
                picker.render(frame, area, &app.palette(), app.theme.accent);
            }
        }
        PopupState::SettingsForm(form) => {
            let area = frame.size();
            frame.render_widget(Clear, area);
            render_settings_form_popup(frame, area, app, form);
        }
        PopupState::Notes(editor) => {
            let area = frame.size();
            frame.render_widget(Clear, area);
            render_notes_popup(frame, area, app, editor);
        }
        PopupState::Contacts(contacts_app) => {
            contacts_app.render(frame, frame.size(), &app.title, &app.palette());
        }
        PopupState::Calendar(calendar_app) => {
            calendar_app.render(frame, frame.size(), &app.title, &app.palette());
        }
        PopupState::Spreadsheet(sheet_app) => {
            sheet_app.render(frame, frame.size(), &app.title, &app.palette());
        }
        PopupState::Rss(rss_app) => {
            rss_app.render(frame, frame.size(), &app.title, &app.palette());
        }
        PopupState::Weather(weather_app) => {
            weather_app.render(frame, frame.size(), &app.title, &app.palette());
        }
        PopupState::Todo(todo_app) => {
            todo_app.render(frame, frame.size(), &app.title, &app.palette());
        }
        PopupState::Midi(music_app) => {
            let area = frame.size();
            music_app
                .borrow_mut()
                .render(frame, area, &app.title, &app.palette());
        }
        PopupState::Paint(paint_app) => {
            let area = frame.size();
            paint_app
                .borrow_mut()
                .render(frame, area, &app.title, &app.palette());
        }
        PopupState::Confirm(confirm) => {
            let area = centered_rect(frame.size(), 50, 30);
            frame.render_widget(Clear, area);
            let text = format!("{}\n\ny / Enter = Yes      n / Esc = No", confirm.message);
            let block = Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .alignment(Alignment::Center)
                .style(Style::default().bg(app.theme.surface).fg(app.theme.text))
                .block(
                    Block::default()
                        .title(" Confirm ")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Double)
                        .border_style(Style::default().fg(app.theme.accent))
                        .style(Style::default().bg(app.theme.surface)),
                );
            frame.render_widget(block, area);
        }
    }
}

/// Full-screen editor for a Notes box, laid out like the other forms.
fn render_notes_popup(frame: &mut Frame, area: Rect, app: &AppState, editor: &NotesEditor) {
    let Some([header_area, shortcuts_area, content_area, status_area]) = popup_sections(area)
    else {
        return;
    };
    let bar = Style::default()
        .bg(app.theme.primary)
        .fg(app.theme.text)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(format!("{} - Notes: {}", app.title, editor.box_name))
            .alignment(Alignment::Center)
            .style(bar),
        header_area,
    );
    let key_style = Style::default()
        .fg(app.theme.bar_key())
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.theme.bar_label());
    let mut chips = Vec::new();
    for (idx, (key, label)) in NOTES_SHORTCUTS.iter().enumerate() {
        if idx > 0 {
            chips.push(Span::styled(" | ", label_style));
        }
        chips.push(Span::styled(*key, key_style));
        chips.push(Span::styled(format!(" {label}"), label_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(chips))
        .alignment(Alignment::Center)
        .style(Style::default().bg(app.theme.bar_bg())),
        shortcuts_area,
    );

    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(app.theme.accent))
            .style(Style::default().bg(app.theme.surface).fg(app.theme.text)),
        content_area,
    );
    let text_area = notes_text_area(content_area);
    let (width, height) = (text_area.width as usize, text_area.height as usize);
    editor.view.set((width, height));
    let lines: Vec<Line> = editor
        .lines
        .iter()
        .skip(editor.scroll_row)
        .take(height)
        .map(|line| Line::from(line.iter().skip(editor.scroll_col).take(width).collect::<String>()))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.theme.surface).fg(app.theme.text)),
        text_area,
    );
    if editor.row >= editor.scroll_row
        && editor.row < editor.scroll_row + height
        && editor.col >= editor.scroll_col
        && editor.col < editor.scroll_col + width
    {
        frame.set_cursor(
            text_area.x + (editor.col - editor.scroll_col) as u16,
            text_area.y + (editor.row - editor.scroll_row) as u16,
        );
    }

    frame.render_widget(
        Paragraph::new(format!(
            "Line {}, Col {}",
            editor.row + 1,
            editor.col + 1
        ))
        .alignment(Alignment::Center)
        .style(bar),
        status_area,
    );
}

fn notes_text_area(content_area: Rect) -> Rect {
    content_area.inner(&Margin {
        horizontal: 2,
        vertical: 1,
    })
}

fn popup_sections(area: Rect) -> Option<[Rect; 4]> {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    if sections.len() < 4 {
        return None;
    }
    Some([sections[0], sections[1], sections[2], sections[3]])
}

fn popup_content_margin() -> Margin {
    Margin {
        horizontal: 3,
        vertical: 1,
    }
}

fn render_category_form_popup(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    form: &CategoryFormState,
) {
    let (lines, layout) = form.render_lines(app);
    if let Some(sections) = popup_sections(area) {
        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.background)),
            area,
        );
        let [header_area, shortcuts_area, content_area, status_area] = sections;
        let header_label = if form.category_index.is_some() {
            "Edit Box"
        } else {
            "New Box"
        };
        let header = Paragraph::new(format!("{} - {header_label}", app.title))
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.primary)
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(header, header_area);

        let shortcut_line = layout
            .shortcut_line
            .clone()
            .unwrap_or_else(|| Line::from(""));
        let shortcuts = Paragraph::new(shortcut_line)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.bar_bg())
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(shortcuts, shortcuts_area);

        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.surface)),
            content_area,
        );
        let inner = content_area.inner(&popup_content_margin());
        let rendered_lines = materialize_form_lines(&lines, inner.width as usize, app);
        let paragraph = Paragraph::new(rendered_lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(app.theme.surface).fg(app.theme.text));
        frame.render_widget(paragraph, inner);

        let status = Paragraph::new(app.status_text())
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.primary)
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(status, status_area);
    }
}

fn render_item_form_popup(frame: &mut Frame, area: Rect, app: &AppState, form: &ItemFormState) {
    let (lines, layout) = form.render_lines(app);
    let shortcut_line = layout
        .shortcut_line
        .clone()
        .unwrap_or_else(|| Line::from(""));

    if let Some(sections) = popup_sections(area) {
        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.background)),
            area,
        );
        let [header_area, shortcuts_area, content_area, status_area] = sections;
        let header = Paragraph::new(format!("{} - {}", app.title, form.mode_label))
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.primary)
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(header, header_area);

        let shortcuts = Paragraph::new(shortcut_line)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.bar_bg())
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(shortcuts, shortcuts_area);

        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.surface)),
            content_area,
        );
        let inner = content_area.inner(&popup_content_margin());
        let rendered_lines = materialize_form_lines(&lines, inner.width as usize, app);
        let paragraph = Paragraph::new(rendered_lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(app.theme.surface).fg(app.theme.text));
        frame.render_widget(paragraph, inner);

        let status = Paragraph::new(app.status_text())
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.primary)
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(status, status_area);
    }
}

fn render_settings_form_popup(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    form: &SettingsFormState,
) {
    let (lines, layout) = form.render_lines(app);
    if let Some(sections) = popup_sections(area) {
        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.background)),
            area,
        );
        let [header_area, shortcuts_area, content_area, status_area] = sections;
        let header = Paragraph::new(format!("{} - Application Settings", app.title))
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.primary)
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(header, header_area);

        let shortcut_line = layout
            .shortcut_line
            .clone()
            .unwrap_or_else(|| Line::from(""));
        let shortcuts = Paragraph::new(shortcut_line)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.bar_bg())
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(shortcuts, shortcuts_area);

        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.surface)),
            content_area,
        );
        let inner = content_area.inner(&popup_content_margin());
        let rendered_lines = materialize_form_lines(&lines, inner.width as usize, app);
        let paragraph = Paragraph::new(rendered_lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(app.theme.surface).fg(app.theme.text));
        frame.render_widget(paragraph, inner);

        let status = Paragraph::new(app.status_text())
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .bg(app.theme.primary)
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(status, status_area);
    }
}

fn build_category_shortcut_line(
    app: &AppState,
    include_delete: bool,
) -> (Line<'static>, Vec<CategoryShortcutSegment>, u16) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut segments: Vec<CategoryShortcutSegment> = Vec::new();
    let mut cursor: u16 = 0;
    let key_style = Style::default()
        .fg(app.theme.bar_key())
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.theme.bar_label());
    let entries: Vec<(&str, &str, CategoryShortcutAction)> = vec![
        ("Tab", " Move", CategoryShortcutAction::NextField),
        ("↵", " Save", CategoryShortcutAction::Submit),
        ("Esc", " Cancel", CategoryShortcutAction::Cancel),
    ];

    for (idx, (key, label, action)) in entries.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" | ", label_style));
            cursor = cursor.saturating_add(3);
        }
        let entry_start = cursor;
        spans.push(Span::styled(*key, key_style));
        cursor = cursor.saturating_add(key.chars().count() as u16);
        if !label.is_empty() {
            spans.push(Span::styled(*label, label_style));
            cursor = cursor.saturating_add(label.chars().count() as u16);
        }
        segments.push(CategoryShortcutSegment {
            start: entry_start,
            end: cursor,
            action: *action,
        });
    }

    if !entries.is_empty() {
        spans.push(Span::styled(" | ", label_style));
        cursor = cursor.saturating_add(3);
    }

    let left_start = cursor;
    spans.push(Span::styled("←", key_style));
    cursor = cursor.saturating_add("←".chars().count() as u16);
    segments.push(CategoryShortcutSegment {
        start: left_start,
        end: cursor,
        action: CategoryShortcutAction::PreviousPalette,
    });
    spans.push(Span::styled("/", label_style));
    cursor = cursor.saturating_add(1);
    let right_start = cursor;
    spans.push(Span::styled("→", key_style));
    cursor = cursor.saturating_add("→".chars().count() as u16);
    spans.push(Span::styled(" Select", label_style));
    cursor = cursor.saturating_add(" Select".len() as u16);
    segments.push(CategoryShortcutSegment {
        start: right_start,
        end: cursor,
        action: CategoryShortcutAction::NextPalette,
    });

    if include_delete {
        spans.push(Span::styled(" | ", label_style));
        cursor = cursor.saturating_add(3);
        let entry_start = cursor;
        spans.push(Span::styled("d", key_style));
        cursor = cursor.saturating_add(1);
        spans.push(Span::styled(" Delete Theme", label_style));
        cursor = cursor.saturating_add(" Delete Theme".len() as u16);
        segments.push(CategoryShortcutSegment {
            start: entry_start,
            end: cursor,
            action: CategoryShortcutAction::DeletePreset,
        });
    }

    (Line::from(spans), segments, cursor)
}

#[derive(Clone)]
struct FormLine {
    line: Line<'static>,
    highlight: bool,
}

impl FormLine {
    fn plain(line: Line<'static>) -> Self {
        Self {
            line,
            highlight: false,
        }
    }
    
    fn highlighted(line: Line<'static>) -> Self {
        Self {
            line,
            highlight: true,
        }
    }
}

fn materialize_form_lines(
    lines: &[FormLine],
    width: usize,
    app: &AppState,
) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|form_line| {
            if form_line.highlight {
                highlight_line_with_width(form_line.line.clone(), width, app)
            } else {
                form_line.line.clone()
            }
        })
        .collect()
}

fn highlight_line_with_width(
    mut line: Line<'static>,
    width: usize,
    app: &AppState,
) -> Line<'static> {
    let mut text_width = 0usize;
    let highlight_style = Style::default()
        .fg(app.theme.selection_text())
        .bg(app.theme.highlight)
        .add_modifier(Modifier::BOLD);
    for span in &mut line.spans {
        span.style = highlight_style;
        text_width += UnicodeWidthStr::width(span.content.as_ref());
    }
    if width > text_width {
        line.spans
            .push(Span::styled(" ".repeat(width - text_width), highlight_style));
    }
    line
}

fn plain_line(line: impl Into<Line<'static>>) -> FormLine {
    FormLine::plain(line.into())
}

fn make_action_line(label: &str, selected: bool, app: &AppState) -> FormLine {
    let style = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let line = Line::from(vec![Span::styled(label.to_string(), style)]);
    if selected {
        FormLine::highlighted(line)
    } else {
        FormLine::plain(line)
    }
}

fn make_field_line(label: &str, value: &str, selected: bool, app: &AppState) -> FormLine {
    let value_display = if value.trim().is_empty() {
        "(empty)".to_string()
    } else {
        value.to_string()
    };
    let label_style = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let value_style = Style::default().fg(app.theme.text);
    let label_span = Span::styled(format!("{label}: "), label_style);
    let value_span = Span::styled(value_display, value_style);
    if selected {
        FormLine::highlighted(Line::from(vec![label_span, value_span]))
    } else {
        FormLine::plain(Line::from(vec![label_span, value_span]))
    }
}

fn make_color_field_line(
    label: &str,
    value: &str,
    selected: bool,
    color: Option<Color>,
    app: &AppState,
) -> FormLine {
    let value_display = if value.trim().is_empty() {
        "(empty)".to_string()
    } else {
        value.to_string()
    };
    let label_style = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let value_style = Style::default()
        .fg(color.unwrap_or(app.theme.text))
        .add_modifier(Modifier::BOLD);
    let label_span = Span::styled(format!("{label}: "), label_style);
    let value_span = Span::styled(value_display, value_style);
    if selected {
        FormLine::highlighted(Line::from(vec![label_span, value_span]))
    } else {
        FormLine::plain(Line::from(vec![label_span, value_span]))
    }
}

fn make_toggle_line(label: &str, value: bool, selected: bool, app: &AppState) -> FormLine {
    let status = if value { "Yes" } else { "No" };
    let label_style = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut value_style = Style::default()
        .fg(if value { Color::Green } else { Color::Red })
        .add_modifier(Modifier::BOLD);
    let label_span = Span::styled(format!("{label}: "), label_style);
    let value_span = Span::styled(status, value_style);
    if selected {
        FormLine::highlighted(Line::from(vec![label_span, value_span]))
    } else {
        FormLine::plain(Line::from(vec![label_span, value_span]))
    }
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(horizontal[1]);
    vertical[1]
}

fn with_terminal_suspension<B, F, T>(terminal: &mut Terminal<B>, f: F) -> Result<T>
where
    B: ratatui::backend::Backend + Write,
    F: FnOnce() -> Result<T>,
{
    terminal.show_cursor()?;
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    let result = f();
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    result
}

fn run_command<B>(terminal: &mut Terminal<B>, pending: &PendingCommand) -> Result<Option<i32>>
where
    B: ratatui::backend::Backend + Write,
{
    with_terminal_suspension(terminal, || {
        if pending.clear_screen {
            print!("\x1b[2J\x1b[H");
            io::stdout().flush()?;
        }
        let status = Command::new("sh").arg("-c").arg(&pending.command).status();

        let exit_code = match status {
            Ok(status) => {
                if pending.pause {
                    println!("\nPress Enter to return...");
                    let _ = io::stdin().read_line(&mut String::new());
                }
                status.code()
            }
            Err(err) => {
                println!("Failed to run command: {err}");
                println!("Press Enter to continue...");
                let _ = io::stdin().read_line(&mut String::new());
                None
            }
        };
        Ok(exit_code)
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct NamedColorPair {
    name: Option<String>,
    background: Option<String>,
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    border: Option<String>,
}

/// Name of the box color theme that clears a box's own colors.
const THEME_DEFAULT_PRESET: &str = "Default (current theme)";

/// Built-in box color themes: (name, background, text, border). An empty
/// border keeps the app theme's border color.
const DEFAULT_CATEGORY_COLOR_PRESETS: &[(&str, &str, &str, &str)] = &[
    ("Teal Glow", "#034e68", "#caf0f8", ""),
    ("Amber Pop", "#6f1d1b", "#ffe5d9", ""),
    ("Purple Mist", "#240046", "#f8f9fa", ""),
    ("Forest Tones", "#283618", "#fefae0", ""),
    ("Slate Shine", "#2b2d42", "#edf2f4", ""),
    // Tandy DeskMate 3.05 panels (EGA colors, as in ~/workspace/deskmate).
    // File boxes: navy with yellow text and a yellow double border.
    ("DeskMate Navy", "#00007B", "#BDBD00", "#BDBD00"),
    // The red "Touch Me" tutorial panel.
    ("DeskMate Touch Me", "#7B0000", "#BDBDBD", "#BDBDBD"),
    // The Phone List widget: light gray with black text, navy rules.
    ("DeskMate Phone List", "#BDBDBD", "#000000", "#00007B"),
    // ADDRESS / QUICK LOAD / HANGMAN launch buttons: blue with yellow trim.
    ("DeskMate Button", "#0000BD", "#BDBD00", "#BDBD00"),
];

#[derive(Clone)]
struct ColorPreset {
    name: String,
    background: String,
    text: String,
    /// Box border color; empty = the theme's accent.
    border: String,
    custom_index: Option<usize>,
}

impl ColorPreset {
    fn new(name: impl Into<String>, background: &str, text: &str) -> Self {
        Self {
            name: name.into(),
            background: normalize_hex(background),
            text: normalize_hex(text),
            border: String::new(),
            custom_index: None,
        }
    }

    fn from_custom(
        name: impl Into<String>,
        background: &str,
        text: &str,
        border: Option<&str>,
        index: usize,
    ) -> Self {
        Self {
            name: name.into(),
            background: normalize_hex(background),
            text: normalize_hex(text),
            border: border.map(normalize_hex).unwrap_or_default(),
            custom_index: Some(index),
        }
    }

    fn matches(&self, background: &str, text: &str, border: &str) -> bool {
        hex_strings_equal(&self.background, background)
            && hex_strings_equal(&self.text, text)
            && (hex_strings_equal(&self.border, border)
                || (self.border.trim().is_empty() && border.trim().is_empty()))
    }
}

#[derive(Clone)]
struct ThemeOption {
    key: String,
    label: String,
    primary_hex: String,
    accent_hex: String,
    background_hex: String,
    surface_hex: String,
    text_hex: String,
    highlight_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct AppSettings {
    title: Option<String>,
    columns: Option<u16>,
    #[serde(default)]
    theme_key: Option<String>,
    /// Set once the built-in Address Book and Calendar boxes have been added,
    /// so deleting them is permanent.
    #[serde(default)]
    builtin_boxes_added: bool,
    /// Set once the built-in Paint box has been added.
    #[serde(default)]
    paint_box_added: bool,
    /// Set once the built-in Music box has been added.
    #[serde(default)]
    music_box_added: bool,
    /// Set once the ~/MenuMaker boxes (Programs, Art, Music, Documents,
    /// Spreadsheets) have been added and old data copied there.
    #[serde(default)]
    menumaker_boxes_added: bool,
    /// Set once the built-in Calculator box has been added.
    #[serde(default)]
    calculator_box_added: bool,
    /// Drag a box by its title to another column (default on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drag_boxes: Option<bool>,
    /// Pages: several named desktops, switched from a bar under the
    /// shortcuts. Off = only the first page is shown (the others are kept).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pages_enabled: bool,
    /// Name of the first page (its boxes are `categories`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_page_name: Option<String>,
    /// The page that was showing (0 = the first page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_page: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct SavedTheme {
    name: String,
    primary: String,
    accent: String,
    background: String,
    surface: String,
    text: String,
    #[serde(default)]
    highlight: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct MenuFile {
    categories: BTreeMap<String, CategoryConfig>,
    #[serde(default)]
    app_settings: AppSettings,
    #[serde(default)]
    custom_colors: Vec<NamedColorPair>,
    #[serde(default)]
    saved_themes: Vec<SavedTheme>,
    /// Pages after the first one, each with its own boxes. The first page's
    /// boxes stay in `categories`, so files from before Pages still load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pages: Vec<PageFile>,
}

/// One extra page (desktop) in menus.json.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct PageFile {
    name: String,
    #[serde(default)]
    categories: BTreeMap<String, CategoryConfig>,
}

/// A page (desktop) in memory. The showing page's boxes live in
/// `AppState::categories`; its entry here keeps an empty list until the user
/// switches away.
struct PageState {
    name: String,
    categories: Vec<CategoryState>,
}

impl PageState {
    fn from_file(page: &PageFile) -> Self {
        let mut categories: Vec<CategoryState> = page
            .categories
            .iter()
            .map(|(name, cfg)| CategoryState::from_config(name, cfg))
            .collect();
        categories.sort_by_key(|cat| (cat.column, cat.order, cat.name.clone()));
        Self {
            name: page.name.clone(),
            categories,
        }
    }
}

/// The pages listed in a menus.json: the first one (its boxes are still in
/// `categories`, so its list here is empty) plus the extra ones.
fn pages_from_file(menu_file: &MenuFile) -> Vec<PageState> {
    let first_name = menu_file
        .app_settings
        .first_page_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| pages::DEFAULT_FIRST_PAGE.to_string());
    let mut list = vec![PageState {
        name: first_name,
        categories: Vec::new(),
    }];
    list.extend(menu_file.pages.iter().map(PageState::from_file));
    list
}

fn categories_to_map(categories: &[CategoryState]) -> BTreeMap<String, CategoryConfig> {
    categories
        .iter()
        .map(|category| (category.name.clone(), category.to_config()))
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CategoryConfig {
    #[serde(default = "default_true")]
    expanded: bool,
    column: Option<u16>,
    /// How many columns the box spans horizontally (1 = one column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    span: Option<u16>,
    /// Position of the box within its column (top = 0). Missing = after the
    /// ordered boxes, alphabetically (files saved before boxes could be
    /// reordered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<u32>,
    #[serde(default)]
    items: Vec<MenuItemConfig>,
    #[serde(default)]
    colors: Option<ColorConfig>,
    #[serde(default, skip_serializing_if = "BoxKind::is_menu")]
    kind: BoxKind,
    /// Folder shown by Folder / Apps boxes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    /// Folder boxes: app that opens the files. Shortcut boxes: the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app: Option<String>,
    /// Folder boxes: file patterns such as `*.md, *.txt` (blank = all files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    /// Shortcut boxes: a picture drawn in Paint, shown as the button's icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    /// Shortcut boxes: show only the name, centered inside the box.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    name_only: bool,
    /// Paint boxes: show the selected drawing under the list (default on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    show_picture: Option<bool>,
    /// Paint boxes: the drawing last shown, kept when the cursor leaves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    picture: Option<String>,
    /// Most rows the box shows before scrolling (blank = fit content).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_rows: Option<u16>,
    /// RSS boxes: how many of the newest articles to show (0 = list the
    /// feeds instead; unset = the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    articles: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pause: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clear_screen: Option<bool>,
    /// How the box is drawn (spiral calendar, to-do pad). Unset = the kind's
    /// default (spiral for Calendar boxes, classic for the rest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<BoxStyle>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct ColorConfig {
    background: Option<String>,
    text: Option<String>,
    /// Color of the line around the box and its title band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    border: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct MenuItemConfig {
    label: String,
    cmd: String,
    info: Option<String>,
    category: Option<String>,
    pause: Option<bool>,
    clear_screen: Option<bool>,
}

fn default_true() -> bool {
    true
}

fn default_saved_theme() -> SavedTheme {
    let base_theme = Theme::from_name("nord").unwrap_or_else(|| {
        Theme::from_hexes(
            "default".to_string(),
            "#5E81AC",
            "#D08770",
            "#76B3C5",
            "#3B4252",
            "#4C566A",
            "#ECEFF4",
        )
    });
    SavedTheme {
        name: "default".to_string(),
        primary: base_theme.primary_hex.clone(),
        accent: base_theme.accent_hex.clone(),
        highlight: Some(base_theme.highlight_hex.clone()),
        background: base_theme.background_hex.clone(),
        surface: base_theme.surface_hex.clone(),
        text: base_theme.text_hex.clone(),
    }
}

impl MenuFile {
    fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let data = fs::read_to_string(path)?;
            let parsed: MenuFile = serde_json::from_str(&data)?;
            Ok(parsed)
        } else {
            let default = Self::default_data();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, serde_json::to_string_pretty(&default)?)?;
            Ok(default)
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        fs::write(path, data)?;
        Ok(())
    }

    fn default_data() -> Self {
        let mut categories = BTreeMap::new();
        categories.insert(
            "System Tools".to_string(),
            CategoryConfig {
                expanded: true,
                column: Some(1),
                span: None,
                order: None,
                items: vec![MenuItemConfig {
                    label: "System Monitor".into(),
                    cmd: "htop".into(),
                    info: Some("Interactive process viewer".into()),
                    category: Some("System Tools".into()),
                    pause: Some(false),
                    clear_screen: Some(false),
                }],
                colors: None,
                kind: BoxKind::Menu,
                folder: None,
                app: None,
                filter: None,
                notes: None,
                icon: None,
                name_only: false,
                show_picture: None,
                picture: None,
                max_rows: None,
                articles: None,
                pause: None,
                clear_screen: None,
                style: None,
            },
        );
        let saved_themes = vec![default_saved_theme()];

        MenuFile {
            categories,
            app_settings: AppSettings {
                title: Some("Menu Maker — Enhanced Categorized Menu System".into()),
                columns: Some(1),
                theme_key: Some(saved_theme_key(0)),
                builtin_boxes_added: false,
                paint_box_added: false,
                music_box_added: false,
                menumaker_boxes_added: false,
                calculator_box_added: false,
                drag_boxes: None,
                pages_enabled: false,
                first_page_name: None,
                current_page: None,
            },
            custom_colors: Vec::new(),
            saved_themes,
            pages: Vec::new(),
        }
    }
}

struct AppPaths {
    config_dir: PathBuf,
    menu_file: PathBuf,
    theme_file: PathBuf,
}

impl AppPaths {
    fn new() -> Result<Self> {
        let home = dirs::home_dir().context("Unable to determine home directory")?;
        let config_dir = home.join(".local/menu-maker");
        fs::create_dir_all(&config_dir)?;
        create_menumaker_folders(&menumaker_dir());
        Ok(Self {
            menu_file: config_dir.join("menus.json"),
            theme_file: config_dir.join("theme.json"),
            config_dir,
        })
    }
}

struct AppState {
    categories: Vec<CategoryState>,
    custom_colors: Vec<NamedColorPair>,
    saved_themes: Vec<SavedTheme>,
    column_count: u16,
    current_index: usize,
    display_entries: Vec<DisplayEntry>,
    column_map: Vec<Vec<usize>>,
    should_quit: bool,
    pending_command: Option<PendingCommand>,
    pending_action: Option<DeferredAction>,
    status_message: Option<String>,
    paths: AppPaths,
    theme: Theme,
    theme_key: String,
    title: String,
    active_popup: Option<PopupState>,
    /// A box picked up by its title with the mouse.
    box_drag: Option<BoxDrag>,
    /// Settings: dragging boxes by their title is allowed.
    drag_boxes: bool,
    /// Settings: Pages are on (the page bar shows under the shortcuts).
    pages_enabled: bool,
    /// Every page (desktop); at least one. See `PageState`.
    pages: Vec<PageState>,
    /// Index into `pages` of the page whose boxes are in `categories`.
    current_page: usize,
}

/// A box being dragged by its title to another column.
struct BoxDrag {
    category_index: usize,
    start: (u16, u16),
    /// Last pointer position.
    at: (u16, u16),
    /// The pointer has left the starting cell, so release means "drop".
    moved: bool,
    /// Column (1-based) the box would land in.
    target_column: u16,
    /// The box (category index) it would land above; `None` = the bottom of
    /// the column.
    target_before: Option<usize>,
}

impl AppState {
    fn resolve_theme_key(
        stored: Option<String>,
        theme: &Theme,
        saved_themes: &[SavedTheme],
    ) -> String {
        if let Some(key) = stored {
            if key == CUSTOM_THEME_KEY || is_preset_theme_key(&key) {
                return key;
            }
            if let Some(idx) = parse_saved_theme_key(&key) {
                if idx < saved_themes.len() {
                    return key;
                }
            }
        }
        if let Some(idx) = saved_themes
            .iter()
            .position(|saved| saved.name == theme.name)
        {
            return saved_theme_key(idx);
        }
        if is_preset_theme_key(&theme.name) {
            return theme.name.clone();
        }
        CUSTOM_THEME_KEY.to_string()
    }
    fn new() -> Result<Self> {
        let paths = AppPaths::new()?;
        let mut menu_file = MenuFile::load(&paths.menu_file)?;
        if !menu_file
            .saved_themes
            .iter()
            .any(|saved| saved.name.eq_ignore_ascii_case("default"))
        {
            menu_file.saved_themes.push(default_saved_theme());
            let _ = menu_file.save(&paths.menu_file);
        }
        let theme = Theme::load(&paths.theme_file)?;
        let saved_themes = menu_file.saved_themes.clone();

        let mut categories: Vec<CategoryState> = menu_file
            .categories
            .iter()
            .map(|(name, cfg)| CategoryState::from_config(name, cfg))
            .collect();
        let mut builtin_boxes = Vec::new();
        if !menu_file.app_settings.builtin_boxes_added {
            builtin_boxes.push(("Address Book", BoxKind::Contacts));
            builtin_boxes.push(("Calendar", BoxKind::Calendar));
        }
        // A fresh setup gets the ~/MenuMaker "Art" box instead of a second
        // Paint box showing the same folder.
        if !menu_file.app_settings.paint_box_added && menu_file.app_settings.menumaker_boxes_added {
            builtin_boxes.push(("Paint", BoxKind::Paint));
        }
        if !menu_file.app_settings.music_box_added {
            builtin_boxes.push(("Music", BoxKind::Music));
        }
        let add_menumaker_boxes = !menu_file.app_settings.menumaker_boxes_added;
        if add_menumaker_boxes {
            copy_legacy_data(&paths.config_dir, &menumaker_dir());
            for (name, kind) in MENUMAKER_BOXES {
                builtin_boxes.push((name, kind));
            }
        }
        if !menu_file.app_settings.calculator_box_added {
            builtin_boxes.push(("Pocket Calculator", BoxKind::Calculator));
        }
        let add_builtin_boxes = !builtin_boxes.is_empty();
        if add_builtin_boxes {
            let last_column = menu_file
                .app_settings
                .columns
                .unwrap_or(1)
                .clamp(1, MAX_COLUMNS);
            for (name, kind) in builtin_boxes {
                let menumaker_box = add_menumaker_boxes && MENUMAKER_BOXES.contains(&(name, kind));
                // A ~/MenuMaker box is added even when a box of the same kind
                // exists (that one may point at another folder); only the name
                // must be free.
                let exists = categories.iter().any(|c| {
                    c.name == name || (!menumaker_box && c.kind == kind)
                });
                if !exists {
                    let mut new_box = CategoryState::new_box(name, kind, last_column);
                    if kind == BoxKind::Apps {
                        new_box.folder = format!("~/{MENUMAKER_DIR_NAME}/{APPS_FOLDER}");
                    }
                    if kind == BoxKind::Calculator {
                        // Gray face, black text and frame, like DeskMate's.
                        new_box.colors = Some(ColorConfig {
                            background: Some(calculator::FACE_HEX.into()),
                            text: Some(calculator::INK_HEX.into()),
                            border: Some(calculator::INK_HEX.into()),
                        });
                    }
                    new_box.max_rows = matches!(
                        kind,
                        BoxKind::Contacts
                            | BoxKind::Paint
                            | BoxKind::Music
                            | BoxKind::Apps
                            | BoxKind::Documents
                            | BoxKind::Spreadsheet
                    )
                    .then_some(8);
                    categories.push(new_box);
                }
            }
        }
        categories.sort_by_key(|cat| (cat.column, cat.order, cat.name.clone()));

        let mut column_count = menu_file
            .app_settings
            .columns
            .unwrap_or(1)
            .clamp(1, MAX_COLUMNS);
        if column_count == 0 {
            column_count = 1;
        }

        let stored_theme_key = AppState::resolve_theme_key(
            menu_file.app_settings.theme_key.clone(),
            &theme,
            &saved_themes,
        );
        let resolved_theme = if stored_theme_key == CUSTOM_THEME_KEY {
            theme.clone()
        } else if let Some(idx) = parse_saved_theme_key(&stored_theme_key) {
            saved_themes.get(idx).and_then(|saved| {
                let highlight = saved
                    .highlight
                    .as_deref()
                    .unwrap_or_else(|| saved.accent.as_str());
                Some(Theme::from_hexes(
                    saved.name.clone(),
                    &saved.primary,
                    &saved.accent,
                    highlight,
                    &saved.background,
                    &saved.surface,
                    &saved.text,
                ))
            })
            .unwrap_or_else(|| theme.clone())
        } else if is_preset_theme_key(&stored_theme_key) {
            Theme::from_name(&stored_theme_key).unwrap_or_else(|| theme.clone())
        } else {
            theme.clone()
        };

        let loaded_pages = pages_from_file(&menu_file);
        let mut app = AppState {
            categories,
            custom_colors: menu_file.custom_colors,
            saved_themes,
            column_count,
            current_index: 0,
            display_entries: Vec::new(),
            column_map: Vec::new(),
            should_quit: false,
            pending_command: None,
            pending_action: None,
            status_message: None,
            paths,
            theme_key: stored_theme_key,
            theme: resolved_theme,
            title: menu_file
                .app_settings
                .title
                .unwrap_or_else(|| "Menu Maker".into()),
            active_popup: None,
            box_drag: None,
            drag_boxes: menu_file.app_settings.drag_boxes.unwrap_or(true),
            pages_enabled: menu_file.app_settings.pages_enabled,
            pages: loaded_pages,
            current_page: 0,
        };
        if app.pages_enabled {
            let page = menu_file.app_settings.current_page.unwrap_or(0);
            app.show_page(page);
        }
        app.rebuild_display();
        if add_builtin_boxes {
            let _ = app.save_menu();
        }
        Ok(app)
    }

    fn rebuild_display(&mut self) {
        self.sort_categories();
        self.display_entries.clear();
        let columns = self.column_count.max(1);
        self.column_map = vec![Vec::new(); columns as usize];
        for category in &mut self.categories {
            if category.expanded {
                category.refresh_files(&self.paths.config_dir);
            }
        }
        for (idx, category) in self.categories.iter().enumerate() {
            let column_index = ((category.column.saturating_sub(1)) as usize)
                .min(self.column_map.len().saturating_sub(1));
            let entry_index = self.display_entries.len();
            self.display_entries.push(DisplayEntry::Category {
                category_index: idx,
            });
            self.column_map[column_index].push(entry_index);
            if category.expanded {
                for item_index in 0..category.row_count() {
                    let entry_index = self.display_entries.len();
                    self.display_entries.push(DisplayEntry::Item {
                        category_index: idx,
                        item_index,
                    });
                    self.column_map[column_index].push(entry_index);
                }
            }
        }
        if self.current_index >= self.display_entries.len() {
            self.current_index = self.current_index.saturating_sub(1);
            if self.display_entries.is_empty() {
                self.current_index = 0;
            }
        }
    }

    fn entry_line(&self, entry_index: usize) -> (Line<'_>, Style) {
        match &self.display_entries[entry_index] {
            DisplayEntry::Category { category_index } => {
                let category = &self.categories[*category_index];
                let marker = if category.expanded { "▼" } else { "▶" };
                let mut style = Style::default()
                    .fg(self.theme.text)
                    .bg(self.theme.surface)
                    .add_modifier(Modifier::BOLD);
                if let Some(colors) = &category.colors {
                    if let Some(bg) = colors
                        .background
                        .as_ref()
                        .and_then(|hex| color_from_hex(hex))
                    {
                        style = style.bg(bg);
                    }
                    if let Some(text) = colors.text.as_ref().and_then(|hex| color_from_hex(hex)) {
                        style = style.fg(text);
                    }
                }
                (Line::from(format!("{marker} {}", category.name)), style)
            }
            DisplayEntry::Item {
                category_index,
                item_index,
            } => {
                let label = self.categories[*category_index].row_label(*item_index);
                let mut style = Style::default().fg(self.theme.text).bg(self.theme.surface);
                if let Some(colors) = self.categories[*category_index].colors.as_ref() {
                    if let Some(bg) = colors
                        .background
                        .as_ref()
                        .and_then(|hex| color_from_hex(hex))
                    {
                        style = style.bg(bg);
                    }
                    if let Some(text) = colors.text.as_ref().and_then(|hex| color_from_hex(hex)) {
                        style = style.fg(text);
                    }
                }
                (Line::from(format!(" {label}")), style)
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.active_popup.is_some() {
            let result = {
                let popup = self.active_popup.as_mut().unwrap();
                match popup {
                    PopupState::Info(_) | PopupState::Message(_) => match key.code {
                        KeyCode::Esc | KeyCode::Enter => PopupResult::Close(None),
                        _ => PopupResult::None,
                    },
                    PopupState::Confirm(confirm) => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            match confirm.action {
                                ConfirmAction::DeleteBox(index) => {
                                    PopupResult::Confirmed(ConfirmAction::DeleteBox(index))
                                }
                            }
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            PopupResult::Close(Some("Cancelled".into()))
                        }
                        _ => PopupResult::None,
                    },
                    PopupState::Contacts(app) => match app.handle_key(key) {
                        contacts::AppResult::Continue => PopupResult::None,
                        contacts::AppResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Calendar(app) => match app.handle_key(key) {
                        calendar::AppResult::Continue => PopupResult::None,
                        calendar::AppResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Spreadsheet(app) => match app.handle_key(key) {
                        spreadsheet::AppResult::Continue => PopupResult::None,
                        spreadsheet::AppResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Rss(app) => match app.handle_key(key) {
                        rss::AppResult::Continue => PopupResult::None,
                        rss::AppResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Weather(app) => match app.handle_key(key) {
                        weather::AppResult::Continue => PopupResult::None,
                        weather::AppResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Todo(app) => match app.handle_key(key) {
                        todo::AppResult::Continue => PopupResult::None,
                        todo::AppResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Paint(app) => match app.get_mut().handle_key(key) {
                        PaintResult::Continue => PopupResult::None,
                        PaintResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Midi(app) => match app.get_mut().handle_key(key) {
                        MidiResult::Continue => PopupResult::None,
                        MidiResult::Close => PopupResult::CloseApp,
                    },
                    PopupState::Notes(editor) => match editor.handle_key(key) {
                        NotesKeyResult::Continue => PopupResult::None,
                        NotesKeyResult::Save => PopupResult::SaveNotes,
                        NotesKeyResult::Discard => {
                            PopupResult::Close(Some("Notes changes discarded".into()))
                        }
                    },
                    PopupState::ItemForm(form) => match form.handle_key(key) {
                        ItemFormKeyResult::Continue => PopupResult::None,
                        ItemFormKeyResult::Cancel => {
                            PopupResult::Close(Some("Item edit cancelled".into()))
                        }
                        ItemFormKeyResult::Submit(data) => PopupResult::ItemSubmit(data),
                    },
                    PopupState::CategoryForm(form) => match form.handle_key(key) {
                        FormKeyResult::Continue => PopupResult::None,
                        FormKeyResult::Cancel => {
                            PopupResult::Close(Some("Category edit cancelled".into()))
                        }
                        FormKeyResult::Submit(data) => PopupResult::CategorySubmit(data),
                        FormKeyResult::DeletePreset(index) => {
                            PopupResult::CategoryDeletePreset(index)
                        }
                    },
                    PopupState::SettingsForm(form) => match form.handle_key(key) {
                        SettingsFormKeyResult::Continue => PopupResult::None,
                        SettingsFormKeyResult::Cancel => {
                            PopupResult::Close(Some("Settings update cancelled".into()))
                        }
                        SettingsFormKeyResult::Submit(data) => PopupResult::SettingsSubmit(data),
                        SettingsFormKeyResult::DeleteSavedTheme(index) => {
                            PopupResult::SettingsDeleteSavedTheme(index)
                        }
                    },
                }
            };
            match result {
                PopupResult::None => {}
                PopupResult::Confirmed(ConfirmAction::DeleteBox(index)) => {
                    self.active_popup = None;
                    self.delete_box(index);
                }
                PopupResult::SaveNotes => self.save_notes(),
                PopupResult::CloseApp => {
                    self.active_popup = None;
                    self.rebuild_display();
                }
                PopupResult::Close(status) => {
                    self.active_popup = None;
                    if let Some(msg) = status {
                        self.set_status(Some(msg));
                    }
                }
                PopupResult::ItemSubmit(data) => match self.apply_item_form_input(data) {
                    Ok(msg) => {
                        self.active_popup = None;
                        self.set_status(Some(msg));
                    }
                    Err(err_msg) => {
                        if let Some(PopupState::ItemForm(form)) = self.active_popup.as_mut() {
                            form.error = Some(err_msg);
                        }
                    }
                },
                PopupResult::CategorySubmit(data) => match self.process_category_submission(data) {
                    Ok(msg) => {
                        self.active_popup = None;
                        self.set_status(Some(msg));
                    }
                    Err(err_msg) => {
                        if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
                            form.error = Some(err_msg);
                        }
                    }
                },
                PopupResult::CategoryDeletePreset(index) => {
                    let result = self.delete_custom_category_preset(index);
                    self.handle_category_preset_delete_result(result);
                }
                PopupResult::SettingsSubmit(data) => match self.apply_settings_form_input(data) {
                    Ok(msg) => {
                        self.active_popup = None;
                        self.set_status(Some(msg));
                    }
                    Err(err_msg) => {
                        if let Some(PopupState::SettingsForm(form)) = self.active_popup.as_mut() {
                            form.error = Some(err_msg);
                        }
                    }
                },
                PopupResult::SettingsDeleteSavedTheme(index) => {
                    self.handle_saved_theme_deletion(index);
                }
            }
            return;
        }
        // With a calculator box selected, number and operator keys press its
        // buttons.
        if let Some(DisplayEntry::Category { category_index }) =
            self.display_entries.get(self.current_index)
        {
            let index = *category_index;
            if self.categories[index].kind == BoxKind::Calculator
                && self.categories[index].expanded
                && !key.modifiers.contains(KeyModifiers::CONTROL)
            {
                if let Some(label) = calculator::key_label(key) {
                    self.press_calculator(index, label);
                    return;
                }
            }
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            // Move the selected box: Shift+arrows, or H/J/K/L.
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.move_current_box(key.code)
            }
            KeyCode::Char('K') => self.move_current_box(KeyCode::Up),
            KeyCode::Char('J') => self.move_current_box(KeyCode::Down),
            KeyCode::Char('H') => self.move_current_box(KeyCode::Left),
            KeyCode::Char('L') => self.move_current_box(KeyCode::Right),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection_down(),
            KeyCode::Left | KeyCode::Char('h') => self.move_selection_sideways(false),
            KeyCode::Right | KeyCode::Char('l') => self.move_selection_sideways(true),
            KeyCode::Tab => self.move_to_box(true),
            KeyCode::BackTab => self.move_to_box(false),
            KeyCode::Enter => self.activate_current_entry(),
            KeyCode::Char(' ') => {
                self.toggle_category();
            }
            KeyCode::Char('r') => {
                if let Err(err) = self.reload_from_disk() {
                    self.set_status(Some(format!("Reload failed: {err}")));
                } else {
                    self.set_status(Some("Configuration reloaded".into()));
                }
            }
            KeyCode::Char('i') => self.show_info_popup(),
            KeyCode::Char('n') => self.queue_new_item(),
            KeyCode::Char('a') => self.queue_new_box(),
            KeyCode::Char('e') => self.queue_edit_current(),
            KeyCode::Char('d') => self.delete_selected_item(),
            KeyCode::Char('s') => self.queue_settings(),
            KeyCode::Char('[') if self.pages_enabled => self.cycle_page(false),
            KeyCode::Char(']') if self.pages_enabled => self.cycle_page(true),
            KeyCode::Char('p') => {
                // Settings, on the showing page's name (or the Pages switch).
                if self.pages_enabled {
                    self.queue_settings_with_focus(SettingsField::PageName(self.current_page));
                } else {
                    self.queue_settings_with_focus(SettingsField::Pages);
                }
            }
            KeyCode::Char('t') => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.queue_settings_with_focus(SettingsField::Title);
                } else {
                    self.queue_settings_with_focus(SettingsField::Theme);
                }
            }
            KeyCode::Char('b') => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.run_bin_scan();
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, terminal_area: Rect) {
        // Address Book, Calendar, Spreadsheet and RSS take every mouse event.
        let result = match self.active_popup.as_mut() {
            Some(PopupState::Contacts(app)) => Some(match app.handle_mouse(mouse, terminal_area) {
                contacts::AppResult::Continue => PopupResult::None,
                contacts::AppResult::Close => PopupResult::CloseApp,
            }),
            Some(PopupState::Calendar(app)) => Some(match app.handle_mouse(mouse, terminal_area) {
                calendar::AppResult::Continue => PopupResult::None,
                calendar::AppResult::Close => PopupResult::CloseApp,
            }),
            Some(PopupState::Spreadsheet(app)) => Some(match app.handle_mouse(mouse, terminal_area) {
                spreadsheet::AppResult::Continue => PopupResult::None,
                spreadsheet::AppResult::Close => PopupResult::CloseApp,
            }),
            Some(PopupState::Rss(app)) => Some(match app.handle_mouse(mouse, terminal_area) {
                rss::AppResult::Continue => PopupResult::None,
                rss::AppResult::Close => PopupResult::CloseApp,
            }),
            Some(PopupState::Weather(app)) => Some(match app.handle_mouse(mouse, terminal_area) {
                weather::AppResult::Continue => PopupResult::None,
                weather::AppResult::Close => PopupResult::CloseApp,
            }),
            Some(PopupState::Todo(app)) => Some(match app.handle_mouse(mouse, terminal_area) {
                todo::AppResult::Continue => PopupResult::None,
                todo::AppResult::Close => PopupResult::CloseApp,
            }),
            _ => None,
        };
        match result {
            Some(PopupResult::CloseApp) => {
                self.active_popup = None;
                self.rebuild_display();
                return;
            }
            Some(_) => return,
            None => {}
        }
        // Notes editor: clickable shortcut bar and wheel scrolling; clicks in
        // the text move the cursor (detect_popup_click below).
        if let Some(PopupState::Notes(_)) = self.active_popup.as_ref() {
            if let Some(key) = dmui::shortcut_click(terminal_area, NOTES_SHORTCUTS, mouse) {
                self.handle_key(key);
                return;
            }
            if let Some(key) = dmui::wheel_key(mouse) {
                for _ in 0..3 {
                    self.handle_key(key);
                }
                return;
            }
        }
        // The music app also needs every mouse event (drags, scrolls).
        if let Some(PopupState::Midi(app)) = self.active_popup.as_mut() {
            if let MidiResult::Close = app.get_mut().handle_mouse(mouse, terminal_area) {
                self.active_popup = None;
                self.rebuild_display();
            }
            return;
        }
        // Paint needs every mouse event (drags and releases finish strokes).
        if let Some(PopupState::Paint(app)) = self.active_popup.as_mut() {
            if let PaintResult::Close = app.get_mut().handle_mouse(mouse, terminal_area) {
                self.active_popup = None;
                self.rebuild_display();
            }
            return;
        }
        if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
            if form.glyph_picker.is_some() {
                return;
            }
            if let Some(picker) = form.icon_picker.as_mut() {
                let result = picker.get_mut().handle_mouse(mouse);
                form.finish_icon_pick(result);
                return;
            }
        }
        if self.active_popup.is_some() {
            if let Some(action) = self.detect_popup_click(mouse, terminal_area) {
                self.apply_popup_click(action);
            }
            return;
        }
        if self.box_drag.is_some() {
            self.continue_box_drag(mouse, terminal_area);
            return;
        }
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        if let Some(hit) = self.box_hit_at(mouse.column, mouse.row, terminal_area) {
            match hit {
                BoxHit::Title(entry_index) if !self.drag_boxes => {
                    // Dragging is off in Settings: a title click acts at once.
                    self.current_index = entry_index;
                    let is_shortcut = matches!(
                        self.display_entries.get(entry_index),
                        Some(DisplayEntry::Category { category_index })
                            if self.categories[*category_index].kind == BoxKind::Shortcut
                    );
                    if is_shortcut {
                        self.activate_current_entry();
                    } else {
                        self.toggle_category();
                    }
                }
                BoxHit::Title(entry_index) => {
                    // Pick the box up; release decides click vs. drop.
                    self.current_index = entry_index;
                    if let Some(DisplayEntry::Category { category_index }) =
                        self.display_entries.get(entry_index)
                    {
                        let column = self.categories[*category_index].column;
                        self.box_drag = Some(BoxDrag {
                            category_index: *category_index,
                            start: (mouse.column, mouse.row),
                            at: (mouse.column, mouse.row),
                            moved: false,
                            target_column: column.min(self.column_count.max(1)),
                            target_before: None,
                        });
                    }
                }
                BoxHit::Border(entry_index) => {
                    self.current_index = entry_index;
                    self.toggle_category();
                }
                BoxHit::CalcButton(entry_index, category_index, label) => {
                    self.current_index = entry_index;
                    self.press_calculator(category_index, label);
                }
                BoxHit::Row(entry_index) | BoxHit::Body(entry_index) => {
                    self.current_index = entry_index;
                    self.activate_current_entry();
                }
            }
            return;
        }

        let areas = screen_areas(terminal_area, self.pages_enabled);
        if let Some(page_area) = areas.pages {
            if mouse.row == page_area.y
                && mouse.column >= page_area.x
                && mouse.column < page_area.x + page_area.width
            {
                let bar = self.page_bar(page_area.width);
                match pages::action_at(&bar, mouse.column - page_area.x) {
                    Some(PageBarAction::Switch(index)) => self.switch_page(index),
                    Some(PageBarAction::Add) => self.add_page(),
                    None => {}
                }
                return;
            }
        }
        let footer_area = areas.shortcuts;
        if mouse.row >= footer_area.y
            && mouse.row < footer_area.y + footer_area.height
            && mouse.column >= footer_area.x
            && mouse.column < footer_area.x + footer_area.width
        {
            if self.handle_footer_click(mouse.column, footer_area) {
                return;
            }
        }
    }

    /// Lay out every box across all columns in one coordinated pass so a box
    /// spanning several columns reserves its vertical band in each column it
    /// covers (no overlapping boxes). A span-1 box stacks from the top of its
    /// column with a one-row gap, exactly as before, and scrolls so the
    /// selected row stays visible.
    fn all_box_layouts(&self, columns: &[Rect]) -> Vec<BoxLayout> {
        let mut layouts = Vec::new();
        if columns.is_empty() {
            return layouts;
        }
        let bottom = columns[0].y + columns[0].height;
        // Next free row per column; a spanning box advances the cursor of every
        // column it covers so the boxes below it stack underneath, not over it.
        let mut cursors: Vec<u16> = columns.iter().map(|c| c.y).collect();

        for (col_idx, &column_area) in columns.iter().enumerate() {
            if column_area.width < 3 || column_area.height == 0 {
                continue;
            }
            let entries = match self.column_map.get(col_idx) {
                Some(entries) => entries,
                None => continue,
            };

            // Group entries into (box_entry_index, category_index, row_entry_indices).
            let mut groups: Vec<(usize, usize, Vec<usize>)> = Vec::new();
            for &entry_index in entries {
                match &self.display_entries[entry_index] {
                    DisplayEntry::Category { category_index } => {
                        groups.push((entry_index, *category_index, Vec::new()));
                    }
                    DisplayEntry::Item { .. } => {
                        if let Some(last) = groups.last_mut() {
                            last.2.push(entry_index);
                        }
                    }
                }
            }

            for (box_entry_index, category_index, row_entries) in groups {
                let category = &self.categories[category_index];

                // A box may span several columns: its width is the run of
                // `span` columns starting at `col_idx`, gutters included.
                let span = category.span.max(1) as usize;
                let end = (col_idx + span).min(columns.len());
                let last = columns[end - 1];
                let box_width = (last.x + last.width) - column_area.x;
                let inner_width = box_width.saturating_sub(2);
                let file_cols = self.file_columns(category_index);

                // Sit below the last-placed box in every covered column.
                let top = (col_idx..end)
                    .map(|c| cursors[c])
                    .max()
                    .unwrap_or(column_area.y);
                let remaining = bottom.saturating_sub(top);
                if remaining < 2 {
                    break;
                }

                // Non-selectable text shown in the box body.
                let text: Vec<(String, Option<(usize, usize)>)> = if !category.expanded {
                    Vec::new()
                } else if let Some(placeholder) = category.placeholder() {
                    vec![(placeholder, None)]
                } else if category.kind == BoxKind::Notes {
                    boxes::wrap_text(&category.notes, inner_width.saturating_sub(1) as usize)
                        .into_iter()
                        .map(|line| (line, None))
                        .collect()
                } else if matches!(category.kind, BoxKind::Calendar | BoxKind::Spreadsheet) {
                    category.preview.clone()
                } else if matches!(category.kind, BoxKind::Weather | BoxKind::Todo) {
                    category.preview.clone()
                } else {
                    Vec::new()
                };
                let content_len = match (&category.icon_picture, category.kind) {
                    // The name is in the title band, so the body is just the picture.
                    (_, BoxKind::Shortcut) if category.name_only => 1,
                    (Some(picture), BoxKind::Shortcut) => picture.height.min(16) as usize,
                    (None, BoxKind::Shortcut) if category.icon_glyph.is_some() => 8,
                    (None, BoxKind::Shortcut) => 1,
                    (_, _) if !category.expanded => 0,
                    (_, BoxKind::Calculator) => calculator::BODY_ROWS,
                    // Files flow down, then across the box's file columns.
                    (_, _) if file_cols > 1 => row_entries.len().div_ceil(file_cols).max(text.len()),
                    (_, _) => (row_entries.len() * category.row_lines()).max(text.len()),
                };
                // A styled look (spiral calendar / to-do pad) adds rows of its own.
                let style = category.drawn_style(box_width);
                let content_len = if style == BoxStyle::AddressBook {
                    // A notebook page shows a few ruled lines even when short.
                    content_len.max(box_style::BOOK_MIN_LINES)
                } else if style == BoxStyle::Clipboard {
                    // A clipboard always shows its checklist lines, filled or not.
                    category.preview.len().max(
                        category.max_rows.map(usize::from).unwrap_or(box_style::CLIPBOARD_DEFAULT_LINES),
                    )
                } else if style == BoxStyle::TodoPad {
                    // A pad always shows its ruled lines, filled or not.
                    box_style::todo_items(&category.notes)
                        .len()
                        .max(category.max_rows.map(usize::from).unwrap_or(box_style::PAD_DEFAULT_LINES))
                } else {
                    content_len
                };
                let visible = match category.max_rows {
                    Some(max) if category.kind != BoxKind::Shortcut => {
                        content_len.min(max as usize)
                    }
                    _ => content_len,
                };
                // A notebook page never gets shorter than its minimum lines.
                let visible = if style == BoxStyle::AddressBook {
                    visible.max(box_style::BOOK_MIN_LINES)
                } else {
                    visible
                };
                let extra = style.extra_rows();
                // Paint boxes show the selected drawing under the list,
                // separated by a blank row. After the cursor leaves the box the
                // last drawing shown stays (else the first one with a picture).
                let selected_row = row_entries.iter().position(|e| *e == self.current_index);
                let preview = if category.kind == BoxKind::Paint
                    && category.expanded
                    && category.show_picture
                {
                    category.preview_picture(selected_row)
                } else {
                    None
                };
                let picture_rows = preview
                    .as_ref()
                    .map(|picture| picture.height.min(12) + 1)
                    .unwrap_or(0);
                let height = (2 + extra + visible as u16 + picture_rows).min(remaining);
                let outer = Rect {
                    x: column_area.x,
                    y: top,
                    width: box_width,
                    height,
                };
                // The list keeps its rows first when the column is too short.
                let inner_height = (height.saturating_sub(2 + extra) as usize).min(visible);
                let picture_space = height.saturating_sub(2 + inner_height as u16);
                let picture = preview.filter(|_| picture_space >= 2).map(|picture| {
                    let area = Rect {
                        x: outer.x + 1,
                        y: outer.y + 2 + inner_height as u16,
                        width: inner_width,
                        height: picture_space - 1,
                    };
                    (area, picture)
                });

                // Scroll so the selected row is inside the visible window. A
                // row may take more than one line (RSS articles take two).
                let lines = category.row_lines();
                let visible_rows = inner_height / lines;
                // Rows per file column (all rows when there is one column).
                let per_col = row_entries.len().div_ceil(file_cols).max(1);
                let selected = row_entries
                    .iter()
                    .position(|e| *e == self.current_index)
                    .map(|sel| sel % per_col);
                let offset = match selected {
                    Some(sel) if visible_rows > 0 && sel >= visible_rows => sel + 1 - visible_rows,
                    _ => 0,
                };
                let row_rect = |i: usize| Rect {
                    x: outer.x + 1,
                    y: outer.y + 1 + (i * lines) as u16,
                    width: inner_width,
                    height: lines as u16,
                };

                let mut rows = vec![(
                    box_entry_index,
                    Rect {
                        x: outer.x,
                        y: outer.y,
                        width: outer.width,
                        height: 1,
                    },
                )];
                let mut text_lines = Vec::new();
                if category.kind != BoxKind::Shortcut && file_cols > 1 {
                    // Equal file columns with a one-cell gap between them.
                    let col_width = inner_width / file_cols as u16;
                    for (i, entry) in row_entries.iter().enumerate() {
                        let (row, col) = (i % per_col, (i / per_col) as u16);
                        if row < offset || row >= offset + visible_rows {
                            continue;
                        }
                        let x = outer.x + 1 + col * col_width;
                        let width = if col + 1 == file_cols as u16 {
                            inner_width - col * col_width
                        } else {
                            col_width.saturating_sub(1)
                        };
                        rows.push((*entry, Rect { x, width, ..row_rect(row - offset) }));
                    }
                    for (i, (line, mark)) in text.iter().take(inner_height).enumerate() {
                        text_lines.push((row_rect(i), line.clone(), *mark));
                    }
                } else if category.kind != BoxKind::Shortcut {
                    for (i, entry) in row_entries.iter().skip(offset).take(visible_rows).enumerate() {
                        rows.push((*entry, row_rect(i)));
                    }
                    for (i, (line, mark)) in text.iter().take(inner_height).enumerate() {
                        text_lines.push((row_rect(i), line.clone(), *mark));
                    }
                }

                layouts.push(BoxLayout {
                    category_index,
                    outer,
                    rows,
                    text_lines,
                    picture,
                    more_above: offset > 0,
                    more_below: if lines > 1 {
                        offset + visible_rows < row_entries.len()
                    } else {
                        offset + inner_height < content_len
                    },
                });
                // Advance the cursor of every covered column past this box.
                let next = top.saturating_add(height + 1);
                for covered in col_idx..end {
                    cursors[covered] = next;
                }
            }
        }
        layouts
    }

    /// Drag or release while a box is picked up by its title. Where it is
    /// dropped picks both the column and the place in it: above the first box
    /// whose middle is below the pointer, else at the bottom.
    fn continue_box_drag(&mut self, mouse: MouseEvent, terminal_area: Rect) {
        let Some(dragged) = self.box_drag.as_ref().map(|d| d.category_index) else {
            return;
        };
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                // The column under the pointer (nearest one when in a gutter).
                let columns = column_areas(boxes_area(terminal_area, self.pages_enabled), self.column_count);
                let target_column = columns
                    .iter()
                    .position(|c| mouse.column < c.x + c.width + 1)
                    .or_else(|| columns.len().checked_sub(1))
                    .map(|idx| idx as u16 + 1);
                let target_before = target_column.and_then(|target| {
                    self.all_box_layouts(&columns)
                        .into_iter()
                        .filter(|layout| {
                            layout.category_index != dragged
                                && self.shown_column(layout.category_index) == target
                        })
                        .find(|layout| mouse.row < layout.outer.y + layout.outer.height / 2)
                        .map(|layout| layout.category_index)
                });
                let drag = self.box_drag.as_mut().unwrap();
                drag.at = (mouse.column, mouse.row);
                drag.moved |= drag.at != drag.start;
                if let Some(target) = target_column {
                    drag.target_column = target;
                    drag.target_before = target_before;
                }
                let name = self.categories[dragged].name.clone();
                let target = drag.target_column;
                let place = match drag.target_before {
                    Some(before) => format!("above \"{}\"", self.categories[before].name),
                    None => "at the bottom".to_string(),
                };
                self.set_status(Some(format!(
                    "Moving \"{name}\" to column {target}, {place} - release to drop"
                )));
            }
            MouseEventKind::Up(_) => {
                let drag = self.box_drag.take().unwrap();
                if !drag.moved {
                    // A plain click on the title: run a shortcut, else
                    // collapse/expand.
                    if self.categories[drag.category_index].kind == BoxKind::Shortcut {
                        self.activate_current_entry();
                    } else {
                        self.toggle_category();
                    }
                    return;
                }
                let name = self.categories[drag.category_index].name.clone();
                if self.place_box(drag.category_index, drag.target_column, drag.target_before) {
                    self.set_status(Some(format!("Moved \"{name}\" to column {}", drag.target_column)));
                } else {
                    self.set_status(Some(format!("\"{name}\" stays where it was")));
                }
            }
            _ => {}
        }
    }

    /// The column (1-based) a box is drawn in: its own, or the last one when
    /// there are fewer columns now.
    fn shown_column(&self, category_index: usize) -> u16 {
        self.categories[category_index]
            .column
            .clamp(1, self.column_count.max(1))
    }

    /// Put a box in `column`, above the box `before` (or at the bottom of the
    /// column), keep it selected and save. Returns false if nothing moved.
    fn place_box(&mut self, category_index: usize, column: u16, before: Option<usize>) -> bool {
        // The boxes of the target column, top to bottom, without the moved one.
        let mut stack: Vec<usize> = (0..self.categories.len())
            .filter(|&i| i != category_index && self.shown_column(i) == column)
            .collect();
        let at = before
            .and_then(|b| stack.iter().position(|&i| i == b))
            .unwrap_or(stack.len());
        stack.insert(at, category_index);
        let unchanged = self.shown_column(category_index) == column && {
            let current: Vec<usize> = (0..self.categories.len())
                .filter(|&i| self.shown_column(i) == column)
                .collect();
            current == stack
        };
        if unchanged {
            return false;
        }
        let name = self.categories[category_index].name.clone();
        // Boxes kept from a wider layout are pinned to the column they show
        // in, so the new order holds.
        for &i in &stack {
            self.categories[i].column = column;
        }
        // Renumber the target column in its new order. Its boxes keep their
        // slot relative to other columns since sorting is by column first.
        let base = stack.iter().map(|&i| self.categories[i].order).min().unwrap_or(0);
        for (pos, &i) in stack.iter().enumerate() {
            self.categories[i].order = base.saturating_add(pos as u32);
        }
        self.rebuild_display();
        if let Some(pos) = self.display_entries.iter().position(|entry| {
            matches!(entry, DisplayEntry::Category { category_index }
                if self.categories[*category_index].name == name)
        }) {
            self.current_index = pos;
        }
        let _ = self.save_menu();
        true
    }

    /// Keyboard move of the selected box: up/down within its column, or
    /// left/right to the neighbouring column at about the same height.
    fn move_current_box(&mut self, direction: KeyCode) {
        let Some(category_index) = self.current_category_index() else {
            return;
        };
        let column = self.shown_column(category_index);
        let in_column = |app: &Self, col: u16| -> Vec<usize> {
            (0..app.categories.len()).filter(|&i| app.shown_column(i) == col).collect()
        };
        let here = in_column(self, column);
        let pos = here.iter().position(|&i| i == category_index).unwrap_or(0);
        let name = self.categories[category_index].name.clone();
        let (target_column, before) = match direction {
            KeyCode::Up => {
                if pos == 0 {
                    self.set_status(Some(format!("\"{name}\" is already at the top")));
                    return;
                }
                (column, Some(here[pos - 1]))
            }
            KeyCode::Down => {
                if pos + 1 >= here.len() {
                    self.set_status(Some(format!("\"{name}\" is already at the bottom")));
                    return;
                }
                (column, here.get(pos + 2).copied())
            }
            KeyCode::Left | KeyCode::Right => {
                let target = if direction == KeyCode::Left {
                    column.saturating_sub(1)
                } else {
                    column + 1
                };
                if target < 1 || target > self.column_count.max(1) {
                    self.set_status(Some(format!("No column to the {}", if direction == KeyCode::Left { "left" } else { "right" })));
                    return;
                }
                (target, in_column(self, target).get(pos).copied())
            }
            _ => return,
        };
        if self.place_box(category_index, target_column, before) {
            self.set_status(Some(format!(
                "Moved \"{name}\" (column {target_column}) - Shift+arrows or H/J/K/L move it again"
            )));
        }
    }

    /// How many columns a box lists its files in: one per desktop column it
    /// spans (Span in the box editor), for boxes that list files.
    fn file_columns(&self, category_index: usize) -> usize {
        let category = &self.categories[category_index];
        if !category.lists_files() {
            return 1;
        }
        let free = (self.column_count.max(1) - self.shown_column(category_index) + 1) as usize;
        (category.span.max(1) as usize).min(free).max(1)
    }

    /// Left/Right: in a box that lists its files in several columns, move to
    /// the file beside the selected one; elsewhere, to the next/previous box.
    fn move_selection_sideways(&mut self, forward: bool) {
        if let Some(&DisplayEntry::Item { category_index, item_index }) =
            self.display_entries.get(self.current_index)
        {
            let cols = self.file_columns(category_index);
            if cols > 1 {
                let count = self.categories[category_index].row_count();
                let per_col = count.div_ceil(cols).max(1);
                let target = if forward {
                    Some(item_index + per_col).filter(|&t| t < count)
                } else {
                    item_index.checked_sub(per_col)
                };
                if let Some(target) = target {
                    self.current_index = self.current_index + target - item_index;
                }
                return;
            }
        }
        self.move_to_box(forward);
    }

    /// The box the selection is in (its title or one of its rows).
    fn current_category_index(&self) -> Option<usize> {
        match self.display_entries.get(self.current_index)? {
            DisplayEntry::Category { category_index } => Some(*category_index),
            DisplayEntry::Item { category_index, .. } => Some(*category_index),
        }
    }

    fn box_hit_at(&self, column: u16, row: u16, terminal_area: Rect) -> Option<BoxHit> {
        let inside = |r: Rect| {
            column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
        };
        let area = boxes_area(terminal_area, self.pages_enabled);
        if area.width == 0 || area.height == 0 || !inside(area) {
            return None;
        }
        let columns = column_areas(area, self.column_count);
        for box_layout in self.all_box_layouts(&columns) {
            if !inside(box_layout.outer) {
                continue;
            }
            let (box_entry, title_rect) = *box_layout.rows.first()?;
            if let Some((entry_index, _)) =
                box_layout.rows.iter().skip(1).find(|(_, rect)| inside(*rect))
            {
                return Some(BoxHit::Row(*entry_index));
            }
            let kind = self.categories[box_layout.category_index].kind;
            if kind == BoxKind::Calculator {
                if let Some(label) = calculator::button_at(
                    box_body(box_layout.outer),
                    column,
                    row,
                ) {
                    return Some(BoxHit::CalcButton(box_entry, box_layout.category_index, label));
                }
            }
            // The top line (title band) picks the box up for dragging.
            if inside(title_rect) {
                return Some(BoxHit::Title(box_entry));
            }
            if matches!(
                kind,
                BoxKind::Notes
                    | BoxKind::Shortcut
                    | BoxKind::Calendar
                    | BoxKind::Contacts
                    | BoxKind::Paint
                    | BoxKind::Music
                    | BoxKind::Documents
                    | BoxKind::Calculator
            ) {
                return Some(BoxHit::Body(box_entry));
            }
            return Some(BoxHit::Border(box_entry));
        }
        None
    }

    fn handle_footer_click(&mut self, column: u16, footer_area: Rect) -> bool {
        let line_data = self.footer_line_data();
        if line_data.segments.is_empty() || line_data.total_width == 0 {
            return false;
        }
        if footer_area.width == 0 || footer_area.height == 0 {
            return false;
        }
        let text_width = line_data.total_width.min(footer_area.width);
        let mut start_x = footer_area.x;
        if footer_area.width > text_width {
            start_x += (footer_area.width - text_width) / 2;
        }
        if column < start_x || column >= start_x + text_width {
            return false;
        }
        let relative = column - start_x;
        for segment in line_data.segments {
            if relative >= segment.start && relative < segment.end {
                self.execute_footer_action(segment.action);
                return true;
            }
        }
        false
    }

    fn detect_popup_click(
        &self,
        mouse: MouseEvent,
        terminal_area: Rect,
    ) -> Option<PopupClickAction> {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }
        let popup = self.active_popup.as_ref()?;
        match popup {
            PopupState::Notes(_) => {
                let [_, _, content_area, _] = popup_sections(terminal_area)?;
                let text_area = notes_text_area(content_area);
                if mouse.column < text_area.x
                    || mouse.column >= text_area.x + text_area.width
                    || mouse.row < text_area.y
                    || mouse.row >= text_area.y + text_area.height
                {
                    return None;
                }
                Some(PopupClickAction::Notes {
                    row: usize::from(mouse.row - text_area.y),
                    col: usize::from(mouse.column - text_area.x),
                })
            }
            PopupState::ItemForm(form) => {
                let (_lines, layout) = form.render_lines(self);
                let Some([_, _shortcut_area, content_area, _]) = popup_sections(terminal_area)
                else {
                    return None;
                };
                let inner = content_area.inner(&popup_content_margin());
                if inner.width == 0
                    || inner.height == 0
                    || mouse.column < inner.x
                    || mouse.column >= inner.x + inner.width
                    || mouse.row < inner.y
                    || mouse.row >= inner.y + inner.height
                {
                    return None;
                }
                let line_idx = usize::from(mouse.row.saturating_sub(inner.y));
                if line_idx >= layout.line_count {
                    return None;
                }
                if layout.label_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::Label,
                    )));
                }
                if layout.command_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::Command,
                    )));
                }
                if layout.description_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::Description,
                    )));
                }
                if layout.category_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::Category,
                    )));
                }
                if layout.clear_screen_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::ClearScreen,
                    )));
                }
                if layout.pause_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::Pause,
                    )));
                }
                if layout.available_heading_line == Some(line_idx) {
                    return Some(PopupClickAction::Item(ItemFormClick::SelectField(
                        ItemField::AvailableCategories,
                    )));
                }
                if let Some(start) = layout.available_start_line {
                    if line_idx >= start && line_idx < start + layout.available_count {
                        let index = line_idx - start;
                        return Some(PopupClickAction::Item(
                            ItemFormClick::SelectAvailableCategory(index),
                        ));
                    }
                }
                None
            }
            PopupState::CategoryForm(form) => {
                let (_lines, layout) = form.render_lines(self);
                let Some([_, shortcut_area, content_area, _]) = popup_sections(terminal_area)
                else {
                    return None;
                };
                if mouse.column >= shortcut_area.x
                    && mouse.column < shortcut_area.x + shortcut_area.width
                    && mouse.row >= shortcut_area.y
                    && mouse.row < shortcut_area.y + shortcut_area.height
                {
                    if layout.shortcut_total_width == 0
                        || layout.shortcut_segments.is_empty()
                        || shortcut_area.width == 0
                    {
                        return None;
                    }
                    let text_width = layout.shortcut_total_width.min(shortcut_area.width);
                    if text_width == 0 {
                        return None;
                    }
                    let mut start_x = shortcut_area.x;
                    if shortcut_area.width > text_width {
                        start_x += (shortcut_area.width - text_width) / 2;
                    }
                    if mouse.column < start_x || mouse.column >= start_x + text_width {
                        return None;
                    }
                    let relative = mouse.column - start_x;
                    for segment in &layout.shortcut_segments {
                        if relative >= segment.start && relative < segment.end {
                            return Some(PopupClickAction::Category(CategoryFormClick::Shortcut(
                                segment.action,
                            )));
                        }
                    }
                    return None;
                }
                let inner = content_area.inner(&popup_content_margin());
                if inner.width == 0
                    || inner.height == 0
                    || mouse.column < inner.x
                    || mouse.column >= inner.x + inner.width
                    || mouse.row < inner.y
                    || mouse.row >= inner.y + inner.height
                {
                    return None;
                }
                let line_idx = usize::from(mouse.row.saturating_sub(inner.y));
                if line_idx >= layout.line_count {
                    return None;
                }
                if let Some((_, field)) =
                    layout.field_lines.iter().find(|(line, _)| *line == line_idx)
                {
                    return Some(PopupClickAction::Category(CategoryFormClick::SelectField(
                        *field,
                    )));
                }
                if layout.presets_heading_line == Some(line_idx) {
                    return Some(PopupClickAction::Category(CategoryFormClick::SelectField(
                        CategoryField::Palette,
                    )));
                }
                if let Some(start) = layout.presets_start_line {
                    if line_idx >= start && line_idx < start + layout.presets_count {
                        let palette_idx = line_idx - start;
                        return Some(PopupClickAction::Category(
                            CategoryFormClick::SelectPalette(palette_idx),
                        ));
                    }
                }
                None
            }
            PopupState::SettingsForm(form) => {
                let (_lines, layout) = form.render_lines(self);
                let Some([_, shortcut_area, content_area, _]) = popup_sections(terminal_area)
                else {
                    return None;
                };
                if mouse.column >= shortcut_area.x
                    && mouse.column < shortcut_area.x + shortcut_area.width
                    && mouse.row >= shortcut_area.y
                    && mouse.row < shortcut_area.y + shortcut_area.height
                {
                    if layout.shortcut_total_width == 0
                        || layout.shortcut_segments.is_empty()
                        || shortcut_area.width == 0
                    {
                        return None;
                    }
                    let text_width = layout.shortcut_total_width.min(shortcut_area.width);
                    if text_width == 0 {
                        return None;
                    }
                    let mut start_x = shortcut_area.x;
                    if shortcut_area.width > text_width {
                        start_x += (shortcut_area.width - text_width) / 2;
                    }
                    if mouse.column < start_x || mouse.column >= start_x + text_width {
                        return None;
                    }
                    let relative = mouse.column - start_x;
                    for segment in &layout.shortcut_segments {
                        if relative >= segment.start && relative < segment.end {
                            return Some(PopupClickAction::Settings(SettingsFormClick::Shortcut(
                                segment.action,
                            )));
                        }
                    }
                    return None;
                }
                let inner = content_area.inner(&popup_content_margin());
                if inner.width == 0
                    || inner.height == 0
                    || mouse.column < inner.x
                    || mouse.column >= inner.x + inner.width
                    || mouse.row < inner.y
                    || mouse.row >= inner.y + inner.height
                {
                    return None;
                }
                let line_idx = usize::from(mouse.row.saturating_sub(inner.y));
                if line_idx >= layout.line_count {
                    return None;
                }
                if layout.title_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::Title,
                    )));
                }
                if layout.columns_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::Columns,
                    )));
                }
                if layout.drag_boxes_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::DragBoxes,
                    )));
                }
                if layout.pages_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::TogglePages));
                }
                if let Some((_, page)) =
                    layout.page_name_lines.iter().find(|(line, _)| *line == line_idx)
                {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::PageName(*page),
                    )));
                }
                if layout.theme_heading_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::Theme,
                    )));
                }
                if let Some(start) = layout.theme_list_start {
                    if line_idx >= start && line_idx < start + layout.theme_count {
                        let theme_idx = line_idx - start;
                        return Some(PopupClickAction::Settings(SettingsFormClick::SelectTheme(
                            theme_idx,
                        )));
                    }
                }
                if layout.delete_saved_theme_line == Some(line_idx) {
                    if let Some(idx) = layout.delete_saved_theme_index {
                        return Some(PopupClickAction::Settings(
                            SettingsFormClick::DeleteSavedTheme(idx),
                        ));
                    }
                }
                if layout.custom_heading_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomName,
                    )));
                }
                if layout.custom_name_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomName,
                    )));
                }
                if layout.custom_primary_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomPrimary,
                    )));
                }
                if layout.custom_accent_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomAccent,
                    )));
                }
                if layout.custom_background_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomBackground,
                    )));
                }
                if layout.custom_surface_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomSurface,
                    )));
                }
                if layout.custom_text_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomText,
                    )));
                }
                if layout.custom_highlight_line == Some(line_idx) {
                    return Some(PopupClickAction::Settings(SettingsFormClick::SelectField(
                        SettingsField::CustomHighlight,
                    )));
                }
                None
            }
            _ => None,
        }
    }

    fn apply_popup_click(&mut self, action: PopupClickAction) {
        match action {
            PopupClickAction::Notes { row, col } => {
                if let Some(PopupState::Notes(editor)) = self.active_popup.as_mut() {
                    editor.click(row, col);
                }
            }
            PopupClickAction::Item(item_click) => {
                if let Some(PopupState::ItemForm(form)) = self.active_popup.as_mut() {
                    match item_click {
                        ItemFormClick::SelectField(field) => {
                            form.selected_field = field;
                        }
                        ItemFormClick::SelectAvailableCategory(index) => {
                            form.selected_field = ItemField::AvailableCategories;
                            form.pick_available_category(index);
                            form.selected_field = ItemField::Category;
                        }
                    }
                }
            }
            PopupClickAction::Category(category_click) => {
                let mut pending_submit: Option<CategorySubmitPayload> = None;
                let mut pending_delete: Option<usize> = None;
                let mut pending_cancel = false;
                if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
                    match category_click {
                        CategoryFormClick::SelectField(field) => {
                            form.selected_field = field;
                        }
                        CategoryFormClick::SelectPalette(index) => {
                            if index < form.color_presets.len() {
                                form.selected_field = CategoryField::Palette;
                                form.palette_index = index;
                                form.apply_selected_palette();
                            }
                        }
                        CategoryFormClick::Shortcut(action) => match action {
                            CategoryShortcutAction::NextField => {
                                form.error = None;
                                form.next_field();
                            }
                            CategoryShortcutAction::PreviousField => {
                                form.error = None;
                                form.previous_field();
                            }
                            CategoryShortcutAction::Submit => {
                                match form.build_submission() {
                                    Ok(input) => pending_submit = Some(input),
                                    Err(err) => form.error = Some(err),
                                }
                            }
                            CategoryShortcutAction::Cancel => {
                                pending_cancel = true;
                            }
                            CategoryShortcutAction::PreviousPalette => {
                                form.error = None;
                                form.previous_palette();
                            }
                            CategoryShortcutAction::NextPalette => {
                                form.error = None;
                                form.next_palette();
                            }
                            CategoryShortcutAction::DeletePreset => {
                                if let Some(index) = form.current_custom_preset_index() {
                                    pending_delete = Some(index);
                                } else {
                                    form.error = Some("Select a custom theme to delete".into());
                                }
                            }
                        },
                    }
                }
                if let Some(index) = pending_delete {
                    let result = self.delete_custom_category_preset(index);
                    self.handle_category_preset_delete_result(result);
                }
                if let Some(input) = pending_submit {
                    let submit_result = self.process_category_submission(input);
                    match submit_result {
                        Ok(msg) => {
                            self.active_popup = None;
                            self.set_status(Some(msg));
                        }
                        Err(err) => {
                            if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut()
                            {
                                form.error = Some(err);
                            }
                        }
                    }
                }
                if pending_cancel {
                    self.active_popup = None;
                    self.set_status(Some("Category edit cancelled".into()));
                }
            }
            PopupClickAction::Settings(settings_click) => {
                let mut pending_delete_theme: Option<usize> = None;
                if let Some(PopupState::SettingsForm(form)) = self.active_popup.as_mut() {
                    match settings_click {
                        SettingsFormClick::SelectField(field) => {
                            form.selected_field = field;
                        }
                        SettingsFormClick::TogglePages => {
                            form.selected_field = SettingsField::Pages;
                            form.toggle_pages();
                        }
                        SettingsFormClick::SelectTheme(index) => {
                            if index < form.theme_options.len() {
                                form.selected_field = SettingsField::Theme;
                                if form.theme_index != index {
                                    form.theme_index = index;
                                    form.populate_custom_fields_from_selection();
                                }
                            }
                        }
                        SettingsFormClick::DeleteSavedTheme(index) => {
                            pending_delete_theme = Some(index);
                        }
                        SettingsFormClick::Shortcut(action) => match action {
                            SettingsShortcutAction::NextField => {
                                if let Some(PopupState::SettingsForm(form)) =
                                    self.active_popup.as_mut()
                                {
                                    form.error = None;
                                    form.next_field();
                                }
                            }
                            SettingsShortcutAction::Submit => {
                                let input = if let Some(PopupState::SettingsForm(form)) =
                                    self.active_popup.as_mut()
                                {
                                    form.error = None;
                                    form.to_input()
                                } else {
                                    return;
                                };
                                match self.apply_settings_form_input(input) {
                                    Ok(msg) => {
                                        self.active_popup = None;
                                        self.set_status(Some(msg));
                                    }
                                    Err(err) => {
                                        if let Some(PopupState::SettingsForm(form)) =
                                            self.active_popup.as_mut()
                                        {
                                            form.error = Some(err);
                                        }
                                    }
                                }
                            }
                            SettingsShortcutAction::Cancel => {
                                self.active_popup = None;
                                self.set_status(Some("Settings update cancelled".into()));
                            }
                            SettingsShortcutAction::PreviousTheme => {
                                if let Some(PopupState::SettingsForm(form)) =
                                    self.active_popup.as_mut()
                                {
                                    form.error = None;
                                    form.previous_theme();
                                }
                            }
                            SettingsShortcutAction::NextTheme => {
                                if let Some(PopupState::SettingsForm(form)) =
                                    self.active_popup.as_mut()
                                {
                                    form.error = None;
                                    form.next_theme();
                                }
                            }
                            SettingsShortcutAction::DeleteTheme => {
                                if let Some(PopupState::SettingsForm(form)) =
                                    self.active_popup.as_mut()
                                {
                                    if let Some(index) = form.current_deletable_theme_index() {
                                        pending_delete_theme = Some(index);
                                    } else {
                                        form.error =
                                            Some("Select a custom theme to delete".into());
                                    }
                                }
                            }
                        },
                    }
                }
                if let Some(index) = pending_delete_theme {
                    self.handle_saved_theme_deletion(index);
                }
            }
        }
    }

    fn reload_from_disk(&mut self) -> Result<()> {
        let menu_file = MenuFile::load(&self.paths.menu_file)?;
        let loaded_pages = pages_from_file(&menu_file);
        self.theme = Theme::load(&self.paths.theme_file)?;
        self.saved_themes = menu_file.saved_themes;
        self.theme_key = AppState::resolve_theme_key(
            menu_file.app_settings.theme_key.clone(),
            &self.theme,
            &self.saved_themes,
        );
        self.categories = menu_file
            .categories
            .iter()
            .map(|(name, cfg)| CategoryState::from_config(name, cfg))
            .collect();
        self.categories
            .sort_by_key(|category| (category.column, category.name.clone()));
        self.custom_colors = menu_file.custom_colors;
        self.column_count = menu_file
            .app_settings
            .columns
            .unwrap_or(self.column_count)
            .clamp(1, MAX_COLUMNS);
        self.drag_boxes = menu_file.app_settings.drag_boxes.unwrap_or(true);
        self.pages_enabled = menu_file.app_settings.pages_enabled;
        self.pages = loaded_pages;
        self.current_page = 0;
        self.box_drag = None;
        if self.pages_enabled {
            self.show_page(menu_file.app_settings.current_page.unwrap_or(0));
        }
        if let Some(title) = menu_file.app_settings.title {
            self.title = title;
        }
        self.rebuild_display();
        Ok(())
    }

    fn toggle_category(&mut self) {
        if let Some(DisplayEntry::Category { category_index }) =
            self.display_entries.get(self.current_index)
        {
            if let Some(category) = self.categories.get_mut(*category_index) {
                if category.kind == BoxKind::Shortcut {
                    return;
                }
                category.expanded = !category.expanded;
                self.rebuild_display();
                let _ = self.save_menu();
            }
        }
    }

    fn move_selection_up(&mut self) {
        if self.display_entries.is_empty() {
            return;
        }
        if self.current_index == 0 {
            self.current_index = self.display_entries.len().saturating_sub(1);
        } else {
            self.current_index -= 1;
        }
    }

    fn move_selection_down(&mut self) {
        if self.display_entries.is_empty() {
            return;
        }
        self.current_index = (self.current_index + 1) % self.display_entries.len();
    }

    /// Press a button on a calculator box; the box remembers the display.
    fn press_calculator(&mut self, category_index: usize, label: &str) {
        if let Some(category) = self.categories.get_mut(category_index) {
            category.calc.press(label);
            category.notes = category.calc.display();
        }
    }

    /// Jump to the title of the next box (Tab) or the previous one
    /// (Shift+Tab), in screen order, wrapping around. From inside a box,
    /// Shift+Tab goes to the box before it, not its own title.
    fn move_to_box(&mut self, forward: bool) {
        if let Some(index) = box_jump(&self.display_entries, self.current_index, forward) {
            self.current_index = index;
        }
    }

    fn activate_current_entry(&mut self) {
        if let Some(entry) = self.display_entries.get(self.current_index) {
            match entry {
                DisplayEntry::Category { category_index } => {
                    match self.categories[*category_index].kind {
                        BoxKind::Shortcut => self.prepare_command(),
                        BoxKind::Notes => self.open_notes_editor(*category_index),
                        BoxKind::Documents => self.open_document(*category_index, None),
                        // Its buttons are pressed in place (keys / clicks).
                        BoxKind::Calculator => {}
                        BoxKind::Contacts
                        | BoxKind::Calendar
                        | BoxKind::Paint
                        | BoxKind::Music
                        | BoxKind::Spreadsheet
                        | BoxKind::Rss => self.open_box_app(*category_index, None),
                        BoxKind::Weather | BoxKind::Todo => {
                            self.open_box_app(*category_index, None)
                        }
                        _ => self.toggle_category(),
                    }
                }
                DisplayEntry::Item {
                    category_index,
                    item_index,
                } if matches!(self.categories[*category_index].kind, BoxKind::Paint | BoxKind::Music)
                    || (self.categories[*category_index].kind == BoxKind::Folder
                        && matches!(
                            self.categories[*category_index].app.trim(),
                            BUILTIN_PAINT_APP | BUILTIN_MIDI_APP
                        )) =>
                {
                    self.open_box_app(*category_index, Some(*item_index))
                }
                DisplayEntry::Item {
                    category_index,
                    item_index,
                } if matches!(
                    self.categories[*category_index].kind,
                    BoxKind::Contacts | BoxKind::Rss | BoxKind::Spreadsheet
                ) => {
                    self.open_box_app(*category_index, Some(*item_index))
                }
                DisplayEntry::Item {
                    category_index,
                    item_index,
                } if self.categories[*category_index].kind == BoxKind::Documents => {
                    self.open_document(*category_index, Some(*item_index))
                }
                DisplayEntry::Item { .. } => self.prepare_command(),
            }
        }
    }

    /// The command the current selection would run, with a label for the
    /// status bar.
    fn selected_command(&self) -> Option<(String, PendingCommand)> {
        match *self.display_entries.get(self.current_index)? {
            DisplayEntry::Item {
                category_index,
                item_index,
            } => {
                let category = &self.categories[category_index];
                match category.kind {
                    BoxKind::Menu => {
                        let item = category.items.get(item_index)?;
                        if item.cmd.trim().is_empty() {
                            return None;
                        }
                        Some((
                            item.label.clone(),
                            PendingCommand {
                                command: item.cmd.clone(),
                                pause: item.pause,
                                clear_screen: item.clear_screen,
                            },
                        ))
                    }
                    BoxKind::Folder | BoxKind::Apps => {
                        let file = category.files.get(item_index)?;
                        let command = if category.kind == BoxKind::Apps {
                            boxes::shell_quote(&file.to_string_lossy())
                        } else {
                            if category.app.trim().is_empty()
                                || category.app.trim() == BUILTIN_PAINT_APP
                                || category.app.trim() == BUILTIN_MIDI_APP
                            {
                                return None;
                            }
                            boxes::open_with_command(&category.app, file)
                        };
                        Some((
                            category.row_label(item_index),
                            PendingCommand {
                                command,
                                pause: category.pause,
                                clear_screen: category.clear_screen,
                            },
                        ))
                    }
                    BoxKind::Shortcut
                    | BoxKind::Notes
                    | BoxKind::Contacts
                    | BoxKind::Calendar
                    | BoxKind::Paint
                    | BoxKind::Music
                    | BoxKind::Spreadsheet
                    | BoxKind::Rss
                    | BoxKind::Documents
                    | BoxKind::Calculator => None,
                    BoxKind::Weather | BoxKind::Todo => None,
                }
            }
            DisplayEntry::Category { category_index } => {
                let category = &self.categories[category_index];
                if category.kind != BoxKind::Shortcut || category.app.trim().is_empty() {
                    return None;
                }
                Some((
                    category.name.clone(),
                    PendingCommand {
                        command: category.app.clone(),
                        pause: category.pause,
                        clear_screen: category.clear_screen,
                    },
                ))
            }
        }
    }

    fn prepare_command(&mut self) {
        if let Some((label, command)) = self.selected_command() {
            self.status_message = Some(format!("Running {label}"));
            self.pending_command = Some(command);
        }
    }

    /// Open the full-screen Address Book or Calendar for a box.
    fn open_box_app(&mut self, category_index: usize, select: Option<usize>) {
        let Some(category) = self.categories.get(category_index) else {
            return;
        };
        let path = category.data_path(&self.paths.config_dir);
        self.active_popup = match category.kind {
            BoxKind::Contacts => Some(PopupState::Contacts(Box::new(ContactsApp::open(
                &category.name,
                path,
                select,
            )))),
            BoxKind::Calendar => Some(PopupState::Calendar(Box::new(CalendarApp::open(
                &category.name,
                path,
            )))),
            BoxKind::Spreadsheet => {
                // A sheets folder: open the chosen .csv, or start a new one
                // from the title. A single .csv file: open it.
                let file = if category.sheet_is_folder() {
                    match select.and_then(|row| category.files.get(row)) {
                        Some(file) => file.clone(),
                        None => new_file_name(&path, "sheet", "csv"),
                    }
                } else {
                    path
                };
                Some(PopupState::Spreadsheet(Box::new(SpreadsheetApp::open(
                    &category.name,
                    file,
                ))))
            }
            BoxKind::Rss => {
                let article = select
                    .filter(|_| category.shows_articles())
                    .and_then(|row| category.headlines.get(row));
                Some(PopupState::Rss(Box::new(match article {
                    Some(h) => RssApp::open_article(&category.name, path, h.feed, h.article),
                    None => RssApp::open(
                        &category.name,
                        path,
                        select.filter(|_| !category.shows_articles()),
                    ),
                })))
            }
            BoxKind::Music => {
                let app = match select.and_then(|row| category.files.get(row)) {
                    Some(file) => MidiApp::open(path, file),
                    None => MidiApp::new(path),
                };
                Some(PopupState::Midi(Box::new(RefCell::new(app))))
            }
            BoxKind::Weather => {
                Some(PopupState::Weather(Box::new(WeatherApp::open(&category.name, path))))
            }
            BoxKind::Todo => {
                Some(PopupState::Todo(Box::new(TodoApp::open(&category.name, path))))
            }
            BoxKind::Folder if category.app.trim() == BUILTIN_MIDI_APP => select
                .and_then(|row| category.files.get(row))
                .map(|file| {
                    let app = MidiApp::open(music_dir(&self.paths.config_dir), file);
                    PopupState::Midi(Box::new(RefCell::new(app)))
                }),
            BoxKind::Paint | BoxKind::Folder => {
                let dir = if category.kind == BoxKind::Paint {
                    path
                } else {
                    paintings_dir(&self.paths.config_dir)
                };
                let app = match select.and_then(|row| category.files.get(row)) {
                    Some(file) => PaintApp::open(dir, file),
                    None => PaintApp::new(dir),
                };
                Some(PopupState::Paint(Box::new(RefCell::new(app))))
            }
            _ => None,
        };
    }

    fn palette(&self) -> Palette {
        Palette {
            primary: self.theme.primary,
            accent: self.theme.accent,
            highlight: self.theme.highlight,
            background: self.theme.background,
            surface: self.theme.surface,
            text: self.theme.text,
            bar_bg: self.theme.bar_bg(),
            bar_key: self.theme.bar_key(),
            bar_label: self.theme.bar_label(),
            selection_text: self.theme.selection_text(),
            focus_band: self.theme.focus_band(),
        }
    }

    /// Open a Documents box file in the Notes editor; `None` starts a new
    /// document in the box's folder.
    fn open_document(&mut self, category_index: usize, select: Option<usize>) {
        let Some(category) = self.categories.get(category_index) else {
            return;
        };
        let file = match select.and_then(|row| category.files.get(row)) {
            Some(file) => file.clone(),
            None => {
                let dir = category.data_path(&self.paths.config_dir);
                let _ = fs::create_dir_all(&dir);
                new_file_name(&dir, "document", "txt")
            }
        };
        self.active_popup = Some(PopupState::Notes(NotesEditor::for_file(category_index, file)));
    }

    fn open_notes_editor(&mut self, category_index: usize) {
        if let Some(category) = self.categories.get(category_index) {
            self.active_popup = Some(PopupState::Notes(NotesEditor::new(
                category_index,
                category.name.clone(),
                &category.notes,
            )));
        }
    }

    /// The boxes of page `index` (the showing page's are in `categories`).
    fn page_categories(&self, index: usize) -> &[CategoryState] {
        if index == self.current_page {
            &self.categories
        } else {
            &self.pages[index].categories
        }
    }

    fn page_names(&self) -> Vec<String> {
        self.pages.iter().map(|page| page.name.clone()).collect()
    }

    /// Swap page `index`'s boxes onto the desktop. Does not redraw or save.
    /// Returns false when that page is already showing or does not exist.
    fn show_page(&mut self, index: usize) -> bool {
        if index >= self.pages.len() || index == self.current_page {
            return false;
        }
        let showing = std::mem::take(&mut self.categories);
        self.pages[self.current_page].categories = showing;
        self.categories = std::mem::take(&mut self.pages[index].categories);
        self.current_page = index;
        self.current_index = 0;
        self.box_drag = None;
        true
    }

    /// Switch to another page (desktop) and remember it.
    fn switch_page(&mut self, index: usize) {
        if !self.pages_enabled || index >= self.pages.len() {
            return;
        }
        if self.show_page(index) {
            self.rebuild_display();
            let _ = self.save_menu();
        }
        let name = self.pages[self.current_page].name.clone();
        self.set_status(Some(format!(
            "Page {} of {}: {name}",
            self.current_page + 1,
            self.pages.len()
        )));
    }

    /// `]` / `[`: the next or previous page, wrapping around.
    fn cycle_page(&mut self, forward: bool) {
        let count = self.pages.len();
        if count < 2 {
            self.set_status(Some(
                "Only one page - click + New page on the page bar to add another".into(),
            ));
            return;
        }
        let next = if forward {
            (self.current_page + 1) % count
        } else {
            (self.current_page + count - 1) % count
        };
        self.switch_page(next);
    }

    /// The page bar's `+ New page`: add an empty page and show it.
    fn add_page(&mut self) {
        let name = pages::default_page_name(&self.page_names());
        self.pages.push(PageState {
            name: name.clone(),
            categories: Vec::new(),
        });
        self.show_page(self.pages.len() - 1);
        self.rebuild_display();
        let _ = self.save_menu();
        self.set_status(Some(format!(
            "Added page \"{name}\" - press a to add boxes, p to rename it"
        )));
    }

    fn page_bar(&self, width: u16) -> PageBar {
        let bar_bg = self.theme.surface;
        let active = match self.theme.focus_band() {
            Some(band) => Style::default().fg(self.theme.accent).bg(band),
            None => Style::default()
                .fg(self.theme.selection_text())
                .bg(self.theme.highlight),
        };
        let styles = PageBarStyles {
            tab: Style::default().fg(self.theme.text).bg(bar_bg),
            active: active.add_modifier(Modifier::BOLD),
            add: Style::default()
                .fg(self.theme.accent)
                .bg(bar_bg)
                .add_modifier(Modifier::BOLD),
            hint: Style::default()
                .fg(self.theme.text)
                .bg(bar_bg)
                .add_modifier(Modifier::DIM),
        };
        pages::page_bar(&self.page_names(), self.current_page, width, &styles)
    }

    /// Apply the page names saved in Settings: rename, add and remove pages.
    /// A removed page's boxes move to the first page (renamed if a box
    /// there already has the name), so nothing is lost. Returns whether
    /// anything changed.
    fn apply_page_plan(&mut self, plan: pages::PagePlan) -> bool {
        let old_names = self.page_names();
        // Park the showing page's boxes so every page's boxes are in `pages`.
        let showing = std::mem::take(&mut self.categories);
        self.pages[self.current_page].categories = showing;
        let old_current = self.current_page;
        let mut old: Vec<Option<PageState>> =
            std::mem::take(&mut self.pages).into_iter().map(Some).collect();

        let mut new_pages: Vec<PageState> = Vec::new();
        let mut new_current = 0;
        for (new_index, (source, name)) in plan.pages.into_iter().enumerate() {
            let categories = source
                .and_then(|index| old.get_mut(index).and_then(Option::take))
                .map(|page| page.categories)
                .unwrap_or_default();
            if source == Some(old_current) {
                new_current = new_index;
            }
            new_pages.push(PageState { name, categories });
        }
        // Removed pages (and, defensively, any page the plan skipped).
        let mut moved = 0;
        for page in old.into_iter().flatten() {
            for mut category in page.categories {
                let name = {
                    let first = &new_pages[0].categories;
                    pages::unique_name(&category.name, |n| first.iter().any(|c| c.name == n))
                };
                category.name = name;
                new_pages[0].categories.push(category);
                moved += 1;
            }
        }
        self.pages = new_pages;
        self.current_page = new_current;
        self.categories = std::mem::take(&mut self.pages[new_current].categories);
        if new_current != old_current || !plan.removed.is_empty() {
            self.current_index = 0;
            self.box_drag = None;
        }
        self.rebuild_display();
        moved > 0 || self.page_names() != old_names
    }

    fn save_menu(&self) -> Result<()> {
        let categories_map = categories_to_map(self.page_categories(0));
        let extra_pages = (1..self.pages.len())
            .map(|index| PageFile {
                name: self.pages[index].name.clone(),
                categories: categories_to_map(self.page_categories(index)),
            })
            .collect();
        let menu_file = MenuFile {
            categories: categories_map,
            pages: extra_pages,
            app_settings: AppSettings {
                title: Some(self.title.clone()),
                columns: Some(self.column_count),
                theme_key: Some(self.theme_key.clone()),
                builtin_boxes_added: true,
                paint_box_added: true,
                music_box_added: true,
                menumaker_boxes_added: true,
                calculator_box_added: true,
                drag_boxes: Some(self.drag_boxes),
                pages_enabled: self.pages_enabled,
                // Only written once Pages has been used, so older files keep their shape.
                first_page_name: self
                    .pages
                    .first()
                    .map(|page| page.name.clone())
                    .filter(|name| {
                        self.pages_enabled
                            || self.pages.len() > 1
                            || name != pages::DEFAULT_FIRST_PAGE
                    }),
                current_page: (self.current_page > 0).then_some(self.current_page),
            },
            custom_colors: self.custom_colors.clone(),
            saved_themes: self.saved_themes.clone(),
        };
        menu_file.save(&self.paths.menu_file)
    }

    fn take_pending_command(&mut self) -> Option<PendingCommand> {
        self.pending_command.take()
    }

    fn take_pending_action(&mut self) -> Option<DeferredAction> {
        self.pending_action.take()
    }

    fn set_status(&mut self, message: Option<String>) {
        self.status_message = message;
    }

    fn status_text(&self) -> String {
        let total = self.display_entries.len();
        let current = if total == 0 {
            0
        } else {
            self.current_index + 1
        };
        let mut text = format!("Item {}/{} | Theme: {}", current, total, self.theme.name);
        if let Some(msg) = &self.status_message {
            text.push_str(" | ");
            text.push_str(msg);
        }
        text
    }

    fn available_color_presets(&self) -> Vec<ColorPreset> {
        // First choice: no colors of its own, so the box follows the app theme.
        let mut presets = vec![ColorPreset {
            name: THEME_DEFAULT_PRESET.to_string(),
            background: String::new(),
            text: String::new(),
            border: String::new(),
            custom_index: None,
        }];
        presets.extend(DEFAULT_CATEGORY_COLOR_PRESETS
            .iter()
            .map(|(name, bg, text, border)| {
                let mut preset = ColorPreset::new(*name, *bg, *text);
                if !border.is_empty() {
                    preset.border = normalize_hex(border);
                }
                preset
            }));
        for (idx, pair) in self.custom_colors.iter().enumerate() {
            if let (Some(bg), Some(text)) = (pair.background.as_deref(), pair.text.as_deref()) {
                let name = pair
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Custom Theme {}", idx + 1));
                presets.push(ColorPreset::from_custom(
                    name,
                    bg,
                    text,
                    pair.border.as_deref(),
                    idx,
                ));
            }
        }
        if presets.is_empty() {
            presets.push(ColorPreset::new("Default", "#034e68", "#caf0f8"));
        }
        presets
    }

    fn theme_options(&self) -> Vec<ThemeOption> {
        let mut options: Vec<ThemeOption> = THEME_PRESETS
            .iter()
            .map(|(key, def)| ThemeOption::from_definition(key, def))
            .collect();
        for (idx, saved) in self.saved_themes.iter().enumerate() {
            options.push(ThemeOption {
                key: saved_theme_key(idx),
                label: saved.name.clone(),
                primary_hex: saved.primary.clone(),
                accent_hex: saved.accent.clone(),
                highlight_hex: saved
                    .highlight
                    .clone()
                    .unwrap_or_else(|| saved.accent.clone()),
                background_hex: saved.background.clone(),
                surface_hex: saved.surface.clone(),
                text_hex: saved.text.clone(),
            });
        }
        if self.theme_key == CUSTOM_THEME_KEY {
            options.push(ThemeOption {
                key: CUSTOM_THEME_KEY.to_string(),
                label: "Custom Theme".to_string(),
                primary_hex: self.theme.primary_hex.clone(),
                accent_hex: self.theme.accent_hex.clone(),
                highlight_hex: self.theme.highlight_hex.clone(),
                background_hex: self.theme.background_hex.clone(),
                surface_hex: self.theme.surface_hex.clone(),
                text_hex: self.theme.text_hex.clone(),
            });
        }
        options
    }

    fn theme_from_saved_index(&self, index: usize) -> Option<Theme> {
        self.saved_themes.get(index).map(|saved| {
            let highlight = saved
                .highlight
                .as_deref()
                .unwrap_or_else(|| saved.accent.as_str());
            Theme::from_hexes(
                saved.name.clone(),
                &saved.primary,
                &saved.accent,
                highlight,
                &saved.background,
                &saved.surface,
                &saved.text,
            )
        })
    }

    fn footer_line(&self) -> Line<'static> {
        self.footer_line_data().line
    }

    fn footer_line_data(&self) -> FooterLineData {
        let (base_bg, shortcut_fg, label_fg) = match self.theme.extras {
            Some(extras) => (extras.bar_bg, extras.bar_key, extras.bar_label),
            None => (
                color_from_hex("#76B3C5").unwrap_or(self.theme.highlight),
                color_from_hex("#FDA009").unwrap_or(self.theme.accent),
                color_from_hex("#2E3544").unwrap_or(self.theme.surface),
            ),
        };
        let shortcut_style = Style::default()
            .fg(shortcut_fg)
            .bg(base_bg)
            .add_modifier(Modifier::BOLD);
        let label_style = Style::default().fg(label_fg).bg(base_bg);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut segments = Vec::new();
        let mut cursor: u16 = 0;
        for (index, shortcut) in FOOTER_SHORTCUTS.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(" | ", label_style));
                cursor = cursor.saturating_add(3);
            }
            let entry_start = cursor;
            spans.push(Span::styled(shortcut.key, shortcut_style));
            spans.push(Span::styled(shortcut.label, label_style));
            let key_len = shortcut.key.chars().count() as u16;
            let label_len = shortcut.label.chars().count() as u16;
            let entry_end = entry_start
                .saturating_add(key_len)
                .saturating_add(label_len);
            segments.push(FooterSegment {
                start: entry_start,
                end: entry_end,
                action: shortcut.action,
            });
            cursor = entry_end;
        }
        FooterLineData {
            line: Line::from(spans),
            segments,
            total_width: cursor,
        }
    }

    fn execute_footer_action(&mut self, action: FooterAction) {
        match action {
            FooterAction::Quit => self.should_quit = true,
            FooterAction::Edit => self.queue_edit_current(),
            FooterAction::Execute => self.prepare_command(),
            FooterAction::NewItem => self.queue_new_item(),
            FooterAction::NewBox => self.queue_new_box(),
            FooterAction::Delete => self.delete_selected_item(),
            FooterAction::Settings => self.queue_settings(),
            FooterAction::ScanBin => self.run_bin_scan(),
        }
    }

    fn queue_new_item(&mut self) {
        self.pending_action = Some(DeferredAction::NewItem);
    }

    fn queue_new_box(&mut self) {
        self.pending_action = Some(DeferredAction::EditCategory {
            category_index: None,
        });
    }

    fn queue_edit_current(&mut self) {
        if let Some(entry) = self.display_entries.get(self.current_index) {
            match entry {
                DisplayEntry::Item {
                    category_index,
                    item_index,
                } if self.categories[*category_index].kind == BoxKind::Menu => {
                    self.pending_action = Some(DeferredAction::EditItem {
                        category_index: *category_index,
                        item_index: *item_index,
                    });
                }
                DisplayEntry::Item { category_index, .. }
                | DisplayEntry::Category { category_index } => {
                    self.pending_action = Some(DeferredAction::EditCategory {
                        category_index: Some(*category_index),
                    });
                }
            }
        }
    }

    fn queue_settings(&mut self) {
        self.queue_settings_with_focus(SettingsField::Title);
    }

    fn queue_settings_with_focus(&mut self, focus: SettingsField) {
        self.pending_action = Some(DeferredAction::Settings(focus));
    }

    fn show_info_popup(&mut self) {
        if let Some(DisplayEntry::Item {
            category_index,
            item_index,
        }) = self.display_entries.get(self.current_index)
        {
            let category = &self.categories[*category_index];
            if category.kind.uses_folder() {
                let command = self
                    .selected_command()
                    .map(|(_, pending)| pending.command)
                    .unwrap_or_else(|| "(no app set for this folder)".into());
                self.active_popup = Some(PopupState::Info(InfoPopup {
                    label: category.row_label(*item_index),
                    command,
                    category: category.name.clone(),
                    description: category
                        .files
                        .get(*item_index)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                }));
                return;
            }
        }
        if let Some((category_index, item_index)) = self.selected_item_indices() {
            let item = &self.categories[category_index].items[item_index];
            self.active_popup = Some(PopupState::Info(InfoPopup {
                label: item.label.clone(),
                command: item.cmd.clone(),
                category: self.categories[category_index].name.clone(),
                description: item.info.clone(),
            }));
        }
    }

    /// The selected menu item, if the selection is a row of a Menu box.
    fn selected_item_indices(&self) -> Option<(usize, usize)> {
        if let Some(DisplayEntry::Item {
            category_index,
            item_index,
        }) = self.display_entries.get(self.current_index)
        {
            if self.categories[*category_index].kind == BoxKind::Menu {
                return Some((*category_index, *item_index));
            }
        }
        None
    }

    /// The box the current selection belongs to.
    fn selected_category_index(&self) -> Option<usize> {
        match self.display_entries.get(self.current_index)? {
            DisplayEntry::Category { category_index }
            | DisplayEntry::Item { category_index, .. } => Some(*category_index),
        }
    }

    fn delete_selected_item(&mut self) {
        if let Some(DisplayEntry::Category { category_index }) =
            self.display_entries.get(self.current_index)
        {
            let name = self.categories[*category_index].name.clone();
            self.active_popup = Some(PopupState::Confirm(ConfirmPopup {
                message: format!("Delete the box \"{name}\" and everything in it?"),
                action: ConfirmAction::DeleteBox(*category_index),
            }));
            return;
        }
        if let Some((category_index, item_index)) = self.selected_item_indices() {
            self.categories[category_index].items.remove(item_index);
            if self.current_index >= self.display_entries.len().saturating_sub(1) {
                self.current_index = self.current_index.saturating_sub(1);
            }
            let _ = self.save_menu();
            self.sort_categories();
            self.rebuild_display();
            self.set_status(Some("Item deleted".into()));
        } else if self.selected_category_index().is_some() {
            self.set_status(Some(
                "Files are listed from the folder; select the box title to delete the box".into(),
            ));
        }
    }

    fn save_notes(&mut self) {
        if let Some(PopupState::Notes(editor)) = self.active_popup.take() {
            if let Some(file) = &editor.file {
                let text = editor.text();
                let name = editor.box_name.clone();
                // A brand-new document left empty is not created.
                let msg = if text.is_empty() && !file.exists() {
                    "Empty document not saved".to_string()
                } else {
                    let result = file
                        .parent()
                        .map(fs::create_dir_all)
                        .unwrap_or(Ok(()))
                        .and_then(|_| fs::write(file, format!("{text}\n")));
                    match result {
                        Ok(()) => format!("Saved {name}"),
                        Err(err) => format!("Could not save {name}: {err}"),
                    }
                };
                if let Some(category) = self.categories.get_mut(editor.category_index) {
                    category.expanded = true;
                }
                self.rebuild_display();
                self.set_status(Some(msg));
                return;
            }
            if let Some(category) = self.categories.get_mut(editor.category_index) {
                category.notes = editor.text();
                category.expanded = true;
                self.rebuild_display();
                let _ = self.save_menu();
                self.set_status(Some(format!("Notes saved to \"{}\"", editor.box_name)));
            }
        }
    }

    fn delete_box(&mut self, category_index: usize) {
        if category_index >= self.categories.len() {
            return;
        }
        let removed = self.categories.remove(category_index);
        self.rebuild_display();
        let _ = self.save_menu();
        self.set_status(Some(format!("Box \"{}\" deleted", removed.name)));
    }

    /// Order the boxes by column, then by their place in the column, and
    /// renumber `order` so new or moved boxes get settled positions.
    fn sort_categories(&mut self) {
        self.categories
            .sort_by_key(|category| (category.column, category.order, category.name.clone()));
        for (index, category) in self.categories.iter_mut().enumerate() {
            category.order = index as u32;
        }
    }

    fn ensure_category(&mut self, name: &str) -> usize {
        if let Some(idx) = self.categories.iter().position(|c| c.name == name) {
            return idx;
        }
        self.categories
            .push(CategoryState::new_box(name, BoxKind::Menu, 1));
        self.categories.len() - 1
    }

    fn upsert_saved_theme(&mut self, saved: SavedTheme) -> usize {
        if let Some(idx) = self
            .saved_themes
            .iter()
            .position(|theme| theme.name == saved.name)
        {
            self.saved_themes[idx] = saved;
            idx
        } else {
            self.saved_themes.push(saved);
            self.saved_themes.len() - 1
        }
    }

    fn delete_saved_theme(&mut self, index: usize) {
        if index >= self.saved_themes.len() {
            return;
        }
        self.saved_themes.remove(index);
        if let Some(old_index) = parse_saved_theme_key(&self.theme_key) {
            if old_index == index {
                if let Some(fallback) = Theme::from_name("nord") {
                    self.theme = fallback.clone();
                    self.theme_key = "nord".into();
                    let _ = self.theme.save(&self.paths.theme_file);
                }
            } else if old_index > index {
                self.theme_key = saved_theme_key(old_index - 1);
            }
        }
    }

    fn handle_saved_theme_deletion(&mut self, index: usize) {
        self.delete_saved_theme(index);
        let new_options = self.theme_options();
        let new_index = new_options
            .iter()
            .position(|opt| opt.key == self.theme_key)
            .unwrap_or(0);
        if let Some(PopupState::SettingsForm(form)) = self.active_popup.as_mut() {
            form.theme_options = new_options;
            form.theme_index = new_index;
            form.selected_field = SettingsField::Theme;
            form.populate_custom_fields_from_selection();
        }
        self.set_status(Some("Custom theme deleted".into()));
    }

    fn execute_deferred_action<B>(
        &mut self,
        _terminal: &mut Terminal<B>,
        action: DeferredAction,
    ) -> Result<()>
    where
        B: ratatui::backend::Backend + Write,
    {
        match action {
            DeferredAction::NewItem => self.open_item_form(None),
            DeferredAction::EditItem {
                category_index,
                item_index,
            } => self.open_item_form(Some((category_index, item_index))),
            DeferredAction::EditCategory { category_index } => {
                let presets = self.available_color_presets();
                let column = self
                    .selected_category_index()
                    .map(|idx| self.categories[idx].column)
                    .unwrap_or(1);
                let blank = CategoryState::new_box("", BoxKind::Menu, column);
                let category = match category_index {
                    Some(index) => self.categories.get(index),
                    None => Some(&blank),
                };
                if let Some(category) = category {
                    let mut form = CategoryFormState::new(category_index, category, presets);
                    form.paintings_dir = paintings_dir(&self.paths.config_dir);
                    self.active_popup = Some(PopupState::CategoryForm(form));
                }
            }
            DeferredAction::Settings(focus) => self.prompt_settings(focus)?,
        }
        Ok(())
    }

    fn open_item_form(&mut self, target: Option<(usize, usize)>) {
        let mut default_categories: Vec<String> = self
            .categories
            .iter()
            .filter(|c| c.kind == BoxKind::Menu)
            .map(|c| c.name.clone())
            .collect();
        if default_categories.is_empty() {
            default_categories.push("General".to_string());
        }
        // New items go into the menu box that is currently selected.
        if let Some(selected) = self
            .selected_category_index()
            .filter(|idx| self.categories[*idx].kind == BoxKind::Menu)
        {
            let name = self.categories[selected].name.clone();
            if let Some(pos) = default_categories.iter().position(|n| *n == name) {
                default_categories.swap(0, pos);
                default_categories[1..].sort();
            }
        }

        let (default_label, default_cmd, default_info, default_category, default_pause) =
            if let Some((cat_idx, item_idx)) = target {
                let category_name = self.categories[cat_idx].name.clone();
                let item = &self.categories[cat_idx].items[item_idx];
                (
                    item.label.clone(),
                    item.cmd.clone(),
                    item.info.clone(),
                    category_name,
                    item.pause,
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    String::new(),
                    default_categories
                        .get(0)
                        .cloned()
                        .unwrap_or_else(|| "General".into()),
                    false,
                )
            };
        let default_clear_screen = if let Some((cat_idx, item_idx)) = target {
            self.categories[cat_idx].items[item_idx].clear_screen
        } else {
            false
        };

        let fallback_category = default_categories
            .get(0)
            .cloned()
            .unwrap_or_else(|| "General".into());
        let initial_category = if default_category.trim().is_empty() {
            fallback_category.clone()
        } else {
            default_category.clone()
        };

        let form = ItemFormState::new(
            target,
            default_label,
            default_cmd,
            default_info,
            initial_category,
            fallback_category,
            default_pause,
            default_clear_screen,
            default_categories,
        );
        self.active_popup = Some(PopupState::ItemForm(form));
    }

    fn prompt_settings(&mut self, focus: SettingsField) -> Result<()> {
        let options = self.theme_options();
        let is_custom = self.theme_key == CUSTOM_THEME_KEY;
        self.active_popup = Some(PopupState::SettingsForm(SettingsFormState::new(
            self.title.clone(),
            self.column_count,
            self.drag_boxes,
            self.theme_key.clone(),
            options,
            focus,
            &self.theme,
            is_custom,
        )
        .with_pages(self.pages_enabled, self.page_names())));
        Ok(())
    }

    fn run_bin_scan(&mut self) {
        if let Err(err) = self.scan_bin_directory() {
            self.set_status(Some(format!("Bin scan failed: {err}")));
        } else {
            self.set_status(Some("Bin directory scanned".into()));
        }
    }

    fn scan_bin_directory(&mut self) -> Result<()> {
        let source = Path::new("./import");
        if !source.exists() || !source.is_dir() {
            return Ok(());
        }
        let dest = self.paths.config_dir.join("bin");
        fs::create_dir_all(&dest)?;

        let mut existing_commands = HashSet::new();
        for category in &self.categories {
            for item in &category.items {
                existing_commands.insert(item.cmd.clone());
            }
        }

        let mut new_items = Vec::new();
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || !is_executable_file(&entry) {
                continue;
            }
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };
            let dest_path = dest.join(&filename);
            if dest_path.exists() {
                continue;
            }
            fs::rename(&path, &dest_path)?;
            #[cfg(unix)]
            {
                let mut perms = fs::metadata(&dest_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&dest_path, perms)?;
            }
            let cmd_path = format!("~/.local/menu-maker/bin/{filename}");
            if existing_commands.contains(&cmd_path) {
                continue;
            }
            existing_commands.insert(cmd_path.clone());
            let label = filename_to_label(&filename);
            new_items.push(MenuItem {
                label,
                cmd: cmd_path,
                info: format!("Executable: {filename}"),
                pause: false,
                clear_screen: false,
            });
        }

        if new_items.is_empty() {
            return Ok(());
        }

        let idx = self.ensure_category("Bin Executables");
        self.categories[idx].expanded = true;
        self.categories[idx].items.extend(new_items);
        self.rebuild_display();
        let _ = self.save_menu();
        Ok(())
    }

    fn apply_item_form_input(&mut self, input: ItemFormInput) -> Result<String, String> {
        let label = input.label.trim();
        if label.is_empty() {
            return Err("Label is required".into());
        }
        let command = input.command.trim();
        if command.is_empty() {
            return Err("Command is required".into());
        }
        let info = input.info.trim().to_string();
        let mut category_name = input.category.trim().to_string();
        if category_name.is_empty() {
            category_name = input.fallback_category.trim().to_string();
        }
        if category_name.is_empty() {
            category_name = "General".into();
        }
        if self
            .categories
            .iter()
            .any(|c| c.name == category_name && c.kind != BoxKind::Menu)
        {
            return Err(format!("\"{category_name}\" is not a menu box"));
        }

        let new_item = MenuItem {
            label: label.to_string(),
            cmd: command.to_string(),
            info,
            pause: input.pause,
            clear_screen: input.clear_screen,
        };

        match input.target {
            Some((category_index, item_index)) => {
                if category_index >= self.categories.len() {
                    return Err("Item no longer exists".into());
                }
                if item_index >= self.categories[category_index].items.len() {
                    return Err("Item no longer exists".into());
                }
                let same_category = self.categories[category_index].name == category_name;
                if same_category {
                    self.categories[category_index].items[item_index] = new_item;
                } else {
                    self.categories[category_index].items.remove(item_index);
                    let dest_idx = self.ensure_category(&category_name);
                    self.categories[dest_idx].expanded = true;
                    self.categories[dest_idx].items.push(new_item);
                }
                self.rebuild_display();
                let _ = self.save_menu();
                Ok("Item updated".into())
            }
            None => {
                let idx = self.ensure_category(&category_name);
                self.categories[idx].expanded = true;
                self.categories[idx].items.push(new_item);
                self.rebuild_display();
                let _ = self.save_menu();
                Ok("Item added".into())
            }
        }
    }

    fn apply_category_form_input(&mut self, input: CategoryFormInput) -> Result<String, String> {
        if let Some(index) = input.category_index {
            if index >= self.categories.len() {
                return Err("Box no longer exists".into());
            }
        }
        let new_name = input.name.trim().to_string();
        let new_name = match (new_name.is_empty(), input.category_index) {
            (false, _) => new_name,
            (true, Some(index)) => self.categories[index].name.clone(),
            (true, None) => return Err("Name is required".into()),
        };
        if self
            .categories
            .iter()
            .enumerate()
            .any(|(idx, cat)| Some(idx) != input.category_index && cat.name == new_name)
        {
            return Err("A box with that name already exists".into());
        }
        let column_value = if input.column_value.trim().is_empty() {
            input
                .category_index
                .map(|index| self.categories[index].column)
                .unwrap_or(1)
        } else {
            input
                .column_value
                .trim()
                .parse::<u16>()
                .map_err(|_| "Column must be a number".to_string())?
        }
        .clamp(1, MAX_COLUMNS);
        let span_value = if input.span_value.trim().is_empty() {
            input
                .category_index
                .map(|index| self.categories[index].span)
                .unwrap_or(1)
        } else {
            input
                .span_value
                .trim()
                .parse::<u16>()
                .map_err(|_| "Span must be a number".to_string())?
        }
        .clamp(1, MAX_COLUMNS);
        let max_rows = if input.max_rows.trim().is_empty() {
            None
        } else {
            let rows = input
                .max_rows
                .trim()
                .parse::<u16>()
                .map_err(|_| "Max rows must be a number".to_string())?;
            (rows > 0).then_some(rows)
        };
        let articles = match input.articles.trim() {
            "" => DEFAULT_RSS_ARTICLES,
            text => text
                .parse::<u16>()
                .ok()
                .filter(|n| *n <= 100)
                .ok_or_else(|| "Articles shown must be a number from 0 to 100".to_string())?,
        };
        if input.kind.uses_folder() && input.folder.trim().is_empty() {
            return Err("Folder is required".into());
        }
        if input.kind == BoxKind::Folder && input.app.trim().is_empty() {
            return Err("Choose the app that opens these files (e.g. nano)".into());
        }
        if input.kind == BoxKind::Shortcut && input.app.trim().is_empty() {
            return Err("Command is required".into());
        }

        let background = parse_color_field(&input.background)?;
        let text = parse_color_field(&input.text_color)?;
        let border = parse_color_field(&input.border_color)?;

        let index = match input.category_index {
            Some(index) => index,
            None => {
                self.categories
                    .push(CategoryState::new_box(&new_name, input.kind, column_value));
                self.categories.len() - 1
            }
        };
        let category = &mut self.categories[index];
        category.name = new_name.clone();
        category.kind = input.kind;
        if category.column != column_value {
            // Moved to another column in the editor: it goes to the bottom.
            category.order = u32::MAX;
        }
        category.column = column_value;
        category.span = span_value;
        category.folder = if input.kind.uses_folder() || input.kind.uses_data_file() {
            input.folder.trim().to_string()
        } else {
            String::new()
        };
        category.app = input.app.trim().to_string();
        category.filter = input.filter.trim().to_string();
        category.show_picture = input.show_picture;
        category.name_only = input.kind == BoxKind::Shortcut && input.name_only;
        category.icon = if input.kind == BoxKind::Shortcut {
            input.icon.trim().to_string()
        } else {
            String::new()
        };
        category.max_rows = max_rows;
        category.articles = articles;
        category.pause = input.pause;
        category.clear_screen = input.clear_screen;
        category.style = if input.style.fits(input.kind) {
            input.style
        } else {
            BoxStyle::default_for(input.kind)
        };
        category.colors = match (background, text, border) {
            (None, None, None) => None,
            (bg, txt, border) => Some(ColorConfig {
                background: bg,
                text: txt,
                border,
            }),
        };

        self.rebuild_display();
        // Keep the edited box selected after re-sorting.
        if let Some(pos) = self.display_entries.iter().position(|entry| {
            matches!(entry, DisplayEntry::Category { category_index }
                if self.categories[*category_index].name == new_name)
        }) {
            self.current_index = pos;
        }
        let _ = self.save_menu();
        Ok(if input.category_index.is_some() {
            "Box updated".into()
        } else {
            "Box created".into()
        })
    }

    fn process_category_submission(
        &mut self,
        payload: CategorySubmitPayload,
    ) -> Result<String, String> {
        let mut messages: Vec<String> = Vec::new();
        if let Some(preset_input) = payload.new_preset {
            match self.add_custom_category_preset(preset_input) {
                Ok(msg) => {
                    self.handle_category_preset_add_result(Ok(msg.clone()));
                    messages.push(msg);
                }
                Err(err_msg) => {
                    self.handle_category_preset_add_result(Err(err_msg.clone()));
                    return Err(err_msg);
                }
            }
        }
        match self.apply_category_form_input(payload.form) {
            Ok(msg) => {
                messages.push(msg.clone());
                Ok(messages.join(" | "))
            }
            Err(err_msg) => Err(err_msg),
        }
    }

    fn add_custom_category_preset(&mut self, input: CustomPresetInput) -> Result<String, String> {
        let mut name = input.name.trim().to_string();
        if name.is_empty() {
            name = format!("Custom Theme {}", self.custom_colors.len() + 1);
        }
        self.custom_colors.push(NamedColorPair {
            name: Some(name.clone()),
            background: Some(input.background.clone()),
            text: Some(input.text.clone()),
            border: input.border.clone(),
        });
        let _ = self.save_menu();
        Ok(format!("Theme '{name}' added"))
    }

    fn delete_custom_category_preset(&mut self, index: usize) -> Result<String, String> {
        if index >= self.custom_colors.len() {
            return Err("Custom theme not found".into());
        }
        let removed = self.custom_colors.remove(index);
        let name = removed
            .name
            .clone()
            .unwrap_or_else(|| format!("Custom Theme {}", index + 1));
        let _ = self.save_menu();
        Ok(format!("Theme '{name}' deleted"))
    }

    fn handle_category_preset_add_result(
        &mut self,
        result: Result<String, String>,
    ) {
        match result {
            Ok(msg) => {
                let presets = self.available_color_presets();
                if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
                    form.refresh_presets(presets);
                    if !form.color_presets.is_empty() {
                        form.focus_palette_index(form.color_presets.len() - 1);
                    }
                }
                self.set_status(Some(msg));
            }
            Err(err_msg) => {
                if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
                    form.error = Some(err_msg);
                }
            }
        }
    }

    fn handle_category_preset_delete_result(
        &mut self,
        result: Result<String, String>,
    ) {
        match result {
            Ok(msg) => {
                let presets = self.available_color_presets();
                if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
                    let target_index = form
                        .palette_index
                        .min(presets.len().saturating_sub(1));
                    form.refresh_presets(presets);
                    if !form.color_presets.is_empty() {
                        form.focus_palette_index(target_index);
                    } else {
                        form.selected_field = CategoryField::Name;
                    }
                }
                self.set_status(Some(msg));
            }
            Err(err_msg) => {
                if let Some(PopupState::CategoryForm(form)) = self.active_popup.as_mut() {
                    form.error = Some(err_msg);
                }
            }
        }
    }

    fn apply_settings_form_input(&mut self, input: SettingsFormInput) -> Result<String, String> {
        let mut title = input.title.trim().to_string();
        if title.is_empty() {
            title = self.title.clone();
        }
        let columns = if input.columns.trim().is_empty() {
            self.column_count
        } else {
            input
                .columns
                .trim()
                .parse::<u16>()
                .map_err(|_| "Columns must be a number".to_string())?
        }
        .clamp(1, MAX_COLUMNS);
        let mut theme_key = input.theme_key.trim().to_string();
        if theme_key.is_empty() {
            theme_key = self.theme_key.clone();
        }
        // Check the page names before changing anything.
        let page_plan = if input.pages_enabled {
            Some(pages::plan_page_edits(self.pages.len(), &input.page_names)?)
        } else {
            None
        };
        let custom_primary = input.custom_primary.trim();
        let custom_accent = input.custom_accent.trim();
        let custom_background = input.custom_background.trim();
        let custom_surface = input.custom_surface.trim();
        let custom_text = input.custom_text.trim();
        let custom_highlight = input.custom_highlight.trim();
        let custom_theme_name = input.custom_theme_name.trim();
        let theme_options_snapshot = self.theme_options();
        let selected_option = theme_options_snapshot
            .iter()
            .find(|opt| opt.key == theme_key);
        let has_any_color_input = [
            custom_primary,
            custom_accent,
            custom_highlight,
            custom_background,
            custom_surface,
            custom_text,
        ]
        .iter()
        .any(|value| !value.is_empty());
        let colors_match_selected = selected_option
            .map(|option| {
                hex_strings_equal(custom_primary, &option.primary_hex)
                    && hex_strings_equal(custom_accent, &option.accent_hex)
                    && hex_strings_equal(custom_highlight, &option.highlight_hex)
                    && hex_strings_equal(custom_background, &option.background_hex)
                    && hex_strings_equal(custom_surface, &option.surface_hex)
                    && hex_strings_equal(custom_text, &option.text_hex)
            })
            .unwrap_or(false);
        let name_matches_selected = if custom_theme_name.is_empty() {
            true
        } else if let Some(option) = selected_option {
            if parse_saved_theme_key(&option.key).is_some() {
                custom_theme_name.eq_ignore_ascii_case(&option.label)
            } else if option.key == CUSTOM_THEME_KEY {
                custom_theme_name.eq_ignore_ascii_case(&self.theme.name)
            } else {
                false
            }
        } else {
            false
        };
        let use_custom_colors = if has_any_color_input {
            selected_option
                .map(|_| !colors_match_selected || !name_matches_selected)
                .unwrap_or(true)
        } else if custom_theme_name.is_empty() {
            false
        } else {
            !name_matches_selected || selected_option.is_none()
        };

        let mut changed = false;
        if input.drag_boxes != self.drag_boxes {
            self.drag_boxes = input.drag_boxes;
            changed = true;
        }
        if title != self.title {
            self.title = title;
            changed = true;
        }
        if columns != self.column_count {
            self.column_count = columns;
            self.rebuild_display();
            changed = true;
        }
        if let Some(plan) = page_plan {
            changed |= self.apply_page_plan(plan);
        }
        if input.pages_enabled != self.pages_enabled {
            self.pages_enabled = input.pages_enabled;
            if !self.pages_enabled {
                // Pages off: show the first page; the others are kept.
                if self.show_page(0) {
                    self.rebuild_display();
                }
                self.box_drag = None;
            }
            changed = true;
        }
        if use_custom_colors {
            let primary = require_color_field(custom_primary, "Primary")?;
            let accent = require_color_field(custom_accent, "Accent")?;
            let highlight = require_color_field(custom_highlight, "Highlight")?;
            let background = require_color_field(custom_background, "Background")?;
            let surface = require_color_field(custom_surface, "Surface")?;
            let text_color = require_color_field(custom_text, "Text")?;
            let theme_name = if custom_theme_name.is_empty() {
                "Custom Theme".to_string()
            } else {
                custom_theme_name.to_string()
            };
            let theme = Theme::from_hexes(
                theme_name.clone(),
                &primary,
                &accent,
                &highlight,
                &background,
                &surface,
                &text_color,
            );
            let mut new_theme_key = CUSTOM_THEME_KEY.to_string();
            if !custom_theme_name.is_empty() {
                let saved = SavedTheme {
                    name: theme_name.clone(),
                    primary,
                    accent,
                    highlight: Some(highlight.clone()),
                    background,
                    surface,
                    text: text_color,
                };
                let index = self.upsert_saved_theme(saved);
                new_theme_key = saved_theme_key(index);
            }
            self.theme = theme.clone();
            self.theme_key = new_theme_key;
            let _ = self.theme.save(&self.paths.theme_file);
            changed = true;
        } else {
            if let Some(index) = parse_saved_theme_key(&theme_key) {
                if parse_saved_theme_key(&self.theme_key) != Some(index) {
                    if let Some(theme) = self.theme_from_saved_index(index) {
                        self.theme = theme.clone();
                        self.theme_key = saved_theme_key(index);
                        let _ = self.theme.save(&self.paths.theme_file);
                        changed = true;
                    } else {
                        return Err("Saved theme not found".into());
                    }
                }
            } else if theme_key == CUSTOM_THEME_KEY {
                if self.theme_key != CUSTOM_THEME_KEY {
                    return Err("Enter custom colors to create a custom theme".into());
                }
            } else if theme_key != self.theme_key {
                let theme = Theme::from_name(&theme_key)
                    .ok_or_else(|| "Unknown theme selected".to_string())?;
                self.theme = theme.clone();
                self.theme_key = theme_key.clone();
                let _ = self.theme.save(&self.paths.theme_file);
                changed = true;
            }
        }

        let _ = self.save_menu();
        if changed {
            Ok("Settings updated".into())
        } else {
            Ok("No settings changed".into())
        }
    }
}

#[derive(Clone)]
struct PendingCommand {
    command: String,
    pause: bool,
    clear_screen: bool,
}

#[derive(Clone)]
struct MenuItem {
    label: String,
    cmd: String,
    info: String,
    pause: bool,
    clear_screen: bool,
}

impl MenuItem {
    fn from_config(category: &str, cfg: &MenuItemConfig) -> Self {
        MenuItem {
            label: cfg.label.clone(),
            cmd: cfg.cmd.clone(),
            info: cfg
                .info
                .clone()
                .unwrap_or_else(|| format!("Item in {category}")),
            pause: cfg.pause.unwrap_or(false),
            clear_screen: cfg.clear_screen.unwrap_or(false),
        }
    }
}

#[derive(Clone)]
struct CategoryState {
    name: String,
    expanded: bool,
    column: u16,
    /// How many columns the box spans horizontally (1 = one column).
    span: u16,
    /// Position within its column; see `AppState::sort_categories`.
    order: u32,
    colors: Option<ColorConfig>,
    items: Vec<MenuItem>,
    kind: BoxKind,
    folder: String,
    app: String,
    filter: String,
    notes: String,
    max_rows: Option<u16>,
    pause: bool,
    clear_screen: bool,
    /// Files currently listed by a Folder / Apps box (refreshed on rebuild).
    files: Vec<PathBuf>,
    /// Why a Folder / Apps box has no listing (e.g. "folder not found").
    folder_error: Option<String>,
    /// Address Book names / Calendar mini-month lines shown in the box, with
    /// an optional highlighted (start, len) character range.
    preview: Vec<(String, Option<(usize, usize)>)>,
    /// Shortcut boxes: path of the icon picture (blank = text button).
    icon: String,
    /// The loaded icon picture (refreshed on rebuild).
    icon_picture: Option<Rc<Picture>>,
    /// Or an enlarged glyph icon (`glyph:...` in the icon field).
    icon_glyph: Option<GlyphIcon>,
    /// Shortcut boxes: just the name, centered inside the box (a button).
    name_only: bool,
    /// Paint boxes: show the selected drawing as a picture under the list.
    show_picture: bool,
    /// Paint boxes: the picture of each listed file (None for images or
    /// unreadable files), parallel to `files`.
    pictures: Vec<Option<Rc<Picture>>>,
    /// Paint boxes: the drawing last selected. It stays on show after the
    /// cursor moves off the box.
    shown_picture: RefCell<Option<PathBuf>>,
    /// Calculator boxes: the calculator's state.
    calc: Calculator,
    /// RSS boxes: how many articles to show (0 = list the feeds).
    articles: u16,
    /// RSS boxes: the articles shown, newest first.
    headlines: Vec<rss::Headline>,
    /// Address Book boxes: each contact's first phone number.
    phones: Vec<String>,
    /// How the box is drawn.
    style: BoxStyle,
}

impl CategoryState {
    fn from_config(name: &str, cfg: &CategoryConfig) -> Self {
        let column = cfg.column.unwrap_or(1).clamp(1, MAX_COLUMNS);
        let span = cfg.span.unwrap_or(1).clamp(1, MAX_COLUMNS);
        let items = cfg
            .items
            .iter()
            .map(|item| MenuItem::from_config(name, item))
            .collect();
        CategoryState {
            name: name.to_string(),
            expanded: cfg.expanded,
            column,
            span,
            order: cfg.order.unwrap_or(u32::MAX),
            colors: cfg.colors.clone(),
            items,
            kind: cfg.kind,
            folder: cfg.folder.clone().unwrap_or_default(),
            app: cfg.app.clone().unwrap_or_default(),
            filter: cfg.filter.clone().unwrap_or_default(),
            notes: cfg.notes.clone().unwrap_or_default(),
            max_rows: cfg.max_rows.filter(|rows| *rows > 0),
            pause: cfg.pause.unwrap_or(false),
            clear_screen: cfg.clear_screen.unwrap_or(false),
            files: Vec::new(),
            folder_error: None,
            preview: Vec::new(),
            icon: cfg.icon.clone().unwrap_or_default(),
            icon_picture: None,
            icon_glyph: None,
            name_only: cfg.name_only,
            show_picture: cfg.show_picture.unwrap_or(true),
            pictures: Vec::new(),
            shown_picture: RefCell::new(cfg.picture.as_deref().map(PathBuf::from)),
            calc: Calculator::new(cfg.notes.as_deref().unwrap_or("")),
            articles: cfg.articles.unwrap_or(DEFAULT_RSS_ARTICLES),
            headlines: Vec::new(),
            phones: Vec::new(),
            style: cfg
                .style
                .filter(|style| style.fits(cfg.kind))
                .unwrap_or_else(|| BoxStyle::default_for(cfg.kind)),
        }
    }

    fn new_box(name: &str, kind: BoxKind, column: u16) -> Self {
        CategoryState {
            name: name.to_string(),
            expanded: true,
            column,
            span: 1,
            order: u32::MAX,
            colors: None,
            items: Vec::new(),
            kind,
            folder: String::new(),
            app: String::new(),
            filter: String::new(),
            notes: String::new(),
            max_rows: None,
            pause: false,
            clear_screen: false,
            files: Vec::new(),
            folder_error: None,
            preview: Vec::new(),
            icon: String::new(),
            icon_picture: None,
            icon_glyph: None,
            name_only: false,
            show_picture: true,
            pictures: Vec::new(),
            shown_picture: RefCell::new(None),
            calc: Calculator::new(""),
            articles: DEFAULT_RSS_ARTICLES,
            headlines: Vec::new(),
            phones: Vec::new(),
            style: BoxStyle::default_for(kind),
        }
    }

    /// The drawing a Paint box shows: the selected row's picture (which is
    /// then remembered), else the remembered one, else the first picture.
    fn preview_picture(&self, selected_row: Option<usize>) -> Option<Rc<Picture>> {
        if let Some(row) = selected_row {
            if let (Some(file), Some(Some(picture))) = (self.files.get(row), self.pictures.get(row)) {
                *self.shown_picture.borrow_mut() = Some(file.clone());
                return Some(picture.clone());
            }
        }
        let remembered = self.shown_picture.borrow().clone();
        remembered
            .and_then(|path| self.files.iter().position(|f| *f == path))
            .and_then(|row| self.pictures.get(row).cloned().flatten())
            .or_else(|| self.pictures.iter().flatten().next().cloned())
    }

    /// The style actually drawn for a box `outer_width` cells wide.
    fn drawn_style(&self, outer_width: u16) -> BoxStyle {
        if self.placeholder().is_some() && self.kind != BoxKind::Notes {
            return BoxStyle::Classic;
        }
        self.style.effective(self.kind, self.expanded, outer_width)
    }

    /// An RSS box showing articles (title + first line) rather than feeds.
    fn shows_articles(&self) -> bool {
        self.kind == BoxKind::Rss && self.articles > 0
    }

    /// Screen lines one row of the box takes: two for an article (title,
    /// then the first line of its text), one otherwise.
    fn row_lines(&self) -> usize {
        if self.shows_articles() {
            2
        } else {
            1
        }
    }

    /// The second line of a two-line row (an article's first line of text).
    fn row_detail(&self, row: usize) -> String {
        self.headlines
            .get(row)
            .map(|h| h.snippet.clone())
            .unwrap_or_default()
    }

    /// A Spreadsheet box lists every .csv in a folder unless its Folder field
    /// names a single .csv file.
    fn sheet_is_folder(&self) -> bool {
        !self.folder.trim().to_ascii_lowercase().ends_with(".csv")
    }

    /// Where an Address Book / Calendar box keeps its data. The Folder
    /// field holds a custom file; blank uses the config directory.
    fn data_path(&self, config_dir: &Path) -> PathBuf {
        if !self.folder.trim().is_empty() {
            return boxes::expand_tilde(&self.folder);
        }
        match self.kind {
            BoxKind::Calendar => config_dir.join("calendar.json"),
            BoxKind::Spreadsheet => spreadsheets_dir(config_dir),
            BoxKind::Rss => config_dir.join("rss.json"),
            BoxKind::Paint => paintings_dir(config_dir),
            BoxKind::Music => music_dir(config_dir),
            BoxKind::Documents => documents_dir(config_dir),
            BoxKind::Weather => config_dir.join("weather.json"),
            BoxKind::Todo => config_dir.join("todos.json"),
            _ => config_dir.join("contacts.vcf"),
        }
    }

    /// Re-read the folder behind a Folder / Apps box, or the data behind an
    /// Address Book / Calendar box.
    fn refresh_files(&mut self, config_dir: &Path) {
        self.preview = match self.kind {
            BoxKind::Contacts => contacts::preview_names(&self.data_path(config_dir))
                .into_iter()
                .map(|name| (name, None))
                .collect(),
            BoxKind::Calendar => calendar::preview_lines(&self.data_path(config_dir)),
            BoxKind::Spreadsheet if !self.sheet_is_folder() => {
                spreadsheet::preview_lines(&self.data_path(config_dir))
            }
            BoxKind::Rss if !self.shows_articles() => rss::preview_lines(&self.data_path(config_dir)),
            BoxKind::Weather => weather::preview_lines(&self.data_path(config_dir)),
            BoxKind::Todo => todo::preview_lines(&self.data_path(config_dir)),
            _ => Vec::new(),
        };
        self.icon_glyph = (self.kind == BoxKind::Shortcut)
            .then(|| GlyphIcon::parse(&self.icon))
            .flatten();
        self.icon_picture = (self.kind == BoxKind::Shortcut
            && !self.icon.trim().is_empty()
            && self.icon_glyph.is_none())
            .then(|| paint::load_picture(&resolve_painting(&self.icon, config_dir)))
            .flatten()
            .map(Rc::new);
        self.phones = if self.kind == BoxKind::Contacts {
            contacts::preview_phones(&self.data_path(config_dir))
        } else {
            Vec::new()
        };
        self.headlines = if self.shows_articles() {
            rss::headlines(&self.data_path(config_dir), self.articles as usize)
        } else {
            Vec::new()
        };
        if self.kind == BoxKind::Documents
            || (self.kind == BoxKind::Spreadsheet && self.sheet_is_folder())
        {
            let dir = self.data_path(config_dir);
            let _ = fs::create_dir_all(&dir);
            let filter = if self.kind == BoxKind::Spreadsheet { "*.csv" } else { "" };
            match boxes::list_folder(&dir.to_string_lossy(), filter, false) {
                Ok(files) => {
                    self.files = if self.kind == BoxKind::Documents {
                        files.into_iter().filter(|f| boxes::is_text_file(f)).collect()
                    } else {
                        files
                    };
                    self.folder_error = None;
                }
                Err(err) => {
                    self.files.clear();
                    self.folder_error = Some(err);
                }
            }
            return;
        } else if self.kind == BoxKind::Spreadsheet {
            self.files.clear();
            self.folder_error = None;
        }
        if self.kind == BoxKind::Music {
            let dir = self.data_path(config_dir);
            let _ = fs::create_dir_all(&dir);
            match boxes::list_folder(&dir.to_string_lossy(), "", false) {
                Ok(files) => {
                    self.files = files.into_iter().filter(|f| midi::is_midi_file(f)).collect();
                    self.folder_error = None;
                }
                Err(err) => {
                    self.files.clear();
                    self.folder_error = Some(err);
                }
            }
            return;
        }
        if self.kind == BoxKind::Paint {
            let dir = self.data_path(config_dir);
            let _ = fs::create_dir_all(&dir);
            match boxes::list_folder(&dir.to_string_lossy(), "", false) {
                Ok(files) => {
                    self.files = files.into_iter().filter(|f| paint::is_paintable(f)).collect();
                    self.pictures = self
                        .files
                        .iter()
                        .map(|file| paint::load_picture(file).map(Rc::new))
                        .collect();
                    self.folder_error = None;
                }
                Err(err) => {
                    self.files.clear();
                    self.folder_error = Some(err);
                }
            }
            return;
        }
        if !self.kind.uses_folder() {
            self.files.clear();
            self.folder_error = None;
            return;
        }
        match boxes::list_folder(&self.folder, &self.filter, self.kind == BoxKind::Apps) {
            Ok(files) => {
                self.files = files;
                self.folder_error = None;
            }
            Err(err) => {
                self.files.clear();
                self.folder_error = Some(err);
            }
        }
    }

    /// Boxes whose rows are files (these can list them in several columns).
    fn lists_files(&self) -> bool {
        matches!(
            self.kind,
            BoxKind::Folder
                | BoxKind::Apps
                | BoxKind::Paint
                | BoxKind::Music
                | BoxKind::Documents
                | BoxKind::Spreadsheet
        )
    }

    /// Number of selectable rows inside the box.
    fn row_count(&self) -> usize {
        match self.kind {
            BoxKind::Menu => self.items.len(),
            BoxKind::Folder
            | BoxKind::Apps
            | BoxKind::Paint
            | BoxKind::Music
            | BoxKind::Documents
            | BoxKind::Spreadsheet => self.files.len(),
            BoxKind::Rss if self.shows_articles() => self.headlines.len(),
            BoxKind::Contacts | BoxKind::Rss => self.preview.len(),
            BoxKind::Shortcut | BoxKind::Notes | BoxKind::Calendar | BoxKind::Calculator => 0,
            BoxKind::Weather | BoxKind::Todo => 0,
        }
    }

    fn row_label(&self, row: usize) -> String {
        match self.kind {
            BoxKind::Menu => self.items.get(row).map(|i| i.label.clone()).unwrap_or_default(),
            BoxKind::Folder
            | BoxKind::Apps
            | BoxKind::Paint
            | BoxKind::Music
            | BoxKind::Documents
            | BoxKind::Spreadsheet => self
                .files
                .get(row)
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            BoxKind::Rss if self.shows_articles() => {
                self.headlines.get(row).map(|h| h.title.clone()).unwrap_or_default()
            }
            BoxKind::Contacts | BoxKind::Rss => {
                self.preview.get(row).map(|(name, _)| name.clone()).unwrap_or_default()
            }
            BoxKind::Shortcut | BoxKind::Notes | BoxKind::Calendar | BoxKind::Calculator => {
                String::new()
            }
            BoxKind::Weather | BoxKind::Todo => String::new(),
        }
    }

    /// Text shown in an expanded box that has no selectable rows.
    fn placeholder(&self) -> Option<String> {
        match self.kind {
            BoxKind::Menu if self.items.is_empty() => Some("(empty - press n to add)".into()),
            BoxKind::Folder | BoxKind::Apps if self.files.is_empty() => Some(format!(
                "({})",
                self.folder_error.as_deref().unwrap_or("no files")
            )),
            BoxKind::Notes if self.notes.trim().is_empty() => {
                Some("(empty - press Enter to write)".into())
            }
            BoxKind::Contacts if self.preview.is_empty() => {
                Some("(no contacts - press Enter to open)".into())
            }
            BoxKind::Paint if self.files.is_empty() => Some(match &self.folder_error {
                Some(err) => format!("({err})"),
                None => "(no drawings - press Enter to draw)".into(),
            }),
            BoxKind::Music if self.files.is_empty() => Some(match &self.folder_error {
                Some(err) => format!("({err})"),
                None => "(no songs - press Enter to compose)".into(),
            }),
            BoxKind::Spreadsheet if self.sheet_is_folder() && self.files.is_empty() => {
                Some(match &self.folder_error {
                    Some(err) => format!("({err})"),
                    None => "(no sheets - press Enter to start one)".into(),
                })
            }
            BoxKind::Spreadsheet if !self.sheet_is_folder() && self.preview.is_empty() => {
                Some("(empty sheet - press Enter to open)".into())
            }
            BoxKind::Documents if self.files.is_empty() => Some(match &self.folder_error {
                Some(err) => format!("({err})"),
                None => "(no documents - press Enter to write one)".into(),
            }),
            BoxKind::Rss if self.shows_articles() && self.headlines.is_empty() => {
                Some("(no articles yet - press Enter to open the reader and sync)".into())
            }
            BoxKind::Rss if !self.shows_articles() && self.preview.is_empty() => {
                Some("(no feeds - press Enter to add one)".into())
            }
            _ => None,
        }
    }

    fn to_config(&self) -> CategoryConfig {
        CategoryConfig {
            expanded: self.expanded,
            column: Some(self.column),
            span: (self.span > 1).then_some(self.span),
            order: Some(self.order),
            items: self
                .items
                .iter()
                .map(|item| MenuItemConfig {
                    label: item.label.clone(),
                    cmd: item.cmd.clone(),
                    info: Some(item.info.clone()),
                    category: Some(self.name.clone()),
                    pause: Some(item.pause),
                    clear_screen: Some(item.clear_screen),
                })
                .collect(),
            colors: self.colors.clone(),
            kind: self.kind,
            folder: non_empty(&self.folder),
            app: non_empty(&self.app),
            filter: non_empty(&self.filter),
            notes: non_empty(&self.notes),
            icon: non_empty(&self.icon),
            name_only: self.kind == BoxKind::Shortcut && self.name_only,
            show_picture: (self.kind == BoxKind::Paint).then_some(self.show_picture),
            picture: self
                .shown_picture
                .borrow()
                .as_ref()
                .filter(|_| self.kind == BoxKind::Paint)
                .map(|path| path.to_string_lossy().to_string()),
            max_rows: self.max_rows,
            articles: (self.kind == BoxKind::Rss).then_some(self.articles),
            pause: self.kind.launches().then_some(self.pause),
            clear_screen: self.kind.launches().then_some(self.clear_screen),
            // Only an explicit choice is saved, so the default can change later.
            style: (self.style.fits(self.kind) && self.style != BoxStyle::default_for(self.kind))
                .then_some(self.style),
        }
    }
}

/// How many articles a new RSS box shows.
const DEFAULT_RSS_ARTICLES: u16 = 5;

/// The user-facing data folder in the home directory (~/MenuMaker).
const MENUMAKER_DIR_NAME: &str = "MenuMaker";
const APPS_FOLDER: &str = "Apps";
const ART_FOLDER: &str = "Art";
const MUSIC_FOLDER: &str = "Music";
const DOCUMENTS_FOLDER: &str = "Documents";
const SPREADSHEETS_FOLDER: &str = "Spreadsheets";

/// Boxes added once, the first time the app runs, for the ~/MenuMaker folders.
const MENUMAKER_BOXES: [(&str, BoxKind); 5] = [
    ("Programs", BoxKind::Apps),
    ("Art", BoxKind::Paint),
    ("Music", BoxKind::Music),
    ("Documents", BoxKind::Documents),
    ("Spreadsheets", BoxKind::Spreadsheet),
];

/// ~/MenuMaker (falls back to ./MenuMaker when there is no home directory).
fn menumaker_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(MENUMAKER_DIR_NAME)
}

/// Create ~/MenuMaker and its Apps, Art, Music, Documents and Spreadsheets
/// folders. A misspelled "Spreadhsheets" folder from earlier is renamed.
fn create_menumaker_folders(root: &Path) {
    let misspelled = root.join("Spreadhsheets");
    let sheets = root.join(SPREADSHEETS_FOLDER);
    if misspelled.is_dir() && !sheets.exists() {
        let _ = fs::rename(&misspelled, &sheets);
    }
    for folder in [APPS_FOLDER, ART_FOLDER, MUSIC_FOLDER, DOCUMENTS_FOLDER, SPREADSHEETS_FOLDER] {
        let _ = fs::create_dir_all(root.join(folder));
    }
}

/// Copy drawings, songs and the sheet kept in the old config-folder
/// locations into ~/MenuMaker so they show in the new boxes. Nothing is
/// deleted, existing files are never overwritten, and editor backups (`~`)
/// are skipped.
fn copy_legacy_data(config_dir: &Path, root: &Path) {
    let copy_dir = |from: PathBuf, to: PathBuf| {
        let Ok(entries) = fs::read_dir(&from) else {
            return;
        };
        let _ = fs::create_dir_all(&to);
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if !path.is_file() || name.to_string_lossy().ends_with('~') {
                continue;
            }
            let dest = to.join(&name);
            if !dest.exists() {
                let _ = fs::copy(&path, &dest);
            }
        }
    };
    copy_dir(config_dir.join("paintings"), root.join(ART_FOLDER));
    copy_dir(config_dir.join("music"), root.join(MUSIC_FOLDER));
    let sheet = config_dir.join("spreadsheet.csv");
    let dest = root.join(SPREADSHEETS_FOLDER).join("spreadsheet.csv");
    if sheet.is_file() && !dest.exists() {
        let _ = fs::create_dir_all(root.join(SPREADSHEETS_FOLDER));
        let _ = fs::copy(&sheet, &dest);
    }
}

/// Where Tab (`forward`) or Shift+Tab lands: the title of the next or
/// previous box in screen order, wrapping around. None when there are no boxes.
fn box_jump(entries: &[DisplayEntry], current: usize, forward: bool) -> Option<usize> {
    let titles: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry, DisplayEntry::Category { .. }))
        .map(|(index, _)| index)
        .collect();
    if titles.is_empty() {
        return None;
    }
    // The box the selection is in (its title is at or above it).
    let here = titles.iter().rposition(|&title| title <= current).unwrap_or(0);
    let next = if forward {
        (here + 1) % titles.len()
    } else {
        (here + titles.len() - 1) % titles.len()
    };
    Some(titles[next])
}

/// A file name in `dir` that does not exist yet: `<stem>_<n>.<ext>`.
fn new_file_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    (1..)
        .map(|n| dir.join(format!("{stem}_{n}.{ext}")))
        .find(|path| !path.exists())
        .unwrap_or_else(|| dir.join(format!("{stem}.{ext}")))
}

/// Where the music app keeps songs by default (~/MenuMaker/Music).
fn music_dir(_config_dir: &Path) -> PathBuf {
    menumaker_dir().join(MUSIC_FOLDER)
}

/// Where Paint keeps drawings by default (~/MenuMaker/Art).
fn paintings_dir(_config_dir: &Path) -> PathBuf {
    menumaker_dir().join(ART_FOLDER)
}

/// Where Documents boxes keep text files by default (~/MenuMaker/Documents).
fn documents_dir(_config_dir: &Path) -> PathBuf {
    menumaker_dir().join(DOCUMENTS_FOLDER)
}

/// Where Spreadsheet boxes keep .csv sheets by default
/// (~/MenuMaker/Spreadsheets).
fn spreadsheets_dir(_config_dir: &Path) -> PathBuf {
    menumaker_dir().join(SPREADSHEETS_FOLDER)
}

/// A painting path from a box field: `~/...` and absolute paths as given, a
/// bare name inside the paintings folder (`.ans` added when missing).
fn resolve_painting(value: &str, config_dir: &Path) -> PathBuf {
    let value = value.trim();
    if value.starts_with('~') || value.starts_with('/') || value.contains('/') {
        return boxes::expand_tilde(value);
    }
    let path = paintings_dir(config_dir).join(value);
    if path.extension().is_none() {
        path.with_extension("ans")
    } else {
        path
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| value.to_string())
}

enum DisplayEntry {
    Category {
        category_index: usize,
    },
    Item {
        category_index: usize,
        item_index: usize,
    },
}

#[derive(Clone)]
struct InfoPopup {
    label: String,
    command: String,
    category: String,
    description: String,
}

struct ItemFormState {
    target: Option<(usize, usize)>,
    label: String,
    command: String,
    info: String,
    category: String,
    fallback_category: String,
    pause: bool,
    clear_screen: bool,
    available_categories: Vec<String>,
    available_category_index: usize,
    selected_field: ItemField,
    error: Option<String>,
    mode_label: &'static str,
}

#[derive(Clone)]
struct ItemFormInput {
    target: Option<(usize, usize)>,
    label: String,
    command: String,
    info: String,
    category: String,
    fallback_category: String,
    pause: bool,
    clear_screen: bool,
}

#[derive(Default)]
struct ItemFormLayout {
    line_count: usize,
    label_line: Option<usize>,
    command_line: Option<usize>,
    description_line: Option<usize>,
    category_line: Option<usize>,
    clear_screen_line: Option<usize>,
    pause_line: Option<usize>,
    available_heading_line: Option<usize>,
    available_start_line: Option<usize>,
    available_count: usize,
    shortcut_line: Option<Line<'static>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemField {
    Label,
    Command,
    Description,
    Category,
    AvailableCategories,
    ClearScreen,
    Pause,
}

enum PopupState {
    Info(InfoPopup),
    Message(String),
    ItemForm(ItemFormState),
    CategoryForm(CategoryFormState),
    SettingsForm(SettingsFormState),
    Notes(NotesEditor),
    Confirm(ConfirmPopup),
    Contacts(Box<ContactsApp>),
    Calendar(Box<CalendarApp>),
    Spreadsheet(Box<SpreadsheetApp>),
    Rss(Box<RssApp>),
    Weather(Box<WeatherApp>),
    Todo(Box<TodoApp>),
    /// Paint draws through `&mut self`, but rendering only has `&AppState`.
    Paint(Box<RefCell<PaintApp>>),
    /// The music app (miditui); same RefCell reason as Paint.
    Midi(Box<RefCell<MidiApp>>),
}

struct ConfirmPopup {
    message: String,
    action: ConfirmAction,
}

enum ConfirmAction {
    DeleteBox(usize),
}

enum DeferredAction {
    NewItem,
    EditItem {
        category_index: usize,
        item_index: usize,
    },
    /// Edit a box, or create a new one when `category_index` is `None`.
    EditCategory {
        category_index: Option<usize>,
    },
    Settings(SettingsField),
}

enum PopupResult {
    None,
    Close(Option<String>),
    Confirmed(ConfirmAction),
    SaveNotes,
    /// An Address Book / Calendar screen was closed; refresh box previews.
    CloseApp,
    ItemSubmit(ItemFormInput),
    CategorySubmit(CategorySubmitPayload),
    CategoryDeletePreset(usize),
    SettingsSubmit(SettingsFormInput),
    SettingsDeleteSavedTheme(usize),
}

enum PopupClickAction {
    Notes { row: usize, col: usize },
    Item(ItemFormClick),
    Category(CategoryFormClick),
    Settings(SettingsFormClick),
}

enum ItemFormClick {
    SelectField(ItemField),
    SelectAvailableCategory(usize),
}

enum CategoryFormClick {
    SelectField(CategoryField),
    SelectPalette(usize),
    Shortcut(CategoryShortcutAction),
}

enum SettingsFormClick {
    SelectField(SettingsField),
    /// The Pages switch: a click turns it on/off.
    TogglePages,
    SelectTheme(usize),
    DeleteSavedTheme(usize),
    Shortcut(SettingsShortcutAction),
}

#[derive(Clone, Copy)]
struct SettingsShortcutSegment {
    start: u16,
    end: u16,
    action: SettingsShortcutAction,
}

#[derive(Clone, Copy)]
struct CategoryShortcutSegment {
    start: u16,
    end: u16,
    action: CategoryShortcutAction,
}

#[derive(Clone, Copy)]
enum SettingsShortcutAction {
    NextField,
    Submit,
    Cancel,
    PreviousTheme,
    NextTheme,
    DeleteTheme,
}

#[derive(Clone, Copy)]
enum CategoryShortcutAction {
    NextField,
    PreviousField,
    Submit,
    Cancel,
    PreviousPalette,
    NextPalette,
    DeletePreset,
}

struct CategoryFormState {
    /// `None` when creating a new box.
    category_index: Option<usize>,
    name: String,
    kind: BoxKind,
    column_value: String,
    span_value: String,
    folder: String,
    app: String,
    filter: String,
    icon: String,
    /// Open while choosing the icon picture (`e` on the Icon field). In a
    /// RefCell because drawing it updates its scroll position.
    icon_picker: Option<RefCell<FilePicker>>,
    /// Open while choosing an enlarged glyph icon (`g` on the Icon field).
    glyph_picker: Option<GlyphPicker>,
    /// Open while choosing the folder (`F2` on the Folder field).
    folder_picker: Option<RefCell<FilePicker>>,
    name_only: bool,
    /// Where the icon picker starts; drawings there are stored by name.
    paintings_dir: PathBuf,
    show_picture: bool,
    max_rows: String,
    articles: String,
    pause: bool,
    clear_screen: bool,
    /// Style picked in the form; None = the box type's default.
    style: Option<BoxStyle>,
    selected_field: CategoryField,
    error: Option<String>,
    color_presets: Vec<ColorPreset>,
    palette_index: usize,
    custom_preset_name: String,
    custom_preset_background: String,
    custom_preset_text: String,
    custom_preset_border: String,
}

#[derive(Default)]
struct CategoryFormLayout {
    line_count: usize,
    /// (line index, field) for every clickable field line.
    field_lines: Vec<(usize, CategoryField)>,
    shortcut_segments: Vec<CategoryShortcutSegment>,
    shortcut_total_width: u16,
    shortcut_line: Option<Line<'static>>,
    presets_heading_line: Option<usize>,
    presets_start_line: Option<usize>,
    presets_count: usize,
}

#[derive(Clone)]
struct CategoryFormInput {
    category_index: Option<usize>,
    name: String,
    kind: BoxKind,
    column_value: String,
    span_value: String,
    folder: String,
    app: String,
    filter: String,
    icon: String,
    name_only: bool,
    show_picture: bool,
    max_rows: String,
    articles: String,
    pause: bool,
    clear_screen: bool,
    style: BoxStyle,
    background: String,
    text_color: String,
    border_color: String,
}

struct CategorySubmitPayload {
    form: CategoryFormInput,
    new_preset: Option<CustomPresetInput>,
}

#[derive(Clone)]
struct CustomPresetInput {
    name: String,
    background: String,
    text: String,
    border: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CategoryField {
    Name,
    Kind,
    Column,
    Span,
    Folder,
    App,
    Icon,
    NameOnly,
    ShowPicture,
    Filter,
    MaxRows,
    Articles,
    ClearScreen,
    Pause,
    Style,
    CustomPresetName,
    CustomPresetBackground,
    CustomPresetText,
    CustomPresetBorder,
    Palette,
}

enum FormKeyResult {
    Continue,
    Cancel,
    Submit(CategorySubmitPayload),
    DeletePreset(usize),
}

enum ItemFormKeyResult {
    Continue,
    Cancel,
    Submit(ItemFormInput),
}

impl CategoryFormState {
    fn new(index: Option<usize>, category: &CategoryState, presets: Vec<ColorPreset>) -> Self {
        let background = category
            .colors
            .as_ref()
            .and_then(|c| c.background.clone())
            .map(|value| normalize_hex(&value))
            .unwrap_or_default();
        let text = category
            .colors
            .as_ref()
            .and_then(|c| c.text.clone())
            .map(|value| normalize_hex(&value))
            .unwrap_or_default();
        let border = category
            .colors
            .as_ref()
            .and_then(|c| c.border.clone())
            .map(|value| normalize_hex(&value))
            .unwrap_or_default();
        let color_presets = if presets.is_empty() {
            vec![ColorPreset::new("Default", "#034e68", "#caf0f8")]
        } else {
            presets
        };
        let palette_index = if !background.is_empty() && !text.is_empty() {
            color_presets
                .iter()
                .position(|preset| preset.matches(&background, &text, &border))
                .unwrap_or(0)
        } else {
            0
        };        let mut custom_name = String::new();
        if let Some(preset) = color_presets.get(palette_index) {
            custom_name = preset.name.clone();
        } else if !category.name.is_empty() {
            custom_name = format!("{} Colors", category.name);
        }

        Self {
            category_index: index,
            name: category.name.clone(),
            kind: category.kind,
            column_value: category.column.to_string(),
            span_value: category.span.to_string(),
            folder: category.folder.clone(),
            app: category.app.clone(),
            filter: category.filter.clone(),
            icon: category.icon.clone(),
            icon_picker: None,
            glyph_picker: None,
            folder_picker: None,
            name_only: category.name_only,
            paintings_dir: PathBuf::new(),
            show_picture: category.show_picture,
            max_rows: category.max_rows.map(|r| r.to_string()).unwrap_or_default(),
            articles: category.articles.to_string(),
            pause: category.pause,
            clear_screen: category.clear_screen,
            style: (category.style != BoxStyle::default_for(category.kind)).then_some(category.style),
            selected_field: if index.is_some() {
                CategoryField::Name
            } else {
                CategoryField::Kind
            },
            error: None,
            color_presets,
            palette_index,
            custom_preset_name: custom_name,
            custom_preset_background: background,
            custom_preset_text: text,
            custom_preset_border: border,
        }
    }

    fn render_lines(&self, app: &AppState) -> (Vec<FormLine>, CategoryFormLayout) {
        let mut layout = CategoryFormLayout::default();
        let mut lines: Vec<FormLine> = Vec::new();
        lines.push(plain_line(Line::from(if self.category_index.is_some() {
            "Update the box fields below."
        } else {
            "Choose what the new box holds, then fill in its fields."
        })));

        for field in self.fields() {
            let selected = self.selected_field == field;
            let line = match field {
                CategoryField::Name => make_field_line("Name", &self.name, selected, app),
                CategoryField::Kind => make_field_line(
                    "Type (←/→ change)",
                    self.kind.label(),
                    selected,
                    app,
                ),
                CategoryField::Column => {
                    make_field_line("Column", &self.column_value, selected, app)
                }
                CategoryField::Span => {
                    make_field_line("Span (columns)", &self.span_value, selected, app)
                }
                CategoryField::Folder => {
                    let label = match self.kind {
                        BoxKind::Contacts => "Contacts file (.vcf, blank = default)",
                        BoxKind::Calendar => "Calendar file (.json, blank = default)",
                        BoxKind::Paint => "Paintings folder (blank = default)",
                        BoxKind::Music => "Music folder (blank = default)",
                        BoxKind::Spreadsheet => "Sheets folder or one .csv (blank = default)",
                        BoxKind::Documents => "Documents folder (blank = default)",
                        BoxKind::Rss => "Feeds file (.json, blank = default)",
                        _ => "Folder",
                    };
                    make_field_line(label, &self.folder, selected, app)
                }
                CategoryField::App => {
                    let label = if self.kind == BoxKind::Shortcut {
                        "Command"
                    } else {
                        "Open files with (app)"
                    };
                    make_field_line(label, &self.app, selected, app)
                }
                CategoryField::NameOnly => make_toggle_line(
                    "Show only the name inside the box",
                    self.name_only,
                    selected,
                    app,
                ),
                CategoryField::ShowPicture => make_toggle_line(
                    "Show picture of the selected drawing",
                    self.show_picture,
                    selected,
                    app,
                ),
                CategoryField::Icon => make_field_line(
                    "Icon picture (e = drawing, g = glyph, Del = clear)",
                    &GlyphIcon::parse(&self.icon)
                        .map(GlyphIcon::describe)
                        .unwrap_or_else(|| self.icon.clone()),
                    selected,
                    app,
                ),
                CategoryField::Filter => make_field_line(
                    "File filter (e.g. *.md, *.txt; blank = all)",
                    &self.filter,
                    selected,
                    app,
                ),
                CategoryField::MaxRows => make_field_line(
                    "Max rows (blank = fit)",
                    &self.max_rows,
                    selected,
                    app,
                ),
                CategoryField::Articles => make_field_line(
                    "Articles shown (0 = list feeds)",
                    &self.articles,
                    selected,
                    app,
                ),
                CategoryField::ClearScreen => make_toggle_line(
                    "Clear Screen Before Run",
                    self.clear_screen,
                    selected,
                    app,
                ),
                CategoryField::Pause => {
                    make_toggle_line("Pause After Run", self.pause, selected, app)
                }
                CategoryField::Style => make_field_line(
                    "Style (←/→ change)",
                    self.form_style().label(),
                    selected,
                    app,
                ),
                _ => continue,
            };
            layout.field_lines.push((lines.len(), field));
            lines.push(line);
        }
        let hint = match self.kind {
            BoxKind::Folder => "Selecting a file runs: <app> <file>. Put {} in the app to place the file, e.g. vim -R {}. Use @paint to open files in the built-in Paint, or @midi for the music app.",
            BoxKind::Apps => "Every executable file in the folder is listed; selecting one runs it.",
            BoxKind::Shortcut => "Enter or click runs the command. Shows its name in the title band with an icon picture (e = drawing, g = glyph) or the command, or turn on \"Show only the name\" for a plain button.",
            BoxKind::Notes => "Press Enter on the box to write notes.",
            BoxKind::Menu => "Add commands to this box with n (New Item).",
            BoxKind::Contacts => "Lists your contacts; Enter opens the Address Book (add, edit, import/export .vcf).",
            BoxKind::Calendar => "Shows this month; Enter opens the Calendar with appointments and a To-Do list.",
            BoxKind::Paint => "Lists your drawings and shows the selected one as a picture; Enter on one opens it in Paint, Enter on the title starts a new drawing.",
            BoxKind::Music => "Lists your MIDI songs (.mid .midi .oxm); Enter on one opens it in the music app, Enter on the title starts a new song.",
            BoxKind::Calculator => "The Pocket Calculator, right in the box. Click its buttons, or select the box and type numbers and + - * / % (Enter or = for the result, Backspace erases, c clears, _ changes the sign). It keeps its last result.",
            BoxKind::Documents => "Lists the text files in the folder (default ~/MenuMaker/Documents); Enter on one edits it in the Notes editor, Enter on the title starts a new document. Esc or Ctrl+S saves, Ctrl+X discards.",
            BoxKind::Spreadsheet => "Lists the .csv sheets in the folder (default ~/MenuMaker/Spreadsheets), or shows the first rows when set to one .csv file; Enter opens a sheet, Enter on the title starts a new one. Type to fill a cell; start with = for formulas, e.g. =SUM(A1:A5), =AVERAGE(B1:B9), =IF(C1>10,\"hi\",\"lo\"). Saved as .csv.",
            BoxKind::Rss => "Shows the newest articles (set how many with Articles shown): each with its title and the first line of its text; Enter on one reads it. Articles shown 0 lists your feeds with unread counts instead. Enter opens the built-in news reader: feeds, articles and article panels, s sync, o open in browser, r/u read, m/v mark, 1/2/3 filters, / search, z zen, ? help, mouse too. :importopml / :exportopml move feeds to and from other readers.",
            BoxKind::Weather => "Weather like the outside app: current conditions, a daily or 24-hour forecast and your bookmarked places (Open-Meteo, no API key). Automatic finds you by IP. Enter opens it: Enter switches place, a adds (City, CC), d deletes, u metric/imperial, f daily/hourly, r refreshes. Data is cached 10 minutes and shown offline.",
            BoxKind::Todo => "A checkable to-do list. Enter opens it: each item has a [ ] / [x] box. Space or x (or a click on the box) toggles done, a adds an item (text, priority, optional due date), d deletes, f cycles the all/open/done filter. Saved automatically.",
        };
        lines.push(plain_line(Line::from(Span::styled(
            hint,
            Style::default().fg(app.theme.text).add_modifier(Modifier::ITALIC),
        ))));

        if !self.color_presets.is_empty() {
            lines.push(plain_line(Line::from("")));
            layout.presets_heading_line = Some(lines.len());
            lines.push(plain_line(Line::from(vec![Span::styled(
                "Color Theme (Tab to focus, ←/→ select)",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )])));
            layout.presets_start_line = Some(lines.len());
            for (idx, preset) in self.color_presets.iter().enumerate() {
                let is_selected = self.palette_index == idx;
                let highlight_palette = is_selected && self.selected_field == CategoryField::Palette;
                let mut label_style = Style::default().fg(app.theme.text);
                if is_selected {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                let mut spans = vec![Span::styled(
                    format!("{:>2}. {}", idx + 1, preset.name),
                    label_style,
                )];
                if preset.background.is_empty() && preset.text.is_empty() {
                    // The "Default (current theme)" choice: preview the
                    // app theme's own box colors.
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        " Aa ",
                        Style::default().bg(app.theme.surface).fg(app.theme.text),
                    ));
                    spans.push(Span::styled(
                        "  uses the app theme's colors",
                        Style::default().fg(app.theme.text),
                    ));
                    lines.push(if highlight_palette {
                        FormLine::highlighted(Line::from(spans))
                    } else {
                        FormLine::plain(Line::from(spans))
                    });
                    continue;
                }
                let preview_bg = color_from_hex(&preset.background);
                let preview_text = color_from_hex(&preset.text).unwrap_or(app.theme.text);
                if let Some(bg) = preview_bg {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        "     ",
                        Style::default().bg(bg).fg(preview_text),
                    ));
                }
                spans.push(Span::raw("  "));
                let mut background_hex_style = Style::default().fg(app.theme.text);
                let mut divider_style = Style::default().fg(app.theme.text);
                let mut text_hex_style = Style::default().fg(app.theme.text);
                if is_selected {
                    background_hex_style = background_hex_style.add_modifier(Modifier::BOLD);
                    divider_style = divider_style.add_modifier(Modifier::BOLD);
                    text_hex_style = text_hex_style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(preset.background.clone(), background_hex_style));
                spans.push(Span::styled(" / ", divider_style));
                spans.push(Span::styled(preset.text.clone(), text_hex_style));
                if let Some(border) = (!preset.border.is_empty())
                    .then(|| color_from_hex(&preset.border))
                    .flatten()
                {
                    spans.push(Span::styled(" / border ", divider_style));
                    spans.push(Span::styled("══", Style::default().fg(border)));
                    spans.push(Span::styled(format!(" {}", preset.border), text_hex_style));
                }
                let line = if highlight_palette {
                    FormLine::highlighted(Line::from(spans))
                } else {
                    FormLine::plain(Line::from(spans))
                };
                lines.push(line);
            }
            layout.presets_count = self.color_presets.len();
        }

        lines.push(plain_line(Line::from("")));
        lines.push(plain_line(Line::from(vec![Span::styled(
            "Custom Theme (#RRGGBB)",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )])));
        layout
            .field_lines
            .push((lines.len(), CategoryField::CustomPresetName));
        lines.push(make_field_line(
            "Name",
            &self.custom_preset_name,
            self.selected_field == CategoryField::CustomPresetName,
            app,
        ));
        layout
            .field_lines
            .push((lines.len(), CategoryField::CustomPresetBackground));
        lines.push(make_color_field_line(
            "Background",
            &self.custom_preset_background,
            self.selected_field == CategoryField::CustomPresetBackground,
            color_from_hex(&self.custom_preset_background),
            app,
        ));
        layout
            .field_lines
            .push((lines.len(), CategoryField::CustomPresetText));
        lines.push(make_color_field_line(
            "Text",
            &self.custom_preset_text,
            self.selected_field == CategoryField::CustomPresetText,
            color_from_hex(&self.custom_preset_text),
            app,
        ));
        layout
            .field_lines
            .push((lines.len(), CategoryField::CustomPresetBorder));
        lines.push(make_color_field_line(
            "Border (line around the box, blank = theme)",
            &self.custom_preset_border,
            self.selected_field == CategoryField::CustomPresetBorder,
            color_from_hex(&self.custom_preset_border),
            app,
        ));

        lines.push(plain_line(Line::from("")));
        let (shortcut_line, shortcut_segments, shortcut_width) =
            build_category_shortcut_line(app, self.has_deletable_preset());
        layout.shortcut_line = Some(shortcut_line);
        layout.shortcut_segments = shortcut_segments;
        layout.shortcut_total_width = shortcut_width;
        if let Some(error) = &self.error {
            lines.push(plain_line(Line::from(vec![Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )])));
        }
        layout.line_count = lines.len();
        (lines, layout)
    }
    fn handle_key(&mut self, key: KeyEvent) -> FormKeyResult {
        self.error = None;
        if let Some(picker) = self.icon_picker.as_mut() {
            let result = picker.get_mut().handle_key(key);
            self.finish_icon_pick(result);
            return FormKeyResult::Continue;
        }
        if let Some(picker) = self.glyph_picker.as_mut() {
            match picker.handle_key(key) {
                GlyphPickResult::Continue => {}
                GlyphPickResult::Cancel => self.glyph_picker = None,
                GlyphPickResult::Picked(value) => {
                    self.icon = value;
                    self.glyph_picker = None;
                }
            }
            return FormKeyResult::Continue;
        }
        if let Some(picker) = self.folder_picker.as_mut() {
            let result = picker.get_mut().handle_key(key);
            self.finish_folder_pick(result);
            return FormKeyResult::Continue;
        }
        match key.code {
            KeyCode::Esc => FormKeyResult::Cancel,
            KeyCode::Char('e') | KeyCode::Char('E') if self.selected_field == CategoryField::Icon => {
                self.icon_picker = Some(RefCell::new(FilePicker::icon(self.paintings_dir.clone())));
                FormKeyResult::Continue
            }
            KeyCode::Char('g') | KeyCode::Char('G') if self.selected_field == CategoryField::Icon => {
                self.glyph_picker = Some(GlyphPicker::new(&self.icon));
                FormKeyResult::Continue
            }
            KeyCode::F(2) if self.selected_field == CategoryField::Folder => {
                // Contacts / Calendar store a *file*; Paint / Music /
                // Folder / Apps store a *folder*. Pick accordingly.
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                match self.kind.data_file_exts() {
                    Some(exts) => {
                        // A data file: start in its parent folder if the
                        // current value is a real file, otherwise in home.
                        let start = if !self.folder.trim().is_empty() {
                            let p = boxes::expand_tilde(&self.folder);
                            if p.is_file() {
                                p.parent().map(|d| d.to_path_buf()).unwrap_or(home)
                            } else {
                                home
                            }
                        } else {
                            home
                        };
                        self.folder_picker = Some(RefCell::new(FilePicker::data_file(start, exts)));
                    }
                    None => {
                        let start = if self.folder.trim().is_empty() {
                            home
                        } else {
                            let expanded = boxes::expand_tilde(&self.folder);
                            if expanded.is_dir() {
                                expanded
                            } else {
                                home
                            }
                        };
                        self.folder_picker = Some(RefCell::new(FilePicker::folder(start)));
                    }
                }
                FormKeyResult::Continue
            }
            KeyCode::Backspace | KeyCode::Delete if self.selected_field == CategoryField::Icon => {
                self.icon.clear();
                FormKeyResult::Continue
            }
            // Enter always saves; on the Color Theme list that means "use
            // this theme" (custom themes are deleted only with d / Del).
            KeyCode::Enter => {
                if self.selected_field == CategoryField::Palette {
                    self.apply_selected_palette();
                }
                match self.build_submission() {
                    Ok(input) => FormKeyResult::Submit(input),
                    Err(err) => {
                        self.error = Some(err);
                        FormKeyResult::Continue
                    }
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                self.next_field();
                FormKeyResult::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.previous_field();
                FormKeyResult::Continue
            }
            KeyCode::Left if self.selected_field == CategoryField::Palette => {
                self.previous_palette();
                FormKeyResult::Continue
            }
            KeyCode::Left if self.selected_field == CategoryField::Kind => {
                self.kind = self.kind.previous();
                FormKeyResult::Continue
            }
            KeyCode::Right | KeyCode::Char(' ') if self.selected_field == CategoryField::Kind => {
                self.kind = self.kind.next();
                FormKeyResult::Continue
            }
            KeyCode::Left if self.selected_field == CategoryField::Style => {
                self.style = Some(self.form_style().previous(self.kind));
                FormKeyResult::Continue
            }
            KeyCode::Right | KeyCode::Char(' ') if self.selected_field == CategoryField::Style => {
                self.style = Some(self.form_style().next(self.kind));
                FormKeyResult::Continue
            }
            KeyCode::Char(' ') if self.selected_field == CategoryField::NameOnly => {
                self.name_only = !self.name_only;
                FormKeyResult::Continue
            }
            KeyCode::Char(' ') if self.selected_field == CategoryField::ShowPicture => {
                self.show_picture = !self.show_picture;
                FormKeyResult::Continue
            }
            KeyCode::Char(' ') if self.selected_field == CategoryField::Pause => {
                self.pause = !self.pause;
                FormKeyResult::Continue
            }
            KeyCode::Char(' ') if self.selected_field == CategoryField::ClearScreen => {
                self.clear_screen = !self.clear_screen;
                FormKeyResult::Continue
            }
            KeyCode::Right if self.selected_field == CategoryField::Palette => {
                self.next_palette();
                FormKeyResult::Continue
            }
            KeyCode::Backspace => {
                if let Some(value) = self.active_value_mut() {
                    value.pop();
                }
                FormKeyResult::Continue
            }
            KeyCode::Delete => {
                if let Some(value) = self.active_value_mut() {
                    value.clear();
                    FormKeyResult::Continue
                } else if self.selected_field == CategoryField::Palette
                    && self.has_deletable_preset()
                {
                    if let Some(index) = self.current_custom_preset_index() {
                        FormKeyResult::DeletePreset(index)
                    } else {
                        FormKeyResult::Continue
                    }
                } else {
                    FormKeyResult::Continue
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D')
                if self.has_deletable_preset()
                    && self.selected_field == CategoryField::Palette =>
            {
                if let Some(index) = self.current_custom_preset_index() {
                    FormKeyResult::DeletePreset(index)
                } else {
                    FormKeyResult::Continue
                }
            }
            KeyCode::Char(c) => {
                if self.selected_field != CategoryField::Palette
                    && !key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let Some(value) = self.active_value_mut() {
                        value.push(c);
                    }
                }
                FormKeyResult::Continue
            }
            _ => FormKeyResult::Continue,
        }
    }

    fn build_submission(&self) -> Result<CategorySubmitPayload, String> {
        let background_result = parse_color_field(&self.custom_preset_background)?;
        let text_result = parse_color_field(&self.custom_preset_text)?;

        let border_result = parse_color_field(&self.custom_preset_border)?;

        let background_value = background_result.clone().unwrap_or_default();
        let text_value = text_result.clone().unwrap_or_default();
        let border_value = border_result.clone().unwrap_or_default();

        let mut new_preset: Option<CustomPresetInput> = None;
        if let (Some(bg), Some(txt)) = (background_result, text_result) {
            let exists = self
                .color_presets
                .iter()
                .any(|preset| preset.matches(&bg, &txt, &border_value));
            if !exists {
                let name = if self.custom_preset_name.trim().is_empty() {
                    "Custom Theme".to_string()
                } else {
                    self.custom_preset_name.trim().to_string()
                };
                new_preset = Some(CustomPresetInput {
                    name,
                    background: bg.clone(),
                    text: txt.clone(),
                    border: border_result,
                });
            }
        }

        Ok(CategorySubmitPayload {
            form: CategoryFormInput {
                category_index: self.category_index,
                name: self.name.clone(),
                kind: self.kind,
                column_value: self.column_value.clone(),
                span_value: self.span_value.clone(),
                folder: self.folder.clone(),
                app: self.app.clone(),
                filter: self.filter.clone(),
                icon: self.icon.clone(),
                name_only: self.name_only,
                show_picture: self.show_picture,
                max_rows: self.max_rows.clone(),
                articles: self.articles.clone(),
                pause: self.pause,
                clear_screen: self.clear_screen,
                style: self.form_style(),
                background: background_value,
                text_color: text_value,
                border_color: border_value,
            },
            new_preset,
        })
    }

    /// Apply the icon picker's result: a drawing in the paintings folder is
    /// stored by name, anything else by its full path.
    fn finish_icon_pick(&mut self, result: PickResult) {
        match result {
            PickResult::Continue => {}
            PickResult::Cancel => self.icon_picker = None,
            PickResult::Picked(path) => {
                self.icon = if path.parent() == Some(self.paintings_dir.as_path()) {
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default()
                } else {
                    path.display().to_string()
                };
                self.icon_picker = None;
            }
        }
    }

    /// Apply the folder/data-file picker's result to the target field.
    /// A folder pick stores the folder; a Contacts/Calendar pick stores the
    /// chosen data file (both resolve through `data_path` the same way).
    fn finish_folder_pick(&mut self, result: PickResult) {
        match result {
            PickResult::Continue => {}
            PickResult::Cancel => self.folder_picker = None,
            PickResult::Picked(path) => {
                self.folder = path.display().to_string();
                self.folder_picker = None;
            }
        }
    }

    /// Fields shown for the current box type, in Tab order.
    /// The chosen style, or the box type's default when none was picked (or
    /// the type changed to one that doesn't offer the picked style).
    fn form_style(&self) -> BoxStyle {
        self.style
            .filter(|style| style.fits(self.kind))
            .unwrap_or_else(|| BoxStyle::default_for(self.kind))
    }

    fn fields(&self) -> Vec<CategoryField> {
        let mut fields = vec![
            CategoryField::Name,
            CategoryField::Kind,
            CategoryField::Column,
            CategoryField::Span,
        ];
        if self.kind.uses_folder() || self.kind.uses_data_file() {
            fields.push(CategoryField::Folder);
        }
        if matches!(self.kind, BoxKind::Folder | BoxKind::Shortcut) {
            fields.push(CategoryField::App);
        }
        if self.kind == BoxKind::Shortcut {
            fields.push(CategoryField::NameOnly);
            if !self.name_only {
                fields.push(CategoryField::Icon);
            }
        }
        if self.kind == BoxKind::Paint {
            fields.push(CategoryField::ShowPicture);
        }
        if self.kind == BoxKind::Folder {
            fields.push(CategoryField::Filter);
        }
        if BoxStyle::choices(self.kind).len() > 1 {
            fields.push(CategoryField::Style);
        }
        if self.kind == BoxKind::Rss {
            fields.push(CategoryField::Articles);
        }
        if self.kind != BoxKind::Shortcut {
            fields.push(CategoryField::MaxRows);
        }
        if self.kind.launches() {
            fields.push(CategoryField::ClearScreen);
            fields.push(CategoryField::Pause);
        }
        if !self.color_presets.is_empty() {
            fields.push(CategoryField::Palette);
        }
        fields.extend([
            CategoryField::CustomPresetName,
            CategoryField::CustomPresetBackground,
            CategoryField::CustomPresetText,
            CategoryField::CustomPresetBorder,
        ]);
        fields
    }

    fn next_field(&mut self) {
        let fields = self.fields();
        let pos = fields.iter().position(|f| *f == self.selected_field).unwrap_or(0);
        self.selected_field = fields[(pos + 1) % fields.len()];
    }

    fn previous_field(&mut self) {
        let fields = self.fields();
        let pos = fields.iter().position(|f| *f == self.selected_field).unwrap_or(0);
        self.selected_field = fields[(pos + fields.len() - 1) % fields.len()];
    }

    fn active_value_mut(&mut self) -> Option<&mut String> {
        match self.selected_field {
            CategoryField::Name => Some(&mut self.name),
            CategoryField::Column => Some(&mut self.column_value),
            CategoryField::Span => Some(&mut self.span_value),
            CategoryField::Folder => Some(&mut self.folder),
            CategoryField::App => Some(&mut self.app),
            CategoryField::Filter => Some(&mut self.filter),
            CategoryField::Icon => None,
            CategoryField::MaxRows => Some(&mut self.max_rows),
            CategoryField::Articles => Some(&mut self.articles),
            CategoryField::CustomPresetName => Some(&mut self.custom_preset_name),
            CategoryField::CustomPresetBackground => Some(&mut self.custom_preset_background),
            CategoryField::CustomPresetText => Some(&mut self.custom_preset_text),
            CategoryField::CustomPresetBorder => Some(&mut self.custom_preset_border),
            CategoryField::Kind
            | CategoryField::Style
            | CategoryField::NameOnly
            | CategoryField::ShowPicture
            | CategoryField::ClearScreen
            | CategoryField::Pause
            | CategoryField::Palette => None,
        }
    }

    fn has_deletable_preset(&self) -> bool {
        self.current_custom_preset_index().is_some()
    }

    fn current_custom_preset_index(&self) -> Option<usize> {
        self.color_presets
            .get(self.palette_index)
            .and_then(|preset| preset.custom_index)
    }
    fn refresh_presets(&mut self, presets: Vec<ColorPreset>) {
        self.color_presets = presets;
        if self.color_presets.is_empty() {
            self.palette_index = 0;
        } else {
            if self.palette_index >= self.color_presets.len() {
                self.palette_index = self.color_presets.len() - 1;
            }
            self.apply_selected_palette();
        }
    }

    fn focus_palette_index(&mut self, index: usize) {
        if self.color_presets.is_empty() {
            return;
        }
        self.palette_index = index.min(self.color_presets.len() - 1);
        self.selected_field = CategoryField::Palette;
        self.apply_selected_palette();
    }

    fn next_palette(&mut self) {
        if self.color_presets.is_empty() {
            return;
        }
        self.palette_index = (self.palette_index + 1) % self.color_presets.len();
        self.apply_selected_palette();
    }

    fn previous_palette(&mut self) {
        if self.color_presets.is_empty() {
            return;
        }
        if self.palette_index == 0 {
            self.palette_index = self.color_presets.len() - 1;
        } else {
            self.palette_index -= 1;
        }
        self.apply_selected_palette();
    }

    fn apply_selected_palette(&mut self) {
        if let Some(preset) = self.color_presets.get(self.palette_index) {
            self.custom_preset_background = preset.background.clone();
            self.custom_preset_text = preset.text.clone();
            self.custom_preset_border = preset.border.clone();
            self.custom_preset_name = preset.name.clone();
        }
    }
}

impl ItemFormState {
    fn new(
        target: Option<(usize, usize)>,
        label: String,
        command: String,
        info: String,
        category: String,
        fallback_category: String,
        pause: bool,
        clear_screen: bool,
        available_categories: Vec<String>,
    ) -> Self {
        let mut available_category_index = 0;
        if !available_categories.is_empty() {
            if let Some(index) = available_categories
                .iter()
                .position(|name| name == &category)
            {
                available_category_index = index;
            }
        }
        Self {
            target,
            label,
            command,
            info,
            category,
            fallback_category,
            pause,
            clear_screen,
            available_categories,
            available_category_index,
            selected_field: ItemField::Label,
            error: None,
            mode_label: if target.is_some() {
                "Edit Menu Item"
            } else {
                "New Menu Item"
            },
        }
    }

    fn render_lines(&self, app: &AppState) -> (Vec<FormLine>, ItemFormLayout) {
        let mut layout = ItemFormLayout::default();
        let mut lines: Vec<FormLine> = Vec::new();
        lines.push(plain_line(Line::from("Fill in the menu item details below.")));

        layout.label_line = Some(lines.len());
        lines.push(make_field_line(
            "Label",
            &self.label,
            self.selected_field == ItemField::Label,
            app,
        ));
        layout.command_line = Some(lines.len());
        lines.push(make_field_line(
            "Command",
            &self.command,
            self.selected_field == ItemField::Command,
            app,
        ));
        layout.description_line = Some(lines.len());
        lines.push(make_field_line(
            "Description",
            &self.info,
            self.selected_field == ItemField::Description,
            app,
        ));
        layout.clear_screen_line = Some(lines.len());
        lines.push(make_toggle_line(
            "Clear Screen Before Run",
            self.clear_screen,
            self.selected_field == ItemField::ClearScreen,
            app,
        ));
        layout.pause_line = Some(lines.len());
        lines.push(make_toggle_line(
            "Pause After Run",
            self.pause,
            self.selected_field == ItemField::Pause,
            app,
        ));
        layout.category_line = Some(lines.len());
        lines.push(make_field_line(
            "Category",
            &self.category,
            self.selected_field == ItemField::Category,
            app,
        ));
        if let Some(error) = &self.error {
            lines.push(plain_line(Line::from(vec![Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )])));
        }
        if !self.available_categories.is_empty() {
            lines.push(plain_line(Line::from("")));
            layout.available_heading_line = Some(lines.len());
            let heading_line = Line::from(vec![Span::styled(
                "Available Categories (Tab to focus, ↑/↓ move, Enter pick)",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )]);
            let heading = if self.selected_field == ItemField::AvailableCategories {
                FormLine::highlighted(heading_line)
            } else {
                FormLine::plain(heading_line)
            };
            lines.push(heading);
            layout.available_start_line = Some(lines.len());
            for (idx, category) in self.available_categories.iter().enumerate() {
                let entry = format!("  • {}", category);
                let line = if self.selected_field == ItemField::AvailableCategories
                    && idx == self.available_category_index
                {
                    FormLine::highlighted(Line::from(entry))
                } else {
                    FormLine::plain(Line::from(entry))
                };
                lines.push(line);
            }
            layout.available_count = self.available_categories.len();
        }

        let shortcut_line = Line::from(vec![
            Span::styled(
                "Tab",
                Style::default()
                    .fg(app.theme.bar_key())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("/", Style::default().fg(app.theme.bar_label())),
            Span::styled(
                "Shift+Tab",
                Style::default()
                    .fg(app.theme.bar_key())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Move    ", Style::default().fg(app.theme.bar_label())),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(app.theme.bar_key())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Save    ", Style::default().fg(app.theme.bar_label())),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(app.theme.bar_key())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Cancel    ", Style::default().fg(app.theme.bar_label())),
            Span::styled(
                "Space",
                Style::default()
                    .fg(app.theme.bar_key())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Toggle Option", Style::default().fg(app.theme.bar_label())),
        ]);
        layout.shortcut_line = Some(shortcut_line);
        layout.line_count = lines.len();
        (lines, layout)
    }

    fn handle_key(&mut self, key: KeyEvent) -> ItemFormKeyResult {
        self.error = None;
        match key.code {
            KeyCode::Esc => ItemFormKeyResult::Cancel,
            KeyCode::Enter => {
                if self.selected_field == ItemField::AvailableCategories {
                    self.pick_available_category(self.available_category_index);
                    self.selected_field = ItemField::Category;
                    ItemFormKeyResult::Continue
                } else {
                    ItemFormKeyResult::Submit(self.to_input())
                }
            }
            KeyCode::Tab => {
                self.next_field();
                ItemFormKeyResult::Continue
            }
            KeyCode::Down => {
                if self.selected_field == ItemField::AvailableCategories {
                    self.next_available_category();
                } else if self.selected_field == ItemField::Category {
                    self.selected_field = ItemField::Label;
                } else {
                    self.next_field();
                }
                ItemFormKeyResult::Continue
            }
            KeyCode::BackTab => {
                self.previous_field();
                ItemFormKeyResult::Continue
            }
            KeyCode::Up => {
                if self.selected_field == ItemField::AvailableCategories {
                    self.previous_available_category();
                } else {
                    self.previous_field();
                }
                ItemFormKeyResult::Continue
            }
            KeyCode::Char(' ')
                if self.selected_field == ItemField::Pause
                    || self.selected_field == ItemField::ClearScreen =>
            {
                if self.selected_field == ItemField::Pause {
                    self.pause = !self.pause;
                } else {
                    self.clear_screen = !self.clear_screen;
                }
                ItemFormKeyResult::Continue
            }
            KeyCode::Backspace => {
                if let Some(value) = self.active_value_mut() {
                    value.pop();
                }
                ItemFormKeyResult::Continue
            }
            KeyCode::Delete => {
                if let Some(value) = self.active_value_mut() {
                    value.clear();
                }
                ItemFormKeyResult::Continue
            }
            KeyCode::Char(c) => {
                if self.selected_field != ItemField::Pause
                    && self.selected_field != ItemField::AvailableCategories
                    && self.selected_field != ItemField::ClearScreen
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    if let Some(value) = self.active_value_mut() {
                        value.push(c);
                    }
                }
                ItemFormKeyResult::Continue
            }
            _ => ItemFormKeyResult::Continue,
        }
    }

    fn to_input(&self) -> ItemFormInput {
        ItemFormInput {
            target: self.target,
            label: self.label.clone(),
            command: self.command.clone(),
            info: self.info.clone(),
            category: self.category.clone(),
            fallback_category: self.fallback_category.clone(),
            pause: self.pause,
            clear_screen: self.clear_screen,
        }
    }

    fn next_field(&mut self) {
        self.selected_field = match self.selected_field {
            ItemField::Label => ItemField::Command,
            ItemField::Command => ItemField::Description,
            ItemField::Description => ItemField::ClearScreen,
            ItemField::ClearScreen => ItemField::Pause,
            ItemField::Pause => ItemField::Category,
            ItemField::Category => ItemField::AvailableCategories,
            ItemField::AvailableCategories => ItemField::Label,
        };
    }

    fn previous_field(&mut self) {
        self.selected_field = match self.selected_field {
            ItemField::Label => ItemField::AvailableCategories,
            ItemField::Command => ItemField::Label,
            ItemField::Description => ItemField::Command,
            ItemField::ClearScreen => ItemField::Description,
            ItemField::Pause => ItemField::ClearScreen,
            ItemField::Category => ItemField::Pause,
            ItemField::AvailableCategories => ItemField::Category,
        };
    }

    fn active_value_mut(&mut self) -> Option<&mut String> {
        match self.selected_field {
            ItemField::Label => Some(&mut self.label),
            ItemField::Command => Some(&mut self.command),
            ItemField::Description => Some(&mut self.info),
            ItemField::Category => Some(&mut self.category),
            ItemField::AvailableCategories => None,
            ItemField::ClearScreen => None,
            ItemField::Pause => None,
        }
    }

    fn next_available_category(&mut self) {
        if self.available_categories.is_empty() {
            return;
        }
        self.available_category_index =
            (self.available_category_index + 1) % self.available_categories.len();
    }

    fn previous_available_category(&mut self) {
        if self.available_categories.is_empty() {
            return;
        }
        if self.available_category_index == 0 {
            self.available_category_index = self.available_categories.len() - 1;
        } else {
            self.available_category_index -= 1;
        }
    }

    fn pick_available_category(&mut self, index: usize) {
        if let Some(category) = self.available_categories.get(index) {
            self.available_category_index = index;
            self.category = category.clone();
        }
    }
}

struct SettingsFormState {
    title: String,
    columns_value: String,
    drag_boxes: bool,
    /// Pages switch, and one name per page plus a blank last one that adds
    /// a page when typed into. Clearing a name removes that page.
    pages_enabled: bool,
    page_names: Vec<String>,
    /// How many pages existed when the form opened.
    existing_pages: usize,
    theme_options: Vec<ThemeOption>,
    theme_index: usize,
    selected_field: SettingsField,
    error: Option<String>,
    custom_primary: String,
    custom_accent: String,
    custom_background: String,
    custom_surface: String,
    custom_text: String,
    custom_highlight: String,
    custom_theme_name: String,
}

#[derive(Default)]
struct SettingsFormLayout {
    line_count: usize,
    title_line: Option<usize>,
    columns_line: Option<usize>,
    drag_boxes_line: Option<usize>,
    pages_line: Option<usize>,
    /// (form line, page index) for each page-name field.
    page_name_lines: Vec<(usize, usize)>,
    theme_heading_line: Option<usize>,
    theme_list_start: Option<usize>,
    theme_count: usize,
    custom_heading_line: Option<usize>,
    delete_saved_theme_line: Option<usize>,
    delete_saved_theme_index: Option<usize>,
    shortcut_line: Option<Line<'static>>,
    shortcut_segments: Vec<SettingsShortcutSegment>,
    shortcut_total_width: u16,
    custom_name_line: Option<usize>,
    custom_primary_line: Option<usize>,
    custom_accent_line: Option<usize>,
    custom_background_line: Option<usize>,
    custom_surface_line: Option<usize>,
    custom_text_line: Option<usize>,
    custom_highlight_line: Option<usize>,
}

#[derive(Clone)]
struct SettingsFormInput {
    title: String,
    columns: String,
    drag_boxes: bool,
    pages_enabled: bool,
    page_names: Vec<String>,
    theme_key: String,
    custom_primary: String,
    custom_accent: String,
    custom_background: String,
    custom_surface: String,
    custom_text: String,
    custom_highlight: String,
    custom_theme_name: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsField {
    Title,
    Columns,
    DragBoxes,
    /// The Pages on/off switch.
    Pages,
    /// The name of page N (N == page count is the blank "new page" slot).
    PageName(usize),
    Theme,
    CustomName,
    CustomPrimary,
    CustomAccent,
    CustomBackground,
    CustomSurface,
    CustomText,
    CustomHighlight,
}

enum SettingsFormKeyResult {
    Continue,
    Cancel,
    Submit(SettingsFormInput),
    DeleteSavedTheme(usize),
}

impl SettingsFormState {
    fn new(
        title: String,
        columns: u16,
        drag_boxes: bool,
        theme_key: String,
        options: Vec<ThemeOption>,
        initial_field: SettingsField,
        current_theme: &Theme,
        is_custom_theme: bool,
    ) -> Self {
        let columns_value = columns.to_string();
        let theme_index = options
            .iter()
            .position(|opt| opt.key == theme_key)
            .unwrap_or(0);
        Self {
            title,
            columns_value,
            drag_boxes,
            pages_enabled: false,
            page_names: Vec::new(),
            existing_pages: 0,
            theme_options: options,
            theme_index,
            selected_field: initial_field,
            error: None,
            custom_primary: String::new(),
            custom_accent: String::new(),
            custom_background: String::new(),
            custom_surface: String::new(),
            custom_text: String::new(),
            custom_highlight: String::new(),
            custom_theme_name: if is_custom_theme {
                current_theme.name.clone()
            } else {
                String::new()
            },
        }
        .with_selected_theme_colors()
    }

    /// Fill in the Pages switch and the page names.
    fn with_pages(mut self, enabled: bool, names: Vec<String>) -> Self {
        self.pages_enabled = enabled;
        self.existing_pages = names.len();
        self.page_names = names;
        self.page_names.push(String::new());
        if let SettingsField::PageName(index) = self.selected_field {
            if !enabled || index >= self.page_names.len() {
                self.selected_field = SettingsField::Pages;
            }
        }
        self
    }

    /// Keep exactly one blank slot at the end for adding a page.
    fn trim_page_slots(&mut self) {
        let keep = self.existing_pages;
        while self.page_names.len() > keep + 1
            && self.page_names.len() >= 2
            && self.page_names[self.page_names.len() - 1].trim().is_empty()
            && self.page_names[self.page_names.len() - 2].trim().is_empty()
        {
            self.page_names.pop();
        }
        if self
            .page_names
            .last()
            .map_or(true, |last| !last.trim().is_empty())
        {
            self.page_names.push(String::new());
        }
        if let SettingsField::PageName(index) = self.selected_field {
            if index >= self.page_names.len() {
                self.selected_field = SettingsField::PageName(self.page_names.len() - 1);
            }
        }
    }

    fn toggle_pages(&mut self) {
        self.pages_enabled = !self.pages_enabled;
    }

    fn current_deletable_theme_index(&self) -> Option<usize> {
        self.theme_options
            .get(self.theme_index)
            .and_then(|opt| parse_saved_theme_key(&opt.key))
    }

    fn render_lines(&self, app: &AppState) -> (Vec<FormLine>, SettingsFormLayout) {
        let mut layout = SettingsFormLayout::default();
        let mut lines: Vec<FormLine> = Vec::new();
        let deletable_index = self.current_deletable_theme_index();
        if let Some(saved_idx) = deletable_index {
            layout.delete_saved_theme_index = Some(saved_idx);
        }
        lines.push(plain_line(Line::from("Adjust application settings below.")));

        layout.title_line = Some(lines.len());
        lines.push(make_field_line(
            "Title",
            &self.title,
            self.selected_field == SettingsField::Title,
            app,
        ));

        layout.columns_line = Some(lines.len());
        lines.push(make_field_line(
            "Columns (1-6)",
            &self.columns_value,
            self.selected_field == SettingsField::Columns,
            app,
        ));
        layout.drag_boxes_line = Some(lines.len());
        lines.push(make_toggle_line(
            "Drag boxes with the mouse (by the title; Space toggles)",
            self.drag_boxes,
            self.selected_field == SettingsField::DragBoxes,
            app,
        ));
        layout.pages_line = Some(lines.len());
        lines.push(make_toggle_line(
            "Pages: several named desktops (Space toggles)",
            self.pages_enabled,
            self.selected_field == SettingsField::Pages,
            app,
        ));
        if self.pages_enabled {
            for (index, name) in self.page_names.iter().enumerate() {
                let label = if index < self.existing_pages {
                    format!("  Page {} name", index + 1)
                } else if index == self.page_names.len() - 1 {
                    "  + New page (type a name)".to_string()
                } else {
                    format!("  New page {}", index + 1)
                };
                layout.page_name_lines.push((lines.len(), index));
                lines.push(make_field_line(
                    &label,
                    name,
                    self.selected_field == SettingsField::PageName(index),
                    app,
                ));
            }
            lines.push(plain_line(Line::from(vec![Span::styled(
                "  Clear a name (Del) to remove that page; its boxes move to page 1. [ ] switch pages, p renames.",
                Style::default().fg(app.theme.text),
            )])));
        }

        lines.push(plain_line(Line::from("")));
        let (shortcut_line, shortcut_segments, shortcut_width) =
            build_settings_shortcut_line(app, deletable_index.is_some());
        layout.shortcut_line = Some(shortcut_line);
        layout.shortcut_segments = shortcut_segments;
        layout.shortcut_total_width = shortcut_width;
        if let Some(error) = &self.error {
            lines.push(plain_line(Line::from(vec![Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )])));
        }
        if !self.theme_options.is_empty() {
            lines.push(plain_line(Line::from("")));
            layout.theme_heading_line = Some(lines.len());
            lines.push(plain_line(Line::from(vec![Span::styled(
                "Theme Presets (Tab to focus, ←/→ select)",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )])));
            layout.theme_list_start = Some(lines.len());
            for (idx, option) in self.theme_options.iter().enumerate() {
                let is_active = self.theme_index == idx;
                let mut label_style = Style::default().fg(app.theme.text);
                if is_active {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                let mut spans = vec![Span::styled(
                    format!("{:>2}. {}", idx + 1, option.label),
                    label_style,
                )];
                if let Some(surface) = color_from_hex(&option.surface_hex) {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        "     ",
                        Style::default().bg(surface).fg(app.theme.text),
                    ));
                }
                if let Some(accent) = color_from_hex(&option.accent_hex) {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        "     ",
                        Style::default().bg(accent).fg(app.theme.background),
                    ));
                }
                if let Some(highlight) = color_from_hex(&option.highlight_hex) {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        "     ",
                        Style::default().bg(highlight).fg(app.theme.background),
                    ));
                }
                for (label, hex) in [
                    ("Primary", &option.primary_hex),
                    ("Accent", &option.accent_hex),
                    ("Highlight", &option.highlight_hex),
                    ("Background", &option.background_hex),
                    ("Surface", &option.surface_hex),
                    ("Text", &option.text_hex),
                ] {
                    spans.push(Span::raw("  "));
                    let mut color_style = Style::default().fg(app.theme.text);
                    if is_active {
                        color_style = color_style.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(
                        format!("{} {}", label, hex.to_uppercase()),
                        color_style,
                    ));
                }
                let line = Line::from(spans);
                if self.selected_field == SettingsField::Theme && is_active {
                    lines.push(FormLine::highlighted(line));
                } else {
                    lines.push(FormLine::plain(line));
                }
            }
            layout.theme_count = self.theme_options.len();
        }
        lines.push(plain_line(Line::from("")));
        layout.custom_heading_line = Some(lines.len());
        lines.push(plain_line(Line::from(vec![Span::styled(
            "Custom Theme Colors (#RRGGBB, leave blank to keep preset)",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )])));
        layout.custom_name_line = Some(lines.len());
        lines.push(make_field_line(
            "Custom Theme Name",
            &self.custom_theme_name,
            self.selected_field == SettingsField::CustomName,
            app,
        ));
        layout.custom_primary_line = Some(lines.len());
        lines.push(make_color_field_line(
            "Primary",
            &self.custom_primary,
            self.selected_field == SettingsField::CustomPrimary,
            color_from_hex(&self.custom_primary),
            app,
        ));
        layout.custom_accent_line = Some(lines.len());
        lines.push(make_color_field_line(
            "Accent",
            &self.custom_accent,
            self.selected_field == SettingsField::CustomAccent,
            color_from_hex(&self.custom_accent),
            app,
        ));
        layout.custom_highlight_line = Some(lines.len());
        lines.push(make_color_field_line(
            "Highlight",
            &self.custom_highlight,
            self.selected_field == SettingsField::CustomHighlight,
            color_from_hex(&self.custom_highlight),
            app,
        ));
        layout.custom_background_line = Some(lines.len());
        lines.push(make_field_line(
            "Background",
            &self.custom_background,
            self.selected_field == SettingsField::CustomBackground,
            app,
        ));
        layout.custom_surface_line = Some(lines.len());
        lines.push(make_color_field_line(
            "Surface",
            &self.custom_surface,
            self.selected_field == SettingsField::CustomSurface,
            color_from_hex(&self.custom_surface),
            app,
        ));
        layout.custom_text_line = Some(lines.len());
        lines.push(make_color_field_line(
            "Text",
            &self.custom_text,
            self.selected_field == SettingsField::CustomText,
            color_from_hex(&self.custom_text),
            app,
        ));
        layout.line_count = lines.len();
        (lines, layout)
    }

    fn handle_key(&mut self, key: KeyEvent) -> SettingsFormKeyResult {
        self.error = None;
        match key.code {
            KeyCode::Esc => SettingsFormKeyResult::Cancel,
            KeyCode::Enter => SettingsFormKeyResult::Submit(self.to_input()),
            KeyCode::Tab | KeyCode::Down => {
                self.next_field();
                SettingsFormKeyResult::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.previous_field();
                SettingsFormKeyResult::Continue
            }
            KeyCode::Left if self.selected_field == SettingsField::Theme => {
                self.previous_theme();
                SettingsFormKeyResult::Continue
            }
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                if self.selected_field == SettingsField::DragBoxes =>
            {
                self.drag_boxes = !self.drag_boxes;
                SettingsFormKeyResult::Continue
            }
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                if self.selected_field == SettingsField::Pages =>
            {
                self.toggle_pages();
                SettingsFormKeyResult::Continue
            }
            KeyCode::Right if self.selected_field == SettingsField::Theme => {
                self.next_theme();
                SettingsFormKeyResult::Continue
            }
            KeyCode::Char('d') | KeyCode::Char('D')
                if self.selected_field == SettingsField::Theme =>
            {
                if let Some(index) = self.current_deletable_theme_index() {
                    SettingsFormKeyResult::DeleteSavedTheme(index)
                } else {
                    SettingsFormKeyResult::Continue
                }
            }
            KeyCode::Backspace => {
                if let Some(value) = self.active_value_mut() {
                    value.pop();
                }
                self.trim_page_slots();
                SettingsFormKeyResult::Continue
            }
            KeyCode::Delete => {
                if let Some(value) = self.active_value_mut() {
                    value.clear();
                }
                self.trim_page_slots();
                SettingsFormKeyResult::Continue
            }
            KeyCode::Char(c) => {
                if self.selected_field != SettingsField::Theme
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    if let Some(value) = self.active_value_mut() {
                        value.push(c);
                    }
                }
                self.trim_page_slots();
                SettingsFormKeyResult::Continue
            }
            _ => SettingsFormKeyResult::Continue,
        }
    }

    fn to_input(&self) -> SettingsFormInput {
        SettingsFormInput {
            title: self.title.clone(),
            columns: self.columns_value.clone(),
            drag_boxes: self.drag_boxes,
            pages_enabled: self.pages_enabled,
            page_names: self.page_names.clone(),
            theme_key: self
                .theme_options
                .get(self.theme_index)
                .map(|opt| opt.key.clone())
                .unwrap_or_default(),
            custom_primary: self.custom_primary.clone(),
            custom_accent: self.custom_accent.clone(),
            custom_background: self.custom_background.clone(),
            custom_surface: self.custom_surface.clone(),
            custom_text: self.custom_text.clone(),
            custom_highlight: self.custom_highlight.clone(),
            custom_theme_name: self.custom_theme_name.clone(),
        }
    }

    fn next_field(&mut self) {
        let last_page = self.page_names.len().saturating_sub(1);
        self.selected_field = match self.selected_field {
            SettingsField::Title => SettingsField::Columns,
            SettingsField::Columns => SettingsField::DragBoxes,
            SettingsField::DragBoxes => SettingsField::Pages,
            SettingsField::Pages if self.pages_enabled && !self.page_names.is_empty() => {
                SettingsField::PageName(0)
            }
            SettingsField::Pages => SettingsField::Theme,
            SettingsField::PageName(index) if index < last_page => {
                SettingsField::PageName(index + 1)
            }
            SettingsField::PageName(_) => SettingsField::Theme,
            SettingsField::Theme => SettingsField::CustomName,
            SettingsField::CustomName => SettingsField::CustomPrimary,
            SettingsField::CustomPrimary => SettingsField::CustomAccent,
            SettingsField::CustomAccent => SettingsField::CustomHighlight,
            SettingsField::CustomHighlight => SettingsField::CustomBackground,
            SettingsField::CustomBackground => SettingsField::CustomSurface,
            SettingsField::CustomSurface => SettingsField::CustomText,
            SettingsField::CustomText => SettingsField::Title,
        };
    }

    fn previous_field(&mut self) {
        self.selected_field = match self.selected_field {
            SettingsField::Title => SettingsField::CustomText,
            SettingsField::Columns => SettingsField::Title,
            SettingsField::DragBoxes => SettingsField::Columns,
            SettingsField::Pages => SettingsField::DragBoxes,
            SettingsField::PageName(0) => SettingsField::Pages,
            SettingsField::PageName(index) => SettingsField::PageName(index - 1),
            SettingsField::Theme if self.pages_enabled && !self.page_names.is_empty() => {
                SettingsField::PageName(self.page_names.len() - 1)
            }
            SettingsField::Theme => SettingsField::Pages,
            SettingsField::CustomName => SettingsField::Theme,
            SettingsField::CustomPrimary => SettingsField::CustomName,
            SettingsField::CustomAccent => SettingsField::CustomPrimary,
            SettingsField::CustomHighlight => SettingsField::CustomAccent,
            SettingsField::CustomBackground => SettingsField::CustomHighlight,
            SettingsField::CustomSurface => SettingsField::CustomBackground,
            SettingsField::CustomText => SettingsField::CustomSurface,
        };
    }

    fn active_value_mut(&mut self) -> Option<&mut String> {
        match self.selected_field {
            SettingsField::Title => Some(&mut self.title),
            SettingsField::Columns => Some(&mut self.columns_value),
            SettingsField::DragBoxes | SettingsField::Pages | SettingsField::Theme => None,
            SettingsField::PageName(index) => self.page_names.get_mut(index),
            SettingsField::CustomName => Some(&mut self.custom_theme_name),
            SettingsField::CustomPrimary => Some(&mut self.custom_primary),
            SettingsField::CustomAccent => Some(&mut self.custom_accent),
            SettingsField::CustomBackground => Some(&mut self.custom_background),
            SettingsField::CustomSurface => Some(&mut self.custom_surface),
            SettingsField::CustomText => Some(&mut self.custom_text),
            SettingsField::CustomHighlight => Some(&mut self.custom_highlight),
        }
    }

    fn with_selected_theme_colors(mut self) -> Self {
        self.populate_custom_fields_from_selection();
        self
    }

    fn populate_custom_fields_from_selection(&mut self) {
        if self.theme_options.is_empty() {
            return;
        }
        if self.theme_index >= self.theme_options.len() {
            self.theme_index = 0;
        }
        if let Some(option) = self.theme_options.get(self.theme_index) {
            self.custom_primary = option.primary_hex.clone();
            self.custom_accent = option.accent_hex.clone();
            self.custom_highlight = option.highlight_hex.clone();
            self.custom_background = option.background_hex.clone();
            self.custom_surface = option.surface_hex.clone();
            self.custom_text = option.text_hex.clone();
            if parse_saved_theme_key(&option.key).is_some() {
                self.custom_theme_name = option.label.clone();
            } else if option.key != CUSTOM_THEME_KEY {
                self.custom_theme_name.clear();
            }
        }
    }

    fn next_theme(&mut self) {
        if self.theme_options.is_empty() {
            return;
        }
        self.theme_index = (self.theme_index + 1) % self.theme_options.len();
        self.populate_custom_fields_from_selection();
    }

    fn previous_theme(&mut self) {
        if self.theme_options.is_empty() {
            return;
        }
        if self.theme_index == 0 {
            self.theme_index = self.theme_options.len() - 1;
        } else {
            self.theme_index -= 1;
        }
        self.populate_custom_fields_from_selection();
    }
}

fn build_settings_shortcut_line(
    app: &AppState,
    include_delete: bool,
) -> (Line<'static>, Vec<SettingsShortcutSegment>, u16) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut segments: Vec<SettingsShortcutSegment> = Vec::new();
    let mut cursor: u16 = 0;
    let key_style = Style::default()
        .fg(app.theme.bar_key())
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.theme.bar_label());
    let entries: Vec<(&str, &str, SettingsShortcutAction)> = vec![
        ("Tab", " Move", SettingsShortcutAction::NextField),
        ("↵", " Save", SettingsShortcutAction::Submit),
        ("Esc", " Cancel/Exit", SettingsShortcutAction::Cancel),
    ];

    for (idx, (key, label, action)) in entries.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" | ", label_style));
            cursor = cursor.saturating_add(3);
        }
        let entry_start = cursor;
        spans.push(Span::styled(*key, key_style));
        cursor = cursor.saturating_add(key.chars().count() as u16);
        if !label.is_empty() {
            spans.push(Span::styled(*label, label_style));
            cursor = cursor.saturating_add(label.chars().count() as u16);
        }
        segments.push(SettingsShortcutSegment {
            start: entry_start,
            end: cursor,
            action: *action,
        });
    }

    if !entries.is_empty() {
        spans.push(Span::styled(" | ", label_style));
        cursor = cursor.saturating_add(3);
    }

    let left_start = cursor;
    spans.push(Span::styled("←", key_style));
    cursor = cursor.saturating_add("←".chars().count() as u16);
    segments.push(SettingsShortcutSegment {
        start: left_start,
        end: cursor,
        action: SettingsShortcutAction::PreviousTheme,
    });
    spans.push(Span::styled("/", label_style));
    cursor = cursor.saturating_add(1);
    let right_start = cursor;
    spans.push(Span::styled("→", key_style));
    cursor = cursor.saturating_add("→".chars().count() as u16);
    spans.push(Span::styled(" Select", label_style));
    cursor = cursor.saturating_add(" Select".len() as u16);
    segments.push(SettingsShortcutSegment {
        start: right_start,
        end: cursor,
        action: SettingsShortcutAction::NextTheme,
    });

    if include_delete {
        spans.push(Span::styled(" | ", label_style));
        cursor = cursor.saturating_add(3);
        let entry_start = cursor;
        spans.push(Span::styled("d", key_style));
        cursor = cursor.saturating_add(1);
        spans.push(Span::styled(" Delete theme", label_style));
        cursor = cursor.saturating_add(" Delete theme".len() as u16);
        segments.push(SettingsShortcutSegment {
            start: entry_start,
            end: cursor,
            action: SettingsShortcutAction::DeleteTheme,
        });
    }

    (Line::from(spans), segments, cursor)
}

#[derive(Clone)]
struct Theme {
    name: String,
    primary: Color,
    accent: Color,
    #[allow(dead_code)]
    highlight: Color,
    background: Color,
    surface: Color,
    text: Color,
    primary_hex: String,
    accent_hex: String,
    highlight_hex: String,
    background_hex: String,
    surface_hex: String,
    text_hex: String,
    /// Extra colors only some presets set (e.g. DeskMate's gray command bar).
    extras: Option<ThemeExtras>,
}

/// Colors a preset can set beyond the six theme colors. Themes without them
/// keep MenuMaker's usual derived colors.
#[derive(Clone, Copy)]
struct ThemeExtras {
    /// Shortcut bar background, key and label colors.
    bar_bg: Color,
    bar_key: Color,
    bar_label: Color,
    /// Text on the highlight (selection) color.
    selection_text: Color,
    /// Background of a focused box's title band (text uses the accent).
    focus_band: Color,
}

struct ThemeExtrasDefinition {
    bar_bg: &'static str,
    bar_key: &'static str,
    bar_label: &'static str,
    selection_text: &'static str,
    focus_band: &'static str,
}

impl Theme {
    fn bar_bg(&self) -> Color {
        self.extras.map(|e| e.bar_bg).unwrap_or(self.highlight)
    }

    fn bar_key(&self) -> Color {
        self.extras.map(|e| e.bar_key).unwrap_or(self.accent)
    }

    fn bar_label(&self) -> Color {
        self.extras.map(|e| e.bar_label).unwrap_or(self.surface)
    }

    fn selection_text(&self) -> Color {
        self.extras
            .map(|e| e.selection_text)
            .unwrap_or(self.background)
    }

    fn focus_band(&self) -> Option<Color> {
        self.extras.map(|e| e.focus_band)
    }

    fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let data = fs::read_to_string(path)?;
            match serde_json::from_str::<ThemeFile>(&data) {
                Ok(file) => {
                    if let Some(skin) = file.skin {
                        if let Some(theme) = Theme::from_name(&skin) {
                            return Ok(theme);
                        }
                    }
                    if let Some(colors) = file.colors {
                        if let Some(theme) = Theme::from_colors("Custom", colors) {
                            return Ok(theme);
                        }
                    }
                }
                Err(_) => {}
            }
        }
        let theme = Theme::from_name("nord").unwrap();
        theme.save(path)?;
        Ok(theme)
    }

    fn save(&self, path: &Path) -> Result<()> {
        let file = ThemeFile {
            skin: Some(self.name.clone()),
            colors: Some(ThemeColorOverrides {
                primary: Some(self.primary_hex.clone()),
                accent: Some(self.accent_hex.clone()),
                highlight: Some(self.highlight_hex.clone()),
                background: Some(self.background_hex.clone()),
                surface: Some(self.surface_hex.clone()),
                text: Some(self.text_hex.clone()),
            }),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(&file)?)?;
        Ok(())
    }

    fn from_name(name: &str) -> Option<Self> {
        THEME_PRESETS
            .iter()
            .find(|preset| preset.0 == name)
            .map(|(_, def)| Theme::from_definition(name.to_string(), def))
    }

    fn from_definition(name: String, def: &ThemeDefinition) -> Theme {
        let color = |hex: &str| color_from_hex(hex).unwrap_or(Color::Reset);
        let extras = def.extras.as_ref().map(|e| ThemeExtras {
            bar_bg: color(e.bar_bg),
            bar_key: color(e.bar_key),
            bar_label: color(e.bar_label),
            selection_text: color(e.selection_text),
            focus_band: color(e.focus_band),
        });
        let mut theme = Theme::from_hexes(
            name,
            def.primary,
            def.accent,
            def.highlight,
            def.background,
            def.surface,
            def.text,
        );
        theme.extras = extras;
        theme
    }

    fn from_colors(name: &str, overrides: ThemeColorOverrides) -> Option<Self> {
        Some(Theme::from_hexes(
            name.to_string(),
            overrides.primary.as_deref().unwrap_or("#5E81AC"),
            overrides.accent.as_deref().unwrap_or("#D08770"),
            overrides
                .highlight
                .as_deref()
                .or(overrides.accent.as_deref())
                .unwrap_or("#76B3C5"),
            overrides.background.as_deref().unwrap_or("#3B4252"),
            overrides.surface.as_deref().unwrap_or("#4C566A"),
            overrides.text.as_deref().unwrap_or("#ECEFF4"),
        ))
    }

    fn from_hexes(
        name: String,
        primary: &str,
        accent: &str,
        highlight: &str,
        background: &str,
        surface: &str,
        text: &str,
    ) -> Theme {
        Theme {
            name,
            primary: color_from_hex(primary).unwrap_or(Color::Blue),
            accent: color_from_hex(accent).unwrap_or(Color::Cyan),
            highlight: color_from_hex(highlight).unwrap_or(Color::Cyan),
            background: color_from_hex(background).unwrap_or(Color::Black),
            surface: color_from_hex(surface).unwrap_or(Color::DarkGray),
            text: color_from_hex(text).unwrap_or(Color::White),
            primary_hex: normalize_hex(primary),
            accent_hex: normalize_hex(accent),
            highlight_hex: normalize_hex(highlight),
            background_hex: normalize_hex(background),
            surface_hex: normalize_hex(surface),
            text_hex: normalize_hex(text),
            extras: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ThemeFile {
    skin: Option<String>,
    colors: Option<ThemeColorOverrides>,
}

#[derive(Serialize, Deserialize)]
struct ThemeColorOverrides {
    primary: Option<String>,
    accent: Option<String>,
    #[serde(default)]
    highlight: Option<String>,
    background: Option<String>,
    surface: Option<String>,
    text: Option<String>,
}

struct ThemeDefinition {
    name: &'static str,
    primary: &'static str,
    accent: &'static str,
    highlight: &'static str,
    background: &'static str,
    surface: &'static str,
    text: &'static str,
    extras: Option<ThemeExtrasDefinition>,
}

impl ThemeOption {
    fn from_definition(key: &str, def: &ThemeDefinition) -> Self {
        Self {
            key: key.to_string(),
            label: def.name.to_string(),
            primary_hex: def.primary.to_string(),
            accent_hex: def.accent.to_string(),
            highlight_hex: def.highlight.to_string(),
            background_hex: def.background.to_string(),
            surface_hex: def.surface.to_string(),
            text_hex: def.text.to_string(),
        }
    }
}

const THEME_PRESETS: &[(&str, ThemeDefinition)] = &[
    (
        "classic",
        ThemeDefinition {
            name: "Midnight Classic",
            primary: "#6FC6D4",
            accent: "#0F1A2B",
            highlight: "#9FE6EC",
            background: "#314A63",
            surface: "#416079",
            text: "#F2F8FF",
            extras: None,
        },
    ),
    (
        "nord",
        ThemeDefinition {
            name: "Nord",
            primary: "#5E81AC",
            accent: "#D08770",
            highlight: "#76B3C5",
            background: "#3B4252",
            surface: "#4C566A",
            text: "#ECEFF4",
            extras: None,
        },
    ),
    (
        "gruvbox",
        ThemeDefinition {
            name: "Midnight Mist",
            primary: "#66C3CF",
            accent: "#0E1828",
            highlight: "#96DFE8",
            background: "#2C4156",
            surface: "#3B5A72",
            text: "#F4FBFF",
            extras: None,
        },
    ),
    (
        "dracula",
        ThemeDefinition {
            name: "Midnight Dusk",
            primary: "#6BC6D7",
            accent: "#142033",
            highlight: "#A1E6EC",
            background: "#2E475F",
            surface: "#3E5D78",
            text: "#F5FBFF",
            extras: None,
        },
    ),
    (
        "monokai",
        ThemeDefinition {
            name: "Midnight Deep",
            primary: "#5FC0CD",
            accent: "#0D1725",
            highlight: "#92DDE7",
            background: "#243A50",
            surface: "#344F68",
            text: "#F6FCFF",
            extras: None,
        },
    ),
    (
        "deskmate",
        // Tandy DeskMate 3.05 (EGA palette, as in ~/workspace/deskmate):
        // teal desktop, navy boxes with yellow double borders and text,
        // yellow selection bar, gray command bar with red keys.
        ThemeDefinition {
            name: "DeskMate",
            primary: "#00007B",
            accent: "#BDBD00",
            highlight: "#BDBD00",
            background: "#007B7B",
            surface: "#00007B",
            text: "#BDBD00",
            extras: Some(ThemeExtrasDefinition {
                bar_bg: "#BDBDBD",
                bar_key: "#AA0000",
                bar_label: "#7B0000",
                selection_text: "#00007B",
                focus_band: "#7B0000",
            }),
        },
    ),
];

fn is_preset_theme_key(key: &str) -> bool {
    THEME_PRESETS
        .iter()
        .any(|(preset_key, _)| preset_key == &key)
}

fn saved_theme_key(index: usize) -> String {
    format!("{SAVED_THEME_PREFIX}{index}")
}

fn parse_saved_theme_key(key: &str) -> Option<usize> {
    key.strip_prefix(SAVED_THEME_PREFIX)?.parse::<usize>().ok()
}

#[derive(Clone, Copy)]
struct FooterShortcut {
    key: &'static str,
    label: &'static str,
    action: FooterAction,
}

#[derive(Clone, Copy)]
enum FooterAction {
    Quit,
    Edit,
    Execute,
    NewItem,
    NewBox,
    Delete,
    Settings,
    ScanBin,
}

struct FooterSegment {
    start: u16,
    end: u16,
    action: FooterAction,
}

struct FooterLineData {
    line: Line<'static>,
    segments: Vec<FooterSegment>,
    total_width: u16,
}

const FOOTER_SHORTCUTS: &[FooterShortcut] = &[
    FooterShortcut {
        key: "q",
        label: " Exit",
        action: FooterAction::Quit,
    },
    FooterShortcut {
        key: "e",
        label: " Edit",
        action: FooterAction::Edit,
    },
    FooterShortcut {
        key: "↵",
        label: " Execute",
        action: FooterAction::Execute,
    },
    FooterShortcut {
        key: "n",
        label: " New Item",
        action: FooterAction::NewItem,
    },
    FooterShortcut {
        key: "a",
        label: " Add Box",
        action: FooterAction::NewBox,
    },
    FooterShortcut {
        key: "d",
        label: " Delete",
        action: FooterAction::Delete,
    },
    FooterShortcut {
        key: "s",
        label: " Settings",
        action: FooterAction::Settings,
    },
    FooterShortcut {
        key: "^b",
        label: " Scan ./import",
        action: FooterAction::ScanBin,
    },
];

fn color_from_hex(value: &str) -> Option<Color> {
    let normalized = normalize_hex(value);
    let bytes = normalized.as_bytes();
    let r = u8::from_str_radix(std::str::from_utf8(&bytes[1..3]).ok()?, 16).ok()?;
    let g = u8::from_str_radix(std::str::from_utf8(&bytes[3..5]).ok()?, 16).ok()?;
    let b = u8::from_str_radix(std::str::from_utf8(&bytes[5..7]).ok()?, 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

fn normalize_hex(value: &str) -> String {
    let mut cleaned = value.trim().to_string();
    if !cleaned.starts_with('#') {
        cleaned.insert(0, '#');
    }
    if cleaned.len() != 7 {
        return "#ffffff".into();
    }
    cleaned
}

fn prompt_with_default(prompt: &str, default: &str) -> Result<String> {
    println!("{prompt} [{default}]: ");
    print!("> ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_optional(prompt: &str) -> Result<String> {
    println!("{prompt}: ");
    print!("> ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

fn prompt_bool(prompt: &str, default: bool) -> Result<bool> {
    let default_hint = if default { "Y/n" } else { "y/N" };
    println!("{prompt} ({default_hint})");
    print!("> ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        Ok(default)
    } else if trimmed == "y" || trimmed == "yes" {
        Ok(true)
    } else if trimmed == "n" || trimmed == "no" {
        Ok(false)
    } else {
        Ok(default)
    }
}

fn sanitize_hex_color_input(input: &str) -> Option<String> {
    let mut value = input.trim().to_string();
    if !value.starts_with('#') {
        value.insert(0, '#');
    }
    if value.len() != 7 {
        return None;
    }
    if u32::from_str_radix(&value[1..], 16).is_ok() {
        Some(value)
    } else {
        None
    }
}

fn hex_strings_equal(a: &str, b: &str) -> bool {
    match (sanitize_hex_color_input(a), sanitize_hex_color_input(b)) {
        (Some(mut left), Some(mut right)) => {
            left.make_ascii_lowercase();
            right.make_ascii_lowercase();
            left == right
        }
        _ => false,
    }
}

fn parse_color_field(value: &str) -> Result<Option<String>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    sanitize_hex_color_input(trimmed)
        .map(Some)
        .ok_or_else(|| "Colors must use #RRGGBB format".to_string())
}

fn require_color_field(value: &str, label: &str) -> Result<String, String> {
    parse_color_field(value)?
        .ok_or_else(|| format!("{label} color is required when creating a custom theme"))
}

fn filename_to_label(name: &str) -> String {
    name.replace(['_', '-'], " ")
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_executable_file(entry: &fs::DirEntry) -> bool {
    #[cfg(unix)]
    {
        entry
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("exe"))
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod menumaker_folder_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "menumaker-test-{tag}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_folders_and_fixes_misspelled_sheets_folder() {
        let root = temp_dir("folders");
        fs::create_dir_all(root.join("Spreadhsheets")).unwrap();
        fs::write(root.join("Spreadhsheets/a.csv"), "1,2\n").unwrap();
        create_menumaker_folders(&root);
        for folder in ["Apps", "Art", "Music", "Documents", "Spreadsheets"] {
            assert!(root.join(folder).is_dir(), "{folder} missing");
        }
        assert!(!root.join("Spreadhsheets").exists());
        assert!(root.join("Spreadsheets/a.csv").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copies_legacy_data_without_overwriting() {
        let config = temp_dir("legacy-config");
        let root = temp_dir("legacy-root");
        fs::create_dir_all(config.join("paintings")).unwrap();
        fs::write(config.join("paintings/hat.ans"), "old hat").unwrap();
        fs::write(config.join("paintings/hat.ans~"), "backup").unwrap();
        fs::write(config.join("paintings/cat.ans"), "old cat").unwrap();
        fs::write(config.join("spreadsheet.csv"), "1,2\n").unwrap();
        create_menumaker_folders(&root);
        fs::write(root.join("Art/cat.ans"), "new cat").unwrap();
        copy_legacy_data(&config, &root);
        assert_eq!(fs::read_to_string(root.join("Art/hat.ans")).unwrap(), "old hat");
        assert_eq!(fs::read_to_string(root.join("Art/cat.ans")).unwrap(), "new cat");
        assert!(!root.join("Art/hat.ans~").exists());
        assert!(root.join("Spreadsheets/spreadsheet.csv").is_file());
        // Originals stay put.
        assert!(config.join("paintings/hat.ans").is_file());
        fs::remove_dir_all(config).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn paint_box_keeps_last_drawing_after_cursor_leaves() {
        let dir = temp_dir("paint-keep");
        fs::write(dir.join("a.ans"), "AAAA").unwrap();
        fs::write(dir.join("b.ans"), "BBBB").unwrap();
        let mut art = CategoryState::new_box("Art", BoxKind::Paint, 1);
        art.folder = dir.to_string_lossy().to_string();
        art.refresh_files(&dir);
        assert_eq!(art.files.len(), 2);
        let pic = |row: usize| art.pictures[row].clone().unwrap();
        // Nothing chosen yet: the first drawing.
        assert!(Rc::ptr_eq(&art.preview_picture(None).unwrap(), &pic(0)));
        // Cursor on b, then off the box: b stays.
        assert!(Rc::ptr_eq(&art.preview_picture(Some(1)).unwrap(), &pic(1)));
        assert!(Rc::ptr_eq(&art.preview_picture(None).unwrap(), &pic(1)));
        // Saved to the config and restored, and survives a folder re-read.
        let cfg = art.to_config();
        assert_eq!(cfg.picture.as_deref(), Some(dir.join("b.ans").to_string_lossy().as_ref()));
        let mut again = CategoryState::from_config("Art", &cfg);
        again.refresh_files(&dir);
        let shown = again.preview_picture(None).unwrap();
        assert!(Rc::ptr_eq(&shown, again.pictures[1].as_ref().unwrap()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn menu_file_without_pages_still_loads() {
        // A menus.json from before Pages: no pages, no page settings.
        let old = r#"{"categories":{"Tools":{"expanded":true,"items":[]}},"app_settings":{"columns":2}}"#;
        let file: MenuFile = serde_json::from_str(old).unwrap();
        assert!(file.pages.is_empty());
        assert!(!file.app_settings.pages_enabled);
        let pages = pages_from_file(&file);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].name, pages::DEFAULT_FIRST_PAGE);
        // Saving it again writes no page keys, so the file stays the same shape.
        let json = serde_json::to_string(&file).unwrap();
        assert!(!json.contains("pages"), "{json}");
    }

    #[test]
    fn pages_round_trip_with_their_own_boxes() {
        let mut file: MenuFile = serde_json::from_str(r#"{"categories":{}}"#).unwrap();
        file.app_settings.pages_enabled = true;
        file.app_settings.first_page_name = Some("Home".into());
        file.app_settings.current_page = Some(1);
        let games = CategoryState::new_box("Arcade", BoxKind::Menu, 1);
        file.pages.push(PageFile {
            name: "Games".into(),
            categories: categories_to_map(&[games]),
        });
        let json = serde_json::to_string(&file).unwrap();
        let back: MenuFile = serde_json::from_str(&json).unwrap();
        assert!(back.app_settings.pages_enabled);
        assert_eq!(back.app_settings.current_page, Some(1));
        let pages = pages_from_file(&back);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].name, "Home");
        assert_eq!(pages[1].name, "Games");
        assert_eq!(pages[1].categories.len(), 1);
        assert_eq!(pages[1].categories[0].name, "Arcade");
    }

    #[test]
    fn page_names_in_settings_keep_one_blank_slot() {
        let theme = Theme::from_name("default").unwrap_or_else(|| {
            Theme::from_hexes("t".into(), "#000000", "#000000", "#000000", "#000000", "#000000", "#ffffff")
        });
        let mut form = SettingsFormState::new(
            "T".into(), 1, true, String::new(), Vec::new(), SettingsField::Pages, &theme, false,
        )
        .with_pages(true, vec!["Home".into()]);
        assert_eq!(form.page_names, vec!["Home".to_string(), String::new()]);
        form.selected_field = SettingsField::PageName(1);
        for c in "Work".chars() {
            form.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(form.page_names, vec!["Home".to_string(), "Work".into(), String::new()]);
        form.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(form.page_names, vec!["Home".to_string(), String::new()]);
        // Tab walks Pages -> each name -> Theme, and back.
        form.selected_field = SettingsField::Pages;
        form.next_field();
        assert!(form.selected_field == SettingsField::PageName(0));
        form.next_field();
        form.next_field();
        assert!(form.selected_field == SettingsField::Theme);
        form.previous_field();
        assert!(form.selected_field == SettingsField::PageName(1));
    }

    #[test]
    fn tab_jumps_between_boxes() {
        let title = |i| DisplayEntry::Category { category_index: i };
        let item = |c, i| DisplayEntry::Item { category_index: c, item_index: i };
        let entries = vec![title(0), item(0, 0), item(0, 1), title(1), title(2), item(2, 0)];
        assert_eq!(box_jump(&entries, 2, true), Some(3)); // inside box 0 -> box 1
        assert_eq!(box_jump(&entries, 5, true), Some(0)); // last box wraps to first
        assert_eq!(box_jump(&entries, 1, false), Some(4)); // box 0 back wraps to box 2
        assert_eq!(box_jump(&entries, 3, false), Some(0));
        assert_eq!(box_jump(&[], 0, true), None);
    }

    #[test]
    fn new_file_name_skips_existing() {
        let dir = temp_dir("names");
        fs::write(dir.join("sheet_1.csv"), "").unwrap();
        assert_eq!(new_file_name(&dir, "sheet", "csv"), dir.join("sheet_2.csv"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn spreadsheet_box_lists_csv_files_in_its_folder() {
        let dir = temp_dir("sheets");
        fs::write(dir.join("b.csv"), "1\n").unwrap();
        fs::write(dir.join("a.csv"), "2\n").unwrap();
        fs::write(dir.join("notes.txt"), "x").unwrap();
        let mut sheet = CategoryState::new_box("Sheets", BoxKind::Spreadsheet, 1);
        sheet.folder = dir.to_string_lossy().to_string();
        assert!(sheet.sheet_is_folder());
        sheet.refresh_files(&dir);
        assert_eq!(sheet.row_count(), 2);
        assert_eq!(sheet.row_label(0), "a.csv");
        // A single .csv file keeps the old preview behaviour.
        sheet.folder = dir.join("a.csv").to_string_lossy().to_string();
        assert!(!sheet.sheet_is_folder());
        sheet.refresh_files(&dir);
        assert_eq!(sheet.row_count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn documents_box_lists_only_text_files() {
        let dir = temp_dir("docs");
        fs::write(dir.join("letter.txt"), "Dear Frank").unwrap();
        fs::write(dir.join("readme"), "plain").unwrap();
        fs::write(dir.join("image.bin"), [0u8, 159, 146, 150]).unwrap();
        let mut docs = CategoryState::new_box("Documents", BoxKind::Documents, 1);
        docs.folder = dir.to_string_lossy().to_string();
        docs.refresh_files(&dir);
        let labels: Vec<String> = (0..docs.row_count()).map(|r| docs.row_label(r)).collect();
        assert_eq!(labels, vec!["letter.txt", "readme"]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// An app with the given (name, kind, column) boxes, saving into `dir`.
    fn test_app(dir: &Path, columns: u16, boxes: &[(&str, BoxKind, u16)]) -> AppState {
        let theme = Theme::from_hexes("t".into(), "#000000", "#000000", "#000000", "#000000", "#000000", "#ffffff");
        let mut app = AppState {
            categories: boxes
                .iter()
                .map(|(name, kind, column)| CategoryState::new_box(name, *kind, *column))
                .collect(),
            custom_colors: Vec::new(),
            saved_themes: Vec::new(),
            column_count: columns,
            current_index: 0,
            display_entries: Vec::new(),
            column_map: Vec::new(),
            should_quit: false,
            pending_command: None,
            pending_action: None,
            status_message: None,
            paths: AppPaths {
                config_dir: dir.to_path_buf(),
                menu_file: dir.join("menus.json"),
                theme_file: dir.join("theme.json"),
            },
            theme,
            theme_key: String::new(),
            title: "T".into(),
            active_popup: None,
            box_drag: None,
            drag_boxes: true,
            pages_enabled: false,
            pages: vec![PageState { name: "Home".into(), categories: Vec::new() }],
            current_page: 0,
        };
        app.rebuild_display();
        app
    }

    fn column_names(app: &AppState, column: u16) -> Vec<String> {
        app.categories
            .iter()
            .filter(|c| c.column == column)
            .map(|c| c.name.clone())
            .collect()
    }

    fn select_box(app: &mut AppState, name: &str) {
        app.current_index = app
            .display_entries
            .iter()
            .position(|e| matches!(e, DisplayEntry::Category { category_index }
                if app.categories[*category_index].name == name))
            .unwrap();
    }

    #[test]
    fn boxes_reorder_within_and_across_columns_and_persist() {
        let dir = temp_dir("order");
        let mut app = test_app(
            &dir,
            2,
            &[("A", BoxKind::Notes, 1), ("B", BoxKind::Notes, 1), ("C", BoxKind::Notes, 1), ("D", BoxKind::Notes, 2)],
        );
        assert_eq!(column_names(&app, 1), ["A", "B", "C"]);
        // Keyboard: C up twice -> top; already at the top says so.
        select_box(&mut app, "C");
        app.move_current_box(KeyCode::Up);
        app.move_current_box(KeyCode::Up);
        assert_eq!(column_names(&app, 1), ["C", "A", "B"]);
        app.move_current_box(KeyCode::Up);
        assert!(app.status_message.as_deref().unwrap_or("").contains("top"));
        // The selection follows the moved box.
        assert!(matches!(app.display_entries[app.current_index],
            DisplayEntry::Category { category_index } if app.categories[category_index].name == "C"));
        // A down -> bottom.
        select_box(&mut app, "A");
        app.move_current_box(KeyCode::Down);
        assert_eq!(column_names(&app, 1), ["C", "B", "A"]);
        // B right -> column 2 at the same height (second), i.e. below D.
        select_box(&mut app, "B");
        app.move_current_box(KeyCode::Right);
        assert_eq!(column_names(&app, 2), ["D", "B"]);
        // Drop (as the mouse does) A above D.
        let a = app.categories.iter().position(|c| c.name == "A").unwrap();
        let d = app.categories.iter().position(|c| c.name == "D").unwrap();
        assert!(app.place_box(a, 2, Some(d)));
        assert_eq!(column_names(&app, 2), ["A", "D", "B"]);
        assert!(!app.place_box(app.categories.iter().position(|c| c.name == "A").unwrap(), 2,
            Some(app.categories.iter().position(|c| c.name == "D").unwrap())));
        // The order survives a save and reload.
        let file = MenuFile::load(&app.paths.menu_file).unwrap();
        let mut loaded: Vec<CategoryState> =
            file.categories.iter().map(|(n, cfg)| CategoryState::from_config(n, cfg)).collect();
        loaded.sort_by_key(|cat| (cat.column, cat.order, cat.name.clone()));
        let names: Vec<(u16, String)> = loaded.iter().map(|c| (c.column, c.name.clone())).collect();
        assert_eq!(
            names,
            [(1, "C".to_string()), (2, "A".into()), (2, "D".into()), (2, "B".into())]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn spanning_file_box_lists_files_in_columns() {
        let dir = temp_dir("filecols");
        for name in ["a", "b", "c", "d", "e"] {
            fs::write(dir.join(format!("{name}.txt")), "x").unwrap();
        }
        let mut app = test_app(&dir, 3, &[("Docs", BoxKind::Documents, 1)]);
        app.categories[0].folder = dir.to_string_lossy().to_string();
        app.categories[0].span = 2;
        app.rebuild_display();
        assert_eq!(app.file_columns(0), 2);
        let columns = column_areas(Rect::new(0, 0, 92, 30), 3);
        let layout = app.all_box_layouts(&columns).remove(0);
        // 5 files in 2 columns: 3 rows, a b c down the left, d e on the right.
        assert_eq!(layout.outer.height, 2 + 3);
        let rects: Vec<Rect> = layout.rows.iter().skip(1).map(|(_, r)| *r).collect();
        assert_eq!(rects.len(), 5);
        assert_eq!(rects[0].y, rects[3].y);
        assert_eq!(rects[1].y, rects[4].y);
        assert!(rects[3].x > rects[0].x + rects[0].width);
        // Left/Right jump between the file columns.
        app.current_index = 1; // a.txt
        app.move_selection_sideways(true);
        assert!(matches!(app.display_entries[app.current_index], DisplayEntry::Item { item_index: 3, .. }));
        app.move_selection_sideways(false);
        assert!(matches!(app.display_entries[app.current_index], DisplayEntry::Item { item_index: 0, .. }));
        // c.txt has nothing to its right (5 files): stays.
        app.current_index = 3;
        app.move_selection_sideways(true);
        assert_eq!(app.current_index, 3);
        // A box that does not span columns keeps one column; a span past the
        // last desktop column is cut to what fits.
        app.categories[0].span = 1;
        assert_eq!(app.file_columns(0), 1);
        app.categories[0].span = 3;
        app.categories[0].column = 2;
        assert_eq!(app.file_columns(0), 2);
        fs::remove_dir_all(dir).unwrap();
    }
}
