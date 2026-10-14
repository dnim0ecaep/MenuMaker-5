//! Optional box looks, picked per box in the box form ("Style"):
//!
//! * `SpiralCalendar` (Calendar boxes): a desk calendar page with spiral
//!   rings across the top, a month banner, and a thick "L" drop shadow under
//!   every day column.
//! * `TodoPad` (Notes boxes): a spiral-bound "TO DO:" pad with rings down the
//!   left side and ruled lines. Each line of the notes is one item; start a
//!   line with `[x]` (or `x `) to tick it off (drawn crossed out).
//!
//! * `AddressBook` (Address Book boxes): a spiral-bound address book: rings
//!   through the left edge, a light blue page titled with the box's name,
//!   A-Z index tabs (AB, CD, ... YZ) down the right, and a gray page edge
//!   behind. One contact per line (name, then phone on the
//!   right). Its own default colors; the box's Custom Theme colors replace
//!   them (Border: rings and page edge, Background: page, Text: writing).
//!
//! * `MusicPlayer` (Music boxes): a little media-player window: a silver
//!   frame with a navy "◄ Music" tab on top, cyan song names on navy, and a
//!   light control strip (◄◄ ▶ ■ ▮▮ ►►) along the bottom. Those are its
//!   own default colors; the box's Custom Theme colors replace them (Border:
//!   frame and strip, Background: screen, Text: songs).
//!
//! * `Clipboard` (To-Do boxes): a clipboard: a dark red board with rounded
//!   corners, a silver clip on top holding the box's name, and a white
//!   sheet with a checkbox (☑ / ☐) and a ruled line for every item. Its own
//!   default colors; the box's Custom Theme colors replace them (Border:
//!   board, Background: sheet, Text: items).
//!
//! * `WeatherPanel` (Weather boxes): a gray panel in a pale gold double
//!   frame with a dark gold `◄ title ►` band: the place in white, the
//!   conditions with a yellow sun / icon, the wind, and today's low-high in
//!   yellow, with a sun-and-cloud and a red thermometer on the right. Its own
//!   default colors; the box's Custom Theme colors replace them (Border:
//!   frame and band, Background: panel, Text: writing).
//!
//! Calendar, Address Book, Music, To-Do and Weather boxes get their drawn look by default
//! (`default_for`); every other kind defaults to `Classic`, the normal
//! double-line frame.
//! Picking Classic on a Calendar box is saved explicitly.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::boxes::BoxKind;

/// How a box is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxStyle {
    /// The normal double-line frame with the name in the top border.
    #[default]
    Classic,
    /// Desk calendar page (Calendar boxes).
    SpiralCalendar,
    /// Spiral to-do pad (Notes boxes).
    TodoPad,
    /// Ring-bound notebook page with ruled lines (Address Book boxes).
    AddressBook,
    /// Media-player window with a control strip (Music boxes).
    MusicPlayer,
    /// A clipboard holding a checklist sheet (To-Do boxes).
    Clipboard,
    /// Gray panel with a gold frame, a sun-and-cloud and a thermometer
    /// (Weather boxes).
    WeatherPanel,
}

/// Narrowest box (outer width) the spiral calendar fits in: 7 day columns of
/// 3 cells (2 digits + shadow) plus a one-cell margin each side. Wider boxes
/// get roomier 4+ cell columns.
pub const CALENDAR_MIN_WIDTH: u16 = 23;
/// Narrowest box the to-do pad fits in.
pub const PAD_MIN_WIDTH: u16 = 12;
/// Narrowest box the address book fits in.
pub const BOOK_MIN_WIDTH: u16 = 20;
/// Ruled lines an address book page always shows, however few contacts.
pub const BOOK_MIN_LINES: usize = 8;

/// Narrowest box the music player fits in (the control strip).
pub const PLAYER_MIN_WIDTH: u16 = 20;

/// Narrowest box the clipboard fits in.
pub const CLIPBOARD_MIN_WIDTH: u16 = 14;
/// Checklist lines the clipboard's sheet shows when Max rows is blank.
pub const CLIPBOARD_DEFAULT_LINES: usize = 6;

/// Narrowest box the weather panel fits in.
pub const WEATHER_MIN_WIDTH: u16 = 22;

/// Ruled lines a to-do pad shows when Max rows is blank.
pub const PAD_DEFAULT_LINES: usize = 8;

impl BoxStyle {
    pub fn is_classic(&self) -> bool {
        *self == BoxStyle::Classic
    }

    pub fn label(self) -> &'static str {
        match self {
            BoxStyle::Classic => "Classic (double-line frame)",
            BoxStyle::SpiralCalendar => "Spiral desk calendar",
            BoxStyle::TodoPad => "Spiral to-do pad",
            BoxStyle::AddressBook => "Ring-bound notebook",
            BoxStyle::MusicPlayer => "Music player",
            BoxStyle::Clipboard => "Clipboard",
            BoxStyle::WeatherPanel => "Weather panel",
        }
    }

    /// The style a box of `kind` gets until one is picked in the box form:
    /// Calendar boxes are spiral desk calendars, everything else classic.
    pub fn default_for(kind: BoxKind) -> Self {
        match kind {
            BoxKind::Calendar => BoxStyle::SpiralCalendar,
            BoxKind::Contacts => BoxStyle::AddressBook,
            BoxKind::Music => BoxStyle::MusicPlayer,
            BoxKind::Todo => BoxStyle::Clipboard,
            BoxKind::Weather => BoxStyle::WeatherPanel,
            _ => BoxStyle::Classic,
        }
    }

    /// Styles offered for a box kind (always starts with `Classic`).
    pub fn choices(kind: BoxKind) -> &'static [BoxStyle] {
        match kind {
            BoxKind::Calendar => &[BoxStyle::Classic, BoxStyle::SpiralCalendar],
            BoxKind::Notes => &[BoxStyle::Classic, BoxStyle::TodoPad],
            BoxKind::Contacts => &[BoxStyle::Classic, BoxStyle::AddressBook],
            BoxKind::Music => &[BoxStyle::Classic, BoxStyle::MusicPlayer],
            BoxKind::Todo => &[BoxStyle::Classic, BoxStyle::Clipboard],
            BoxKind::Weather => &[BoxStyle::Classic, BoxStyle::WeatherPanel],
            _ => &[BoxStyle::Classic],
        }
    }

    /// Whether this style can be used on `kind`.
    pub fn fits(self, kind: BoxKind) -> bool {
        Self::choices(kind).contains(&self)
    }

    pub fn next(self, kind: BoxKind) -> Self {
        let list = Self::choices(kind);
        let idx = list.iter().position(|s| *s == self).unwrap_or(0);
        list[(idx + 1) % list.len()]
    }

    pub fn previous(self, kind: BoxKind) -> Self {
        let list = Self::choices(kind);
        let idx = list.iter().position(|s| *s == self).unwrap_or(0);
        list[(idx + list.len() - 1) % list.len()]
    }

    /// Rows this style adds on top of the classic box height (the classic box
    /// is `2 + content rows`).
    pub fn extra_rows(self) -> u16 {
        match self {
            BoxStyle::Classic => 0,
            // Ring row + the shadow row under the last week.
            BoxStyle::SpiralCalendar => 2,
            // The "TO DO:" header row.
            BoxStyle::TodoPad => 1,
            // Same rows as classic: title on top, the page edge below.
            BoxStyle::AddressBook => 0,
            // The control strip under the songs.
            BoxStyle::MusicPlayer => 1,
            // The sheet's top and bottom margins.
            BoxStyle::Clipboard => 2,
            // Same rows as classic: the frame round the report lines.
            BoxStyle::WeatherPanel => 0,
        }
    }

    /// The style actually drawn: falls back to `Classic` when the box is
    /// collapsed, the style doesn't fit the kind, or the box is too narrow.
    pub fn effective(self, kind: BoxKind, expanded: bool, outer_width: u16) -> Self {
        if !expanded || !self.fits(kind) {
            return BoxStyle::Classic;
        }
        match self {
            BoxStyle::SpiralCalendar if outer_width < CALENDAR_MIN_WIDTH => BoxStyle::Classic,
            BoxStyle::TodoPad if outer_width < PAD_MIN_WIDTH => BoxStyle::Classic,
            BoxStyle::AddressBook if outer_width < BOOK_MIN_WIDTH => BoxStyle::Classic,
            BoxStyle::MusicPlayer if outer_width < PLAYER_MIN_WIDTH => BoxStyle::Classic,
            BoxStyle::Clipboard if outer_width < CLIPBOARD_MIN_WIDTH => BoxStyle::Classic,
            BoxStyle::WeatherPanel if outer_width < WEATHER_MIN_WIDTH => BoxStyle::Classic,
            other => other,
        }
    }
}

/// Colors shared by the styled renderers.
pub struct StyleColors {
    /// The desktop behind the boxes (shows through ring holes).
    pub desktop: Color,
    /// The page (box background).
    pub page: Color,
    /// Text, rings and shadows.
    pub ink: Color,
    /// The month banner / accent (the box's border color).
    pub accent: Color,
    /// Style for the banner / header when the box is selected.
    pub selected: Style,
}

/// One to-do item: (checked, text).
pub fn todo_items(notes: &str) -> Vec<(bool, String)> {
    notes
        .trim_end()
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            for (prefix, checked) in [("[x]", true), ("[X]", true), ("[ ]", false)] {
                if let Some(rest) = trimmed.strip_prefix(prefix) {
                    return (checked, rest.trim_start().to_string());
                }
            }
            if let Some(rest) = trimmed.strip_prefix("x ").or_else(|| trimmed.strip_prefix("X ")) {
                return (true, rest.to_string());
            }
            (false, line.trim_end().to_string())
        })
        .collect()
}

/// Write `text` at (x, y), clipped to `clip`, one cell per character.
fn put(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, style: Style) {
    if y < clip.y || y >= clip.y + clip.height || x >= clip.x + clip.width || x < clip.x {
        return;
    }
    let max = (clip.x + clip.width - x) as usize;
    buf.set_stringn(x, y, text, max, style);
}

/// Truncate to `width` display cells.
fn fit(text: &str, width: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if UnicodeWidthStr::width(format!("{out}{ch}").as_str()) > width {
            break;
        }
        out.push(ch);
    }
    out
}

/// The spiral desk calendar page.
///
/// * `title` – month banner text (e.g. "OCTOBER 2026")
/// * `weeks` – the month grid, Sunday first
/// * `today` – day of the month to highlight, if it is in this month
/// * `extra` – lines under the grid (today's appointments)
pub fn draw_spiral_calendar(
    buf: &mut Buffer,
    outer: Rect,
    colors: &StyleColors,
    selected: bool,
    title: &str,
    weeks: &[[Option<u32>; 7]],
    today: Option<u32>,
    extra: &[String],
) {
    let outer = outer.intersection(buf.area);
    if outer.width < CALENDAR_MIN_WIDTH || outer.height < 3 {
        return;
    }
    let (x0, y0, w) = (outer.x, outer.y, outer.width);
    let desk = Style::default().bg(colors.desktop);
    let page = Style::default().bg(colors.page).fg(colors.ink);
    let ink = Style::default().bg(colors.page).fg(colors.ink);

    // Row 0 is above the page: the tops of the rings over the desktop.
    buf.set_style(Rect { height: 1, ..outer }, desk);
    put(buf, outer, x0, y0, &" ".repeat(w as usize), desk);
    // The page itself.
    let page_rect = Rect { y: y0 + 1, height: outer.height - 1, ..outer };
    for y in page_rect.y..page_rect.y + page_rect.height {
        put(buf, outer, x0, y, &" ".repeat(w as usize), page);
    }
    // Rings: an arc over the edge, curling down into a punched hole.
    let ring_top = Style::default().bg(colors.desktop).fg(colors.page).add_modifier(Modifier::BOLD);
    let hole = Style::default().bg(colors.page).fg(colors.desktop);
    let mut x = x0 + 2;
    while x + 2 < x0 + w {
        put(buf, outer, x, y0, "╭╮", ring_top);
        put(buf, outer, x, y0 + 1, "╰", ink.add_modifier(Modifier::BOLD));
        put(buf, outer, x + 1, y0 + 1, "●", hole);
        x += 4;
    }

    // Month banner.
    let banner_style = if selected {
        colors.selected
    } else {
        Style::default().bg(colors.accent).fg(colors.ink).add_modifier(Modifier::BOLD)
    };
    let banner_width = (w - 2) as usize;
    let label = fit(&title.to_uppercase(), banner_width);
    let pad = banner_width.saturating_sub(UnicodeWidthStr::width(label.as_str()));
    let banner = format!("{}{label}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2));
    put(buf, outer, x0 + 1, y0 + 2, &banner, banner_style);

    // Day columns: digits right-aligned, the shadow in the cell after them.
    let usable = w - 2;
    let cw = usable / 7;
    let left = x0 + 1 + (usable - cw * 7) / 2;
    let col_x = |c: usize| left + c as u16 * cw;
    let digits_x = |c: usize| col_x(c) + cw - 3;
    let shadow_x = |c: usize| col_x(c) + cw - 1;
    let bold = ink.add_modifier(Modifier::BOLD);
    for (c, name) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
        put(buf, outer, digits_x(c), y0 + 3, name, bold);
    }

    let grid_y = y0 + 4;
    let today_style = Style::default()
        .bg(colors.ink)
        .fg(colors.page)
        .add_modifier(Modifier::BOLD);
    let shadow = Style::default().bg(colors.page).fg(colors.ink);
    for c in 0..7 {
        let rows: Vec<usize> = (0..weeks.len()).filter(|r| weeks[*r][c].is_some()).collect();
        let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else {
            continue;
        };
        for &r in &rows {
            let y = grid_y + r as u16;
            let day = weeks[r][c].unwrap_or_default();
            let style = if Some(day) == today { today_style } else { bold };
            put(buf, outer, digits_x(c), y, &format!("{day:>2}"), style);
            // The shadow starts half a row down, beside the first date.
            let bar = if r == first { "▖" } else { "▌" };
            put(buf, outer, shadow_x(c), y, bar, shadow);
        }
        // Bottom of the "L", offset half a cell right and down.
        let y = grid_y + last as u16 + 1;
        put(buf, outer, digits_x(c), y, "▝▀▘", shadow);
    }

    // Today's appointments under the grid.
    let mut y = grid_y + weeks.len() as u16 + 2;
    for line in extra {
        put(buf, outer, x0 + 2, y, &fit(line, (w - 4) as usize), ink);
        y += 1;
    }
}

/// The spiral to-do pad.
///
/// `header` is shown bold at the top (e.g. "TO DO:"), `items` fill the
/// ruled lines from the top; remaining lines stay blank.
pub fn draw_todo_pad(
    buf: &mut Buffer,
    outer: Rect,
    colors: &StyleColors,
    selected: bool,
    header: &str,
    items: &[(bool, String)],
) {
    let outer = outer.intersection(buf.area);
    if outer.width < PAD_MIN_WIDTH || outer.height < 3 {
        return;
    }
    let (x0, y0, w, h) = (outer.x, outer.y, outer.width, outer.height);
    let desk = Style::default().bg(colors.desktop);
    let page = Style::default().bg(colors.page).fg(colors.ink);
    let ring = Style::default().bg(colors.desktop).fg(colors.page).add_modifier(Modifier::BOLD);
    let hole = Style::default().bg(colors.page).fg(colors.desktop);

    for y in y0..y0 + h {
        put(buf, outer, x0, y, " ", desk);
        put(buf, outer, x0 + 1, y, &" ".repeat((w - 1) as usize), page);
        // A coil on every row: hooked around the edge into a hole.
        put(buf, outer, x0, y, "⊂", ring);
        put(buf, outer, x0 + 1, y, "●", hole);
    }

    let text_x = x0 + 3;
    let line_width = (x0 + w).saturating_sub(text_x + 1) as usize;
    let header_style = if selected {
        colors.selected
    } else {
        page.add_modifier(Modifier::BOLD)
    };
    put(buf, outer, text_x, y0 + 1, &fit(header, line_width), header_style);

    // Ruled lines: rows y0+2 ..= y0+h-2 (the last row is the bottom margin).
    let ruled = Style::default()
        .bg(colors.page)
        .fg(colors.ink)
        .add_modifier(Modifier::UNDERLINED);
    let lines = h.saturating_sub(3) as usize;
    for i in 0..lines {
        let y = y0 + 2 + i as u16;
        let (checked, text) = items
            .get(i)
            .map(|(c, t)| (*c, t.as_str()))
            .unwrap_or((false, ""));
        let body_width = line_width;
        let body = fit(text, body_width);
        let fill = body_width.saturating_sub(UnicodeWidthStr::width(body.as_str()));
        let style = if checked {
            ruled.add_modifier(Modifier::CROSSED_OUT | Modifier::DIM)
        } else {
            ruled
        };
        put(buf, outer, text_x, y, &body, style);
        put(
            buf,
            outer,
            text_x + UnicodeWidthStr::width(body.as_str()) as u16,
            y,
            &" ".repeat(fill),
            ruled,
        );
    }
}

/// One line of an address book page.
pub struct BookLine<'a> {
    /// Screen row it is drawn on.
    pub y: u16,
    pub name: &'a str,
    pub phone: &'a str,
    pub selected: bool,
}

/// The address book's colors. Its own default is a light blue page with
/// silver rings, light gray index tabs and a gray page edge behind; a box's Custom Theme colors replace the matching parts
/// (see [`BookColors::with_box_colors`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookColors {
    pub page: Color,
    /// Writing, and the rings' bars across the page.
    pub ink: Color,
    /// The silver ring ends.
    pub ring: Color,
    /// The page edge showing behind the page.
    pub edge: Color,
    /// The index tabs (alternating two shades so each stands apart).
    pub tab: Color,
    pub tab_alt: Color,
    pub tab_text: Color,
}

impl Default for BookColors {
    fn default() -> Self {
        Self {
            page: Color::Rgb(0xB0, 0xC8, 0xD8),
            ink: Color::Rgb(0x00, 0x00, 0x00),
            ring: Color::Rgb(0xC0, 0xC0, 0xC0),
            edge: Color::Rgb(0x80, 0x80, 0x80),
            tab: Color::Rgb(0xE8, 0xE8, 0xE8),
            tab_alt: Color::Rgb(0xD0, 0xD0, 0xD0),
            tab_text: Color::Rgb(0x00, 0x00, 0x00),
        }
    }
}

impl BookColors {
    /// The default book with a box's own colors (each optional) on top:
    /// Border colors the rings and page edge, Background the page, Text the
    /// writing.
    pub fn with_box_colors(border: Option<Color>, background: Option<Color>, text: Option<Color>) -> Self {
        let mut colors = Self::default();
        if let Some(border) = border {
            colors.ring = border;
            colors.edge = border;
        }
        if let Some(background) = background {
            colors.page = background;
        }
        if let Some(text) = text {
            colors.ink = text;
        }
        colors
    }
}

/// Index tab labels for `count` tabs: the alphabet in equal runs, each
/// labelled with its first and last letter (13 tabs: AB, CD, ... YZ).
pub fn index_tabs(count: usize) -> Vec<String> {
    let letters: Vec<char> = ('A'..='Z').collect();
    let count = count.clamp(1, 13);
    (0..count)
        .map(|i| {
            let first = letters[i * 26 / count];
            let last = letters[(i + 1) * 26 / count - 1];
            format!("{first}{last}")
        })
        .collect()
}

/// The address book, after DeskMate's: rings through the page's left edge,
/// `title` across the top of the page, the contacts below (`lines`, already
/// scrolled; the selected one lit in reverse), A-Z index tabs sticking out
/// down the right, and a page edge showing behind.
/// `desktop` shows around it; `focus` is the title's style while the box is
/// selected.
///
/// ```text
/// ◖━◗ Address Book    ▄
/// ◖━◗ Mom   555-0199  AB
/// ◖━◗ dad             CD
/// ◖━◗                 EF
///   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
/// ```
pub fn draw_address_book(
    buf: &mut Buffer,
    outer: Rect,
    colors: &BookColors,
    desktop: Color,
    focus: Option<Style>,
    title: &str,
    lines: &[BookLine],
) {
    let outer = outer.intersection(buf.area);
    if outer.width < BOOK_MIN_WIDTH || outer.height < 3 {
        return;
    }
    let (x0, y0, w, h) = (outer.x, outer.y, outer.width, outer.height);
    // Columns: ring end outside | page x0+1..=right | tabs (2).
    let right = x0 + w - 3;
    let tab_x = right + 1;
    let last_row = y0 + h - 2;
    let page = Style::default().bg(colors.page).fg(colors.ink);
    for y in y0..y0 + h {
        put(buf, outer, x0, y, &" ".repeat(w as usize), Style::default().bg(desktop));
    }
    for y in y0..=last_row {
        put(buf, outer, x0 + 1, y, &" ".repeat((right - x0) as usize), page);
    }

    // The page edge behind: above the tabs and along the bottom, one cell
    // out.
    let edge = Style::default().fg(colors.edge).bg(desktop);
    put(buf, outer, tab_x, y0, "▄", edge);
    put(buf, outer, x0 + 2, last_row + 1, &"▀".repeat((tab_x - x0 - 1) as usize), edge);

    // Rings: a silver end outside the page, a black bar across its edge,
    // and the other end on the page.
    let ring_out = Style::default().fg(colors.ring).bg(desktop).add_modifier(Modifier::BOLD);
    let ring_in = Style::default().fg(colors.ring).bg(colors.page).add_modifier(Modifier::BOLD);
    for y in y0..=last_row {
        put(buf, outer, x0, y, "◖", ring_out);
        put(buf, outer, x0 + 1, y, "━", page.add_modifier(Modifier::BOLD));
        put(buf, outer, x0 + 2, y, "◗", ring_in);
    }

    // Index tabs down the right, below the top row.
    let tab_rows = (last_row - y0) as usize;
    for (i, label) in index_tabs(tab_rows).iter().enumerate() {
        let bg = if i % 2 == 0 { colors.tab } else { colors.tab_alt };
        let style = Style::default().bg(bg).fg(colors.tab_text).add_modifier(Modifier::BOLD);
        put(buf, outer, tab_x, y0 + 1 + i as u16, label, style);
    }

    // The title across the top of the page.
    let text_x = x0 + 4;
    let text_w = right.saturating_sub(text_x) as usize;
    let title_style = focus.unwrap_or(page.add_modifier(Modifier::BOLD));
    put(buf, outer, text_x, y0, &fit(title, text_w), title_style);

    // Contacts: name ............ phone
    for line in lines {
        if line.y <= y0 || line.y > last_row || text_w < 4 {
            continue;
        }
        let style = if line.selected {
            Style::default().bg(colors.ink).fg(colors.page).add_modifier(Modifier::BOLD)
        } else {
            page
        };
        let phone = fit(line.phone, text_w.saturating_sub(6));
        let phone_w = UnicodeWidthStr::width(phone.as_str());
        let name_w = text_w.saturating_sub(phone_w + usize::from(phone_w > 0));
        let name = fit(line.name, name_w);
        let gap = text_w - UnicodeWidthStr::width(name.as_str()) - phone_w;
        put(buf, outer, text_x, line.y, &format!("{name}{}{phone}", " ".repeat(gap)), style);
    }
}

/// One song in the music player: the row it is drawn on, its name, and
/// whether it is selected.
pub struct SongLine<'a> {
    /// Where it is drawn (its column of a multi-column list).
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub name: &'a str,
    pub selected: bool,
}

/// The control strip's buttons.
pub const PLAYER_CONTROLS: &str = "◄◄  ▶  ■  ▮▮  ►►";

/// The music player's colors. Its own default is the classic silver / navy
/// / cyan player; a box's Custom Theme colors replace the matching parts
/// (see [`PlayerColors::with_box_colors`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerColors {
    /// Frame around the window.
    pub frame: Color,
    /// The screen behind the songs (and the title tab).
    pub screen: Color,
    /// Song names.
    pub text: Color,
    /// The title in the tab.
    pub title: Color,
    /// The control strip panel and its buttons.
    pub strip: Color,
    pub keys: Color,
}

impl Default for PlayerColors {
    fn default() -> Self {
        Self {
            frame: Color::Rgb(0xC0, 0xC0, 0xC0),
            screen: Color::Rgb(0x00, 0x00, 0x80),
            text: Color::Rgb(0x00, 0xFF, 0xFF),
            title: Color::Rgb(0xFF, 0xFF, 0xFF),
            strip: Color::Rgb(0xB8, 0xC8, 0xE0),
            keys: Color::Rgb(0x00, 0x00, 0x00),
        }
    }
}

impl PlayerColors {
    /// The default player with a box's own colors (each optional) on top:
    /// Border colors the frame and strip, Background the screen, Text the
    /// songs and title.
    pub fn with_box_colors(border: Option<Color>, background: Option<Color>, text: Option<Color>) -> Self {
        let mut colors = Self::default();
        if let Some(border) = border {
            colors.frame = border;
            colors.strip = border;
        }
        if let Some(background) = background {
            colors.screen = background;
            if border.is_some() {
                colors.keys = background;
            }
        }
        if let Some(text) = text {
            colors.text = text;
            colors.title = text;
        }
        colors
    }
}

/// A Music box drawn as a little media-player window: a rounded frame, a
/// `◄ title` tab on the top edge, the songs on the screen (the selected one
/// lit in reverse) and a control strip on the bottom row inside the frame.
/// `desktop` shows around the rounded corners; `focus` is the tab's style
/// while the box is selected.
pub fn draw_music_player(
    buf: &mut Buffer,
    outer: Rect,
    colors: &PlayerColors,
    desktop: Color,
    focus: Option<Style>,
    title: &str,
    songs: &[SongLine],
) {
    let outer = outer.intersection(buf.area);
    if outer.width < PLAYER_MIN_WIDTH || outer.height < 4 {
        return;
    }
    let (x0, y0, w, h) = (outer.x, outer.y, outer.width, outer.height);
    let inner_w = (w - 2) as usize;
    let edge = Style::default().fg(colors.frame).bg(desktop);
    let frame = Style::default().bg(colors.frame).fg(colors.screen);
    let screen = Style::default().bg(colors.screen).fg(colors.text);

    // Rounded frame: half blocks on the top and bottom edges, solid down
    // the sides.
    put(buf, outer, x0, y0, &format!("▗{}▖", "▄".repeat(inner_w)), edge);
    put(buf, outer, x0, y0 + h - 1, &format!("▝{}▘", "▀".repeat(inner_w)), edge);
    for y in y0 + 1..y0 + h - 1 {
        put(buf, outer, x0, y, " ", frame);
        put(buf, outer, x0 + 1, y, &" ".repeat(inner_w), screen);
        put(buf, outer, x0 + w - 1, y, " ", frame);
    }

    // The title tab, centered on the top edge.
    let tab_style = focus.unwrap_or_else(|| {
        Style::default()
            .bg(colors.screen)
            .fg(colors.title)
            .add_modifier(Modifier::BOLD)
    });
    let tab = fit(&format!(" ◄ {title} "), inner_w.saturating_sub(2));
    let tab_w = UnicodeWidthStr::width(tab.as_str()) as u16;
    put(buf, outer, x0 + (w - tab_w) / 2, y0, &tab, tab_style);

    // Songs on the screen; the selected one lit in reverse.
    let last_song_row = y0 + h - 3;
    for song in songs {
        if song.y <= y0 || song.y > last_song_row {
            continue;
        }
        let style = if song.selected {
            Style::default().bg(colors.text).fg(colors.screen).add_modifier(Modifier::BOLD)
        } else {
            screen
        };
        let width = (song.width as usize).min(inner_w);
        let name = fit(song.name, width);
        let pad = width - UnicodeWidthStr::width(name.as_str());
        put(buf, outer, song.x, song.y, &format!("{name}{}", " ".repeat(pad)), style);
    }

    // The control strip, a panel set into the frame.
    let strip_y = y0 + h - 2;
    let strip = Style::default().bg(colors.strip).fg(colors.keys).add_modifier(Modifier::BOLD);
    let keys = fit(PLAYER_CONTROLS, inner_w);
    let keys_w = UnicodeWidthStr::width(keys.as_str());
    let left = (inner_w - keys_w) / 2;
    let row = format!("{}{keys}{}", " ".repeat(left), " ".repeat(inner_w - keys_w - left));
    put(buf, outer, x0 + 1, strip_y, &row, strip);
}

/// The clipboard's colors. Its own default is a dark red board, a silver
/// clip and a white sheet; a box's Custom Theme colors replace the matching
/// parts (see [`ClipboardColors::with_box_colors`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardColors {
    pub board: Color,
    pub clip: Color,
    /// The box's name on the clip.
    pub clip_text: Color,
    pub sheet: Color,
    /// Checkboxes, items and ruled lines.
    pub ink: Color,
}

impl Default for ClipboardColors {
    fn default() -> Self {
        Self {
            board: Color::Rgb(0x80, 0x20, 0x20),
            clip: Color::Rgb(0xC0, 0xC0, 0xC0),
            clip_text: Color::Rgb(0x00, 0x00, 0x00),
            sheet: Color::Rgb(0xF4, 0xF4, 0xF4),
            ink: Color::Rgb(0x00, 0x00, 0x00),
        }
    }
}

impl ClipboardColors {
    /// The default clipboard with a box's own colors (each optional) on top:
    /// Border colors the board, Background the sheet, Text the items.
    pub fn with_box_colors(border: Option<Color>, background: Option<Color>, text: Option<Color>) -> Self {
        let mut colors = Self::default();
        if let Some(border) = border {
            colors.board = border;
        }
        if let Some(background) = background {
            colors.sheet = background;
        }
        if let Some(text) = text {
            colors.ink = text;
        }
        colors
    }
}

/// A To-Do box drawn as a clipboard: a board with rounded corners, the clip
/// (with `title` on it) centered on the top edge, and a sheet with one
/// checkbox and ruled line per item, blank lines filling the rest. `desktop`
/// shows around the corners; `focus` is the clip's style while the box is
/// selected.
pub fn draw_clipboard(
    buf: &mut Buffer,
    outer: Rect,
    colors: &ClipboardColors,
    desktop: Color,
    focus: Option<Style>,
    title: &str,
    items: &[(bool, String)],
) {
    let outer = outer.intersection(buf.area);
    if outer.width < CLIPBOARD_MIN_WIDTH || outer.height < 4 {
        return;
    }
    let (x0, y0, w, h) = (outer.x, outer.y, outer.width, outer.height);
    let inner_w = (w - 2) as usize;
    let edge = Style::default().fg(colors.board).bg(desktop);
    let board = Style::default().bg(colors.board);
    let sheet = Style::default().bg(colors.sheet).fg(colors.ink);
    let ruled = sheet.add_modifier(Modifier::UNDERLINED);

    // The board: half blocks round the top and bottom, solid down the sides.
    put(buf, outer, x0, y0, &format!("▗{}▖", "▄".repeat(inner_w)), edge);
    put(buf, outer, x0, y0 + h - 1, &format!("▝{}▘", "▀".repeat(inner_w)), edge);
    for y in y0 + 1..y0 + h - 1 {
        put(buf, outer, x0, y, " ", board);
        put(buf, outer, x0 + 1, y, &" ".repeat(inner_w), sheet);
        put(buf, outer, x0 + w - 1, y, " ", board);
    }

    // The clip, holding the box's name, centered on the top edge.
    let clip_style = focus.unwrap_or_else(|| {
        Style::default()
            .bg(colors.clip)
            .fg(colors.clip_text)
            .add_modifier(Modifier::BOLD)
    });
    let clip = fit(&format!(" ◄ {title} ► "), inner_w.saturating_sub(2));
    let clip_w = UnicodeWidthStr::width(clip.as_str()) as u16;
    put(buf, outer, x0 + (w - clip_w) / 2, y0, &clip, clip_style);

    // The checklist: a margin row under the clip, then one ruled line per
    // row down to the bottom margin.
    // " ☑ " then the ruled line, leaving a margin cell before the board.
    let text_w = inner_w.saturating_sub(4);
    for (i, y) in (y0 + 2..y0 + h - 2).enumerate() {
        let (done, text) = items.get(i).map(|(d, t)| (*d, t.as_str())).unwrap_or((false, ""));
        put(buf, outer, x0 + 1, y, if done { " ☑ " } else { " ☐ " }, sheet);
        let text = fit(text, text_w);
        let pad = text_w - UnicodeWidthStr::width(text.as_str());
        put(buf, outer, x0 + 4, y, &format!("{text}{}", " ".repeat(pad)), ruled);
    }
}

/// The weather panel's colors. Its own default is a gray panel in a pale
/// gold frame with a dark gold title band; a box's Custom Theme colors
/// replace the matching parts (see [`WeatherColors::with_box_colors`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeatherColors {
    pub panel: Color,
    pub frame: Color,
    /// The title band and its text.
    pub band: Color,
    pub band_text: Color,
    /// The place name.
    pub place: Color,
    /// Conditions and wind.
    pub text: Color,
    /// The sun, the condition icon and today's low-high.
    pub sun: Color,
    pub cloud: Color,
    /// The thermometer's glass and mercury.
    pub glass: Color,
    pub mercury: Color,
}

impl Default for WeatherColors {
    fn default() -> Self {
        Self {
            panel: Color::Rgb(0x80, 0x80, 0x80),
            frame: Color::Rgb(0xE8, 0xD0, 0x80),
            band: Color::Rgb(0xB8, 0x96, 0x0C),
            band_text: Color::Rgb(0x00, 0x00, 0x00),
            place: Color::Rgb(0xF0, 0xF0, 0xF0),
            text: Color::Rgb(0x00, 0x00, 0x00),
            sun: Color::Rgb(0xFF, 0xE8, 0x00),
            cloud: Color::Rgb(0xF0, 0xF0, 0xF0),
            glass: Color::Rgb(0xD0, 0xD0, 0xD0),
            mercury: Color::Rgb(0xB0, 0x00, 0x00),
        }
    }
}

impl WeatherColors {
    /// The default panel with a box's own colors (each optional) on top:
    /// Border colors the frame and title band, Background the panel, Text
    /// the conditions and wind.
    pub fn with_box_colors(border: Option<Color>, background: Option<Color>, text: Option<Color>) -> Self {
        let mut colors = Self::default();
        if let Some(border) = border {
            colors.frame = border;
            colors.band = border;
        }
        if let Some(background) = background {
            colors.panel = background;
        }
        if let Some(text) = text {
            colors.text = text;
        }
        colors
    }
}

/// Width of the picture on the right of the weather panel (with its gap).
const WEATHER_ART_W: u16 = 5;

/// A Weather box as a gray panel in a pale gold double frame, the dark gold
/// `◄ title ►` band on top, and the report `lines` (place; icon, temperature
/// and conditions; wind; today's low-high | rain) colored like DeskMate's
/// weather card: the place in white, the icon and the low-high in yellow.
/// A sun-and-cloud sits top right with a thermometer under it. `focus` is
/// the band's style while the box is selected.
///
/// ```text
/// ╔═══ ◄ Weather ► ═══╗
/// ║ Sterling, US   ☁☀ ║
/// ║ ☀ 14°C Clear    │ ║
/// ║ Wind 6km/h SSE  ┃ ║
/// ║ 11→25°C | Rain  ● ║
/// ╚═══════════════════╝
/// ```
pub fn draw_weather_panel(
    buf: &mut Buffer,
    outer: Rect,
    colors: &WeatherColors,
    focus: Option<Style>,
    title: &str,
    lines: &[String],
) {
    let outer = outer.intersection(buf.area);
    if outer.width < WEATHER_MIN_WIDTH || outer.height < 3 {
        return;
    }
    let (x0, y0, w, h) = (outer.x, outer.y, outer.width, outer.height);
    let inner_w = (w - 2) as usize;
    let panel = Style::default().bg(colors.panel).fg(colors.text);
    let frame = Style::default().bg(colors.panel).fg(colors.frame);

    // The double frame round the panel.
    put(buf, outer, x0, y0, &format!("╔{}╗", "═".repeat(inner_w)), frame);
    put(buf, outer, x0, y0 + h - 1, &format!("╚{}╝", "═".repeat(inner_w)), frame);
    for y in y0 + 1..y0 + h - 1 {
        put(buf, outer, x0, y, "║", frame);
        put(buf, outer, x0 + 1, y, &" ".repeat(inner_w), panel);
        put(buf, outer, x0 + w - 1, y, "║", frame);
    }

    // The title band, centered on the top edge.
    let band = focus.unwrap_or_else(|| {
        Style::default()
            .bg(colors.band)
            .fg(colors.band_text)
            .add_modifier(Modifier::BOLD)
    });
    let label = fit(&format!(" ◄ {title} ► "), inner_w.saturating_sub(2));
    let label_w = UnicodeWidthStr::width(label.as_str()) as u16;
    put(buf, outer, x0 + (w - label_w) / 2, y0, &label, band);

    // The report, left of the picture.
    let text_x = x0 + 2;
    let text_w = inner_w.saturating_sub(1 + WEATHER_ART_W as usize);
    let bold = panel.add_modifier(Modifier::BOLD);
    let sun = Style::default().bg(colors.panel).fg(colors.sun).add_modifier(Modifier::BOLD);
    for (i, line) in lines.iter().enumerate() {
        let y = y0 + 1 + i as u16;
        if y >= y0 + h - 1 {
            break;
        }
        let line = fit(line, text_w);
        // Which part of the line is lit: the place (white), the icon before
        // the temperature, or today's low-high before " |" (yellow).
        let place = Style::default().bg(colors.panel).fg(colors.place).add_modifier(Modifier::BOLD);
        let (split, lit_style) = match i {
            0 => (line.len(), place),
            1 => (line.find(' ').unwrap_or(0), sun),
            3 => (line.find(" |").unwrap_or(0), sun),
            _ => (0, bold),
        };
        let (lit, rest) = line.split_at(split);
        put(buf, outer, text_x, y, lit, lit_style);
        put(buf, outer, text_x + UnicodeWidthStr::width(lit) as u16, y, rest, bold);
    }

    // The picture: a sun peeking over a cloud, and a thermometer below.
    let art_x = x0 + w - 1 - WEATHER_ART_W + 1;
    let at = |dx: u16| art_x + dx;
    let cloud = Style::default().bg(colors.panel).fg(colors.cloud).add_modifier(Modifier::BOLD);
    put(buf, outer, at(1), y0 + 1, "☁", cloud);
    put(buf, outer, at(2), y0 + 1, "☀", sun);
    let glass = Style::default().bg(colors.panel).fg(colors.glass);
    let mercury = Style::default().bg(colors.panel).fg(colors.mercury).add_modifier(Modifier::BOLD);
    let bottom = y0 + h - 2;
    for y in y0 + 2..=bottom {
        let (glyph, style) = if y == bottom {
            ("●", mercury)
        } else if y == y0 + 2 {
            ("│", glass)
        } else {
            ("┃", mercury)
        };
        put(buf, outer, at(2), y, glyph, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> StyleColors {
        StyleColors {
            desktop: Color::Blue,
            page: Color::Gray,
            ink: Color::Black,
            accent: Color::Cyan,
            selected: Style::default().bg(Color::Yellow),
        }
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf.get(x, y).symbol().to_string()).collect()
    }

    #[test]
    fn music_player_has_a_title_tab_songs_and_controls() {
        let area = Rect::new(0, 0, 24, 6);
        let mut buf = Buffer::empty(area);
        let songs = [
            SongLine { x: 1, y: 1, width: 22, name: "AguadoOp4No6.mid", selected: true },
            SongLine { x: 1, y: 2, width: 22, name: "bwv851_fugue06.mid", selected: false },
            SongLine { x: 1, y: 3, width: 22, name: "giselle.mid", selected: false },
        ];
        let player = PlayerColors::default();
        draw_music_player(&mut buf, area, &player, Color::Blue, None, "Music", &songs);
        assert!(row(&buf, 0).contains("◄ Music"));
        assert!(row(&buf, 1).contains("AguadoOp4No6.mid"));
        assert!(row(&buf, 3).contains("giselle.mid"));
        assert!(row(&buf, 4).contains(PLAYER_CONTROLS));
        // Its own default colors: cyan songs on navy in a silver frame.
        assert_eq!(buf.get(1, 1).bg, player.text, "selected song is lit");
        assert_eq!(buf.get(1, 2).bg, player.screen);
        assert_eq!(buf.get(1, 2).fg, player.text);
        assert_eq!(buf.get(0, 2).bg, player.frame);
        assert_eq!(buf.get(2, 4).bg, player.strip);
        assert!(row(&buf, 5).starts_with('▝'));
        for y in 0..6 {
            println!("{}", row(&buf, y));
        }
    }

    #[test]
    fn box_colors_replace_the_player_defaults() {
        let default = PlayerColors::default();
        assert_eq!(PlayerColors::with_box_colors(None, None, None), default);
        let custom = PlayerColors::with_box_colors(Some(Color::Red), None, Some(Color::Green));
        assert_eq!((custom.frame, custom.strip), (Color::Red, Color::Red));
        assert_eq!((custom.text, custom.title), (Color::Green, Color::Green));
        assert_eq!(custom.screen, default.screen);
    }

    #[test]
    fn clipboard_has_a_clip_and_a_checklist() {
        let area = Rect::new(0, 0, 20, 8);
        let mut buf = Buffer::empty(area);
        let items = vec![(true, "Buy milk".to_string()), (false, "Call mom".to_string())];
        let colors = ClipboardColors::default();
        draw_clipboard(&mut buf, area, &colors, Color::Blue, None, "To-Do", &items);
        for y in 0..8 {
            println!("{}", row(&buf, y));
        }
        assert!(row(&buf, 0).contains("◄ To-Do ►"));
        assert!(row(&buf, 2).contains("☑ Buy milk"));
        assert!(row(&buf, 3).contains("☐ Call mom"));
        // Blank lines fill the sheet down to its bottom margin.
        assert!(row(&buf, 5).contains('☐'));
        assert!(!row(&buf, 6).contains('☐'));
        assert_eq!(buf.get(0, 3).bg, colors.board);
        assert_eq!(buf.get(5, 3).bg, colors.sheet);
        assert!(buf.get(5, 3).modifier.contains(Modifier::UNDERLINED));
        let custom = ClipboardColors::with_box_colors(Some(Color::Green), None, None);
        assert_eq!((custom.board, custom.sheet), (Color::Green, colors.sheet));
        assert_eq!(BoxStyle::default_for(BoxKind::Todo), BoxStyle::Clipboard);
    }

    #[test]
    fn weather_panel_colors_the_report_and_draws_the_picture() {
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = Buffer::empty(area);
        let lines: Vec<String> = ["Sterling, US", "☀ 14°C Clear sky", "Wind 6km/h SSE", "11→25°C | Rain 10%"]
            .iter()
            .map(|l| l.to_string())
            .collect();
        let colors = WeatherColors::default();
        draw_weather_panel(&mut buf, area, &colors, None, "Weather", &lines);
        for y in 0..6 {
            println!("{}", row(&buf, y));
        }
        assert!(row(&buf, 0).contains("◄ Weather ►"));
        assert!(row(&buf, 0).starts_with('╔'));
        assert!(row(&buf, 1).contains("Sterling, US"));
        assert!(row(&buf, 1).contains("☁☀"));
        assert!(row(&buf, 2).contains("☀ 14°C Clear sky"));
        assert!(row(&buf, 4).contains("11→25°C | Rain"));
        // Place white, icon yellow, conditions dark, low-high yellow.
        assert_eq!(buf.get(2, 1).fg, colors.place);
        assert_eq!(buf.get(2, 2).fg, colors.sun);
        assert_eq!(buf.get(4, 2).fg, colors.text);
        assert_eq!(buf.get(2, 4).fg, colors.sun);
        assert_eq!(buf.get(12, 4).fg, colors.text);
        // Thermometer: glass on top, red mercury and bulb below.
        assert_eq!(buf.get(27, 4).symbol(), "●");
        assert_eq!(buf.get(27, 4).fg, colors.mercury);
        assert_eq!(buf.get(5, 3).bg, colors.panel);
        let custom = WeatherColors::with_box_colors(Some(Color::Red), None, None);
        assert_eq!((custom.frame, custom.band, custom.panel), (Color::Red, Color::Red, colors.panel));
        assert_eq!(BoxStyle::default_for(BoxKind::Weather), BoxStyle::WeatherPanel);
    }

    #[test]
    fn music_boxes_default_to_the_player_look() {
        assert_eq!(BoxStyle::default_for(BoxKind::Music), BoxStyle::MusicPlayer);
        assert_eq!(BoxStyle::MusicPlayer.effective(BoxKind::Music, true, 19), BoxStyle::Classic);
        assert_eq!(BoxStyle::MusicPlayer.effective(BoxKind::Music, false, 30), BoxStyle::Classic);
    }

    #[test]
    fn style_choices_follow_kind() {
        assert_eq!(BoxStyle::Classic.next(BoxKind::Calendar), BoxStyle::SpiralCalendar);
        assert_eq!(BoxStyle::SpiralCalendar.next(BoxKind::Calendar), BoxStyle::Classic);
        assert_eq!(BoxStyle::Classic.previous(BoxKind::Notes), BoxStyle::TodoPad);
        assert_eq!(BoxStyle::Classic.next(BoxKind::Menu), BoxStyle::Classic);
        assert!(!BoxStyle::TodoPad.fits(BoxKind::Calendar));
        assert!(!BoxStyle::SpiralCalendar.fits(BoxKind::Notes));
    }

    #[test]
    fn effective_style_falls_back() {
        let cal = BoxStyle::SpiralCalendar;
        assert_eq!(cal.effective(BoxKind::Calendar, true, 40), cal);
        assert_eq!(cal.effective(BoxKind::Calendar, true, 20), BoxStyle::Classic);
        assert_eq!(cal.effective(BoxKind::Calendar, true, 28), cal);
        assert_eq!(cal.effective(BoxKind::Calendar, false, 40), BoxStyle::Classic);
        assert_eq!(cal.effective(BoxKind::Notes, true, 40), BoxStyle::Classic);
        assert_eq!(BoxStyle::TodoPad.effective(BoxKind::Notes, true, 11), BoxStyle::Classic);
    }

    #[test]
    fn serde_names_and_default() {
        assert_eq!(serde_json::to_string(&BoxStyle::SpiralCalendar).unwrap(), "\"spiral_calendar\"");
        assert_eq!(serde_json::to_string(&BoxStyle::TodoPad).unwrap(), "\"todo_pad\"");
        let parsed: BoxStyle = serde_json::from_str("\"todo_pad\"").unwrap();
        assert_eq!(parsed, BoxStyle::TodoPad);
        assert!(BoxStyle::default().is_classic());
    }

    #[test]
    fn spiral_is_the_calendar_default() {
        assert_eq!(BoxStyle::default_for(BoxKind::Calendar), BoxStyle::SpiralCalendar);
        assert_eq!(BoxStyle::default_for(BoxKind::Notes), BoxStyle::Classic);
        assert_eq!(BoxStyle::default_for(BoxKind::Menu), BoxStyle::Classic);
        for kind in crate::boxes::BOX_KINDS {
            assert!(BoxStyle::default_for(kind).fits(kind), "{kind:?}");
        }
    }

    #[test]
    fn todo_items_parse_checks() {
        let items = todo_items("[x] Call Frank\n[ ] Buy milk\nx Done thing\nPlain\n");
        assert_eq!(
            items,
            vec![
                (true, "Call Frank".to_string()),
                (false, "Buy milk".to_string()),
                (true, "Done thing".to_string()),
                (false, "Plain".to_string()),
            ]
        );
        assert!(todo_items("   ").is_empty());
    }

    #[test]
    fn calendar_draws_rings_banner_and_shadows() {
        // October 2026 starts on a Thursday.
        let weeks = vec![
            [None, None, None, None, Some(1), Some(2), Some(3)],
            [Some(4), Some(5), Some(6), Some(7), Some(8), Some(9), Some(10)],
            [Some(11), Some(12), Some(13), Some(14), Some(15), Some(16), Some(17)],
            [Some(18), Some(19), Some(20), Some(21), Some(22), Some(23), Some(24)],
            [Some(25), Some(26), Some(27), Some(28), Some(29), Some(30), Some(31)],
        ];
        let area = Rect::new(0, 0, 32, 12);
        let mut buf = Buffer::empty(area);
        draw_spiral_calendar(&mut buf, area, &colors(), false, "October 2026", &weeks, Some(10), &[]);
        assert!(row(&buf, 0).contains("╭╮"), "ring tops: {:?}", row(&buf, 0));
        assert!(row(&buf, 1).contains("╰●"), "ring holes: {:?}", row(&buf, 1));
        assert!(row(&buf, 2).contains("OCTOBER 2026"));
        assert!(row(&buf, 3).contains("Su"));
        // Thursday's column starts with 1, shadow begins half a row down.
        assert!(row(&buf, 4).contains(" 1▖"), "{:?}", row(&buf, 4));
        assert!(row(&buf, 5).contains("10▌"), "{:?}", row(&buf, 5));
        // Bottom of every column's "L" sits under the last week.
        assert_eq!(row(&buf, 9).matches("▝▀▘").count(), 7, "{:?}", row(&buf, 9));
        // Today is drawn inverted.
        let r5 = row(&buf, 5);
        let x = r5[..r5.find("10").unwrap()].chars().count() as u16;
        assert_eq!(buf.get(x, 5).bg, Color::Black);
        assert_eq!(buf.get(0, 0).bg, Color::Blue);
        assert_eq!(buf.get(0, 3).bg, Color::Gray);
        assert_eq!(buf.get(5, 2).bg, Color::Cyan);
    }

    #[test]
    fn calendar_fits_narrow_columns() {
        let weeks = vec![
            [None, None, None, None, Some(1), Some(2), Some(3)],
            [Some(4), Some(5), Some(6), Some(7), Some(8), Some(9), Some(10)],
            [Some(25), Some(26), Some(27), Some(28), Some(29), Some(30), Some(31)],
        ];
        for width in [23u16, 26, 28, 29] {
            assert_eq!(
                BoxStyle::SpiralCalendar.effective(BoxKind::Calendar, true, width),
                BoxStyle::SpiralCalendar,
                "width {width}"
            );
            let area = Rect::new(0, 0, width, 9);
            let mut buf = Buffer::empty(area);
            draw_spiral_calendar(&mut buf, area, &colors(), false, "October 2026", &weeks, None, &[]);
            assert!(row(&buf, 2).contains("OCTOBER 2026"), "width {width}");
            assert!(row(&buf, 5).contains("10▌"), "width {width}: {:?}", row(&buf, 5));
            assert_eq!(row(&buf, 7).matches("▝▀▘").count(), 7, "width {width}: {:?}", row(&buf, 7));
            // Nothing spills past the right edge of the page.
            assert_eq!(buf.get(width - 1, 5).symbol(), " ", "width {width}");
        }
        assert_eq!(
            BoxStyle::SpiralCalendar.effective(BoxKind::Calendar, true, 22),
            BoxStyle::Classic
        );
    }

    #[test]
    fn todo_pad_draws_rings_header_and_boxes() {
        let area = Rect::new(0, 0, 20, 7);
        let mut buf = Buffer::empty(area);
        let items = todo_items("[x] Milk\nBread");
        draw_todo_pad(&mut buf, area, &colors(), false, "TO DO:", &items);
        for y in 0..7 {
            assert!(row(&buf, y).starts_with("⊂●"), "row {y}: {:?}", row(&buf, y));
        }
        assert!(row(&buf, 1).contains("TO DO:"));
        assert!(row(&buf, 2).contains("Milk"));
        assert!(row(&buf, 3).contains("Bread"));
        // No checkboxes on the ruled lines.
        for y in 0..7 {
            assert!(
                !row(&buf, y).contains('☐') && !row(&buf, y).contains('☑'),
                "row {y}: {:?}",
                row(&buf, y)
            );
        }
        assert!(buf.get(8, 3).modifier.contains(Modifier::UNDERLINED));
        assert!(buf.get(5, 2).modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn address_book_draws_a_spiral_bound_book() {
        let area = Rect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(area);
        let lines = [
            BookLine { y: 1, name: "Mom", phone: "555-0199", selected: true },
            BookLine { y: 2, name: "dad", phone: "", selected: false },
        ];
        let colors = BookColors::default();
        draw_address_book(&mut buf, area, &colors, Color::Blue, None, "Address Book", &lines);
        for y in 0..10 {
            println!("{}", row(&buf, y));
        }
        assert!(row(&buf, 0).starts_with("◖━◗ Address Book"));
        let first = row(&buf, 1);
        assert!(first.starts_with("◖━◗ Mom"));
        assert!(first.contains("555-0199"));
        assert!(row(&buf, 2).starts_with("◖━◗ dad"));
        // Index tabs down the right, below the title row: 8 rows of page
        // share out the alphabet as AC, DF, ... WZ.
        assert!(row(&buf, 1).ends_with("AC"));
        assert!(row(&buf, 8).ends_with("WZ"));
        // The selected contact is lit in reverse; others are on the page.
        assert_eq!(buf.get(4, 1).bg, colors.ink);
        assert_eq!(buf.get(4, 2).bg, colors.page);
        // The page edge under the page, and no shadow: the tabs reach the
        // box's right edge and nothing is drawn below the edge row.
        assert!(row(&buf, 9).contains(&"▀".repeat(20)));
        assert_eq!(buf.get(29, 6).bg, colors.tab_alt);
        assert_eq!(buf.get(29, 9).bg, Color::Blue);
        assert_eq!(index_tabs(13).join(" "), "AB CD EF GH IJ KL MN OP QR ST UV WX YZ");
        assert_eq!(index_tabs(4), ["AF", "GM", "NS", "TZ"]);
        let custom = BookColors::with_box_colors(None, Some(Color::Yellow), None);
        assert_eq!((custom.page, custom.ink), (Color::Yellow, colors.ink));
        assert_eq!(BoxStyle::default_for(BoxKind::Contacts), BoxStyle::AddressBook);
        assert_eq!(BoxStyle::AddressBook.effective(BoxKind::Contacts, true, 19), BoxStyle::Classic);
    }
}
