//! Calendar with appointments and a To-Do list, modeled on RustyTUICalendar
//! (https://github.com/soumyasen1809/RustyTUICalendar), drawn DeskMate-style:
//! a Sunday-first month grid with today highlighted, the selected day's
//! appointments, a high/low priority To-Do list, and the original's command
//! line (`app, ...`, `todo, ...`, `find, ...`, `today`). The weather panel is
//! intentionally left out.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Duration, Local, Months, NaiveDate, NaiveDateTime, NaiveTime};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use serde::{Deserialize, Serialize};

use crate::dmui::{
    confirm_click, dm_box, fit_width, is_left_click, key, pad_width, put_line, render_confirm, screen_chrome,
    shortcut_click, wheel_key, FieldForm, FormField, FormResult, Hits, Palette,
};
use unicode_width::UnicodeWidthStr;

const DATE_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Appointment {
    /// `YYYY-MM-DD HH:MM:SS`, the same format RustyTUICalendar stores.
    pub date: String,
    pub event_name: String,
    #[serde(default)]
    pub location: String,
}

impl Appointment {
    fn when(&self) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(&self.date, DATE_TIME_FORMAT).ok()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub high_prio: bool,
    pub todo_name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CalendarFile {
    #[serde(default)]
    pub all_events: Vec<Appointment>,
    #[serde(default)]
    pub all_todos: Vec<Todo>,
}

impl CalendarFile {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Indices of the appointments on `day`, sorted by time.
    fn on_day(&self, day: NaiveDate) -> Vec<usize> {
        let mut found: Vec<(NaiveDateTime, usize)> = self
            .all_events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.when().filter(|w| w.date() == day).map(|w| (w, i)))
            .collect();
        found.sort();
        found.into_iter().map(|(_, i)| i).collect()
    }

    fn has_events(&self, day: NaiveDate) -> bool {
        self.all_events.iter().any(|e| e.when().map(|w| w.date()) == Some(day))
    }

    /// To-Do indices, high priority first (the original's grouping).
    fn todo_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.all_todos.len()).filter(|i| self.all_todos[*i].high_prio).collect();
        order.extend((0..self.all_todos.len()).filter(|i| !self.all_todos[*i].high_prio));
        order
    }
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn first_of_month(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

fn days_in_month(day: NaiveDate) -> u32 {
    let first = first_of_month(day);
    let next = first.checked_add_months(Months::new(1)).unwrap_or(first);
    (next - first).num_days() as u32
}

/// Weeks of the month, Sunday first; `None` for blank cells.
fn month_weeks(day: NaiveDate) -> Vec<[Option<u32>; 7]> {
    let offset = first_of_month(day).weekday().num_days_from_sunday() as usize;
    let mut weeks = Vec::new();
    let mut week = [None; 7];
    for d in 1..=days_in_month(day) {
        let cell = offset + d as usize - 1;
        week[cell % 7] = Some(d);
        if cell % 7 == 6 {
            weeks.push(week);
            week = [None; 7];
        }
    }
    if week.iter().any(|c| c.is_some()) {
        weeks.push(week);
    }
    weeks
}

/// This month for the spiral calendar box: banner title ("October 2026"),
/// the week grid, and today's day of the month.
pub fn month_view() -> (String, Vec<[Option<u32>; 7]>, u32) {
    let today = today();
    (
        format!("{} {}", MONTHS[today.month0() as usize], today.year()),
        month_weeks(today),
        today.day(),
    )
}

/// Lines for the desktop box preview: a mini month (today highlighted, as a
/// (start, len) character range) and today's appointments.
pub fn preview_lines(path: &Path) -> Vec<(String, Option<(usize, usize)>)> {
    let data = CalendarFile::load(path);
    let today = today();
    let mut lines = vec![
        (format!("{:^21}", format!("{} {}", MONTHS[today.month0() as usize], today.year())), None),
        ("Su Mo Tu We Th Fr Sa".to_string(), None),
    ];
    for week in month_weeks(today) {
        let mut text = String::new();
        let mut mark = None;
        for (i, cell) in week.iter().enumerate() {
            if i > 0 {
                text.push(' ');
            }
            match cell {
                Some(d) => {
                    if *d == today.day() {
                        mark = Some((text.chars().count(), 2));
                    }
                    text.push_str(&format!("{d:>2}"));
                }
                None => text.push_str("  "),
            }
        }
        lines.push((text, mark));
    }
    let todays = data.on_day(today);
    if !todays.is_empty() {
        lines.push((String::new(), None));
        for i in todays {
            let e = &data.all_events[i];
            let time = e.when().map(|w| w.format("%H:%M").to_string()).unwrap_or_default();
            lines.push((format!("{time} {}", e.event_name), None));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// The Calendar screen
// ---------------------------------------------------------------------------

/// What a mouse click can land on.
#[derive(Clone, Copy)]
enum CalHit {
    MonthBox,
    PrevMonth,
    NextMonth,
    Day(NaiveDate),
    ApptBox,
    Appt(usize),
    TodoBox,
    Todo(usize),
    Command,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Month,
    Appointments,
    Todos,
}

enum FormPurpose {
    Appointment(Option<usize>),
    Todo(Option<usize>),
}

enum Popup {
    Form(FieldForm, FormPurpose),
    ConfirmAppointment(usize),
    ConfirmTodo(usize),
}

pub enum AppResult {
    Continue,
    Close,
}

pub struct CalendarApp {
    pub box_name: String,
    path: PathBuf,
    data: CalendarFile,
    day: NaiveDate,
    focus: Focus,
    appt_index: usize,
    todo_index: usize,
    popup: Option<Popup>,
    /// The F9 command line, when open.
    command: Option<String>,
    status: String,
    hits: Hits<CalHit>,
}

impl CalendarApp {
    pub fn open(box_name: &str, path: PathBuf) -> Self {
        Self {
            box_name: box_name.to_string(),
            data: CalendarFile::load(&path),
            status: format!("Calendar file: {}", path.display()),
            path,
            day: today(),
            focus: Focus::Month,
            appt_index: 0,
            todo_index: 0,
            popup: None,
            command: None,
            hits: Hits::default(),
        }
    }

    fn save(&mut self) {
        if let Err(err) = self.data.save(&self.path) {
            self.status = format!("Could not save {}: {err}", self.path.display());
        }
    }

    fn set_day(&mut self, day: NaiveDate) {
        self.day = day;
        self.appt_index = 0;
    }

    fn shift_months(&mut self, months: i32) {
        let moved = if months >= 0 {
            self.day.checked_add_months(Months::new(months as u32))
        } else {
            self.day.checked_sub_months(Months::new((-months) as u32))
        };
        if let Some(day) = moved {
            self.set_day(day);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        if let Some(popup) = self.popup.take() {
            self.handle_popup_key(popup, key);
            return AppResult::Continue;
        }
        if let Some(mut command) = self.command.take() {
            match key.code {
                KeyCode::Esc | KeyCode::F(9) => {}
                KeyCode::Enter => self.run_command(&command),
                KeyCode::Backspace => {
                    command.pop();
                    self.command = Some(command);
                }
                KeyCode::Delete => self.command = Some(String::new()),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    command.push(c);
                    self.command = Some(command);
                }
                _ => self.command = Some(command),
            }
            return AppResult::Continue;
        }

        // Keys that work in every panel (RustyTUICalendar's F-keys).
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return AppResult::Close,
            KeyCode::F(1) => self.set_day(self.day - Duration::days(1)),
            KeyCode::F(2) => self.set_day(self.day + Duration::days(1)),
            KeyCode::F(3) | KeyCode::PageUp => self.shift_months(-1),
            KeyCode::F(4) | KeyCode::PageDown => self.shift_months(1),
            KeyCode::F(5) | KeyCode::Char('[') => self.shift_months(-12),
            KeyCode::F(6) | KeyCode::Char(']') => self.shift_months(12),
            KeyCode::F(9) | KeyCode::Char(':') => self.command = Some(String::new()),
            KeyCode::Char('t') => {
                self.set_day(today());
                self.status = "Jumped to today".into();
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Month => Focus::Appointments,
                    Focus::Appointments => Focus::Todos,
                    Focus::Todos => Focus::Month,
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Month => Focus::Todos,
                    Focus::Appointments => Focus::Month,
                    Focus::Todos => Focus::Appointments,
                }
            }
            _ => match self.focus {
                Focus::Month => self.handle_month_key(key),
                Focus::Appointments => self.handle_appointments_key(key),
                Focus::Todos => self.handle_todos_key(key),
            },
        }
        AppResult::Continue
    }

    fn handle_month_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.set_day(self.day - Duration::days(1)),
            KeyCode::Right => self.set_day(self.day + Duration::days(1)),
            KeyCode::Up => self.set_day(self.day - Duration::days(7)),
            KeyCode::Down => self.set_day(self.day + Duration::days(7)),
            KeyCode::Home => self.set_day(first_of_month(self.day)),
            KeyCode::End => {
                if let Some(day) = self.day.with_day(days_in_month(self.day)) {
                    self.set_day(day);
                }
            }
            KeyCode::Enter => self.focus = Focus::Appointments,
            KeyCode::Char('a') => self.open_appointment_form(None),
            _ => {}
        }
    }

    fn handle_appointments_key(&mut self, key: KeyEvent) {
        let todays = self.data.on_day(self.day);
        match key.code {
            KeyCode::Up => self.appt_index = self.appt_index.saturating_sub(1),
            KeyCode::Down => self.appt_index = (self.appt_index + 1).min(todays.len().saturating_sub(1)),
            KeyCode::Left => self.set_day(self.day - Duration::days(1)),
            KeyCode::Right => self.set_day(self.day + Duration::days(1)),
            KeyCode::Char('a') => self.open_appointment_form(None),
            KeyCode::Enter | KeyCode::Char('e') => match todays.get(self.appt_index) {
                Some(i) => self.open_appointment_form(Some(*i)),
                None => self.open_appointment_form(None),
            },
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(i) = todays.get(self.appt_index) {
                    self.popup = Some(Popup::ConfirmAppointment(*i));
                }
            }
            _ => {}
        }
    }

    fn handle_todos_key(&mut self, key: KeyEvent) {
        let order = self.data.todo_order();
        match key.code {
            KeyCode::Up => self.todo_index = self.todo_index.saturating_sub(1),
            KeyCode::Down => self.todo_index = (self.todo_index + 1).min(order.len().saturating_sub(1)),
            KeyCode::Char('a') => self.open_todo_form(None),
            KeyCode::Enter | KeyCode::Char('e') => match order.get(self.todo_index) {
                Some(i) => self.open_todo_form(Some(*i)),
                None => self.open_todo_form(None),
            },
            KeyCode::Char('p') | KeyCode::Char(' ') => {
                if let Some(i) = order.get(self.todo_index) {
                    let todo = &mut self.data.all_todos[*i];
                    todo.high_prio = !todo.high_prio;
                    let i = *i;
                    self.todo_index = self.data.todo_order().iter().position(|x| *x == i).unwrap_or(0);
                    self.save();
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(i) = order.get(self.todo_index) {
                    self.popup = Some(Popup::ConfirmTodo(*i));
                }
            }
            _ => {}
        }
    }

    fn open_appointment_form(&mut self, index: Option<usize>) {
        let existing = index.and_then(|i| self.data.all_events.get(i)).cloned();
        let (date, time, name, location) = match existing.as_ref().and_then(|e| e.when().map(|w| (e, w))) {
            Some((e, when)) => (
                when.format("%Y-%m-%d").to_string(),
                when.format("%H:%M").to_string(),
                e.event_name.clone(),
                e.location.clone(),
            ),
            None => (self.day.format("%Y-%m-%d").to_string(), "09:00".to_string(), String::new(), String::new()),
        };
        let mut form = FieldForm::new(
            if index.is_some() { "Edit Appointment" } else { "Add Appointment" },
            vec![
                FormField::text("Title", &name),
                FormField::text("Date (YYYY-MM-DD)", &date),
                FormField::text("Time (HH:MM)", &time),
                FormField::text("Location", &location),
            ],
        );
        form.selected = 0;
        self.popup = Some(Popup::Form(form, FormPurpose::Appointment(index)));
    }

    fn open_todo_form(&mut self, index: Option<usize>) {
        let existing = index.and_then(|i| self.data.all_todos.get(i)).cloned();
        let (name, prio) = existing.map(|t| (t.todo_name, t.high_prio)).unwrap_or_default();
        self.popup = Some(Popup::Form(
            FieldForm::new(
                if index.is_some() { "Edit To-Do" } else { "Add To-Do" },
                vec![
                    FormField::text("To-Do", &name),
                    FormField::choice("Priority", if prio { "High" } else { "Low" }, &["Low", "High"]),
                ],
            ),
            FormPurpose::Todo(index),
        ));
    }

    fn handle_popup_key(&mut self, popup: Popup, key: KeyEvent) {
        let yes = matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter);
        let no = matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc);
        match popup {
            Popup::ConfirmAppointment(i) if yes => {
                if i < self.data.all_events.len() {
                    let removed = self.data.all_events.remove(i);
                    self.status = format!("Deleted appointment \"{}\"", removed.event_name);
                    self.appt_index = self.appt_index.saturating_sub(1);
                    self.save();
                }
            }
            Popup::ConfirmTodo(i) if yes => {
                if i < self.data.all_todos.len() {
                    let removed = self.data.all_todos.remove(i);
                    self.status = format!("Deleted to-do \"{}\"", removed.todo_name);
                    self.todo_index = self.todo_index.saturating_sub(1);
                    self.save();
                }
            }
            Popup::ConfirmAppointment(_) | Popup::ConfirmTodo(_) if no => {}
            Popup::ConfirmAppointment(_) | Popup::ConfirmTodo(_) => self.popup = Some(popup),
            Popup::Form(mut form, purpose) => match form.handle_key(key) {
                FormResult::Continue => self.popup = Some(Popup::Form(form, purpose)),
                FormResult::Cancel => {}
                FormResult::Submit => {
                    if let Err(err) = self.submit(&form, &purpose) {
                        form.error = Some(err);
                        self.popup = Some(Popup::Form(form, purpose));
                    }
                }
            },
        }
    }

    fn submit(&mut self, form: &FieldForm, purpose: &FormPurpose) -> Result<(), String> {
        match purpose {
            FormPurpose::Appointment(index) => {
                let name = form.value(0);
                if name.is_empty() {
                    return Err("Enter a title".into());
                }
                let date = NaiveDate::parse_from_str(form.value(1), "%Y-%m-%d")
                    .map_err(|_| "Date must be YYYY-MM-DD".to_string())?;
                let time = parse_time(form.value(2)).ok_or("Time must be HH:MM")?;
                let appointment = Appointment {
                    date: date.and_time(time).format(DATE_TIME_FORMAT).to_string(),
                    event_name: name.to_string(),
                    location: form.value(3).to_string(),
                };
                match index {
                    Some(i) if *i < self.data.all_events.len() => self.data.all_events[*i] = appointment,
                    _ => self.data.all_events.push(appointment),
                }
                self.set_day(date);
                self.focus = Focus::Appointments;
                self.status = format!("Saved appointment \"{name}\"");
            }
            FormPurpose::Todo(index) => {
                let name = form.value(0);
                if name.is_empty() {
                    return Err("Enter a to-do".into());
                }
                let todo = Todo {
                    high_prio: form.value(1) == "High",
                    todo_name: name.to_string(),
                };
                match index {
                    Some(i) if *i < self.data.all_todos.len() => self.data.all_todos[*i] = todo,
                    _ => self.data.all_todos.push(todo),
                }
                self.status = format!("Saved to-do \"{name}\"");
            }
        }
        self.save();
        Ok(())
    }

    /// The original app's command line:
    ///   `app, 2024-09-14 13:14:50, Title, Location`
    ///   `todo, true, Title`
    ///   `find, 2024-09-14`
    ///   `today`
    fn run_command(&mut self, command: &str) {
        let parts: Vec<&str> = command.split(',').map(|p| p.trim()).collect();
        match parts.first().map(|p| p.to_ascii_lowercase()).as_deref() {
            Some("today") => {
                self.set_day(today());
                self.status = "Jumped to today".into();
            }
            Some("find") | Some("search") => match parts.get(1).and_then(|d| parse_date(d)) {
                Some(day) => {
                    self.set_day(day);
                    self.focus = Focus::Appointments;
                    let count = self.data.on_day(day).len();
                    self.status = format!("{count} appointment(s) on {}", day.format("%Y-%m-%d"));
                }
                None => self.status = "Usage: find, YYYY-MM-DD".into(),
            },
            Some("app") if parts.len() >= 3 => {
                let when = NaiveDateTime::parse_from_str(parts[1], DATE_TIME_FORMAT)
                    .ok()
                    .or_else(|| NaiveDateTime::parse_from_str(parts[1], "%Y-%m-%d %H:%M").ok())
                    .or_else(|| parse_date(parts[1]).map(|d| d.and_time(NaiveTime::MIN)));
                match when {
                    Some(when) => {
                        self.data.all_events.push(Appointment {
                            date: when.format(DATE_TIME_FORMAT).to_string(),
                            event_name: parts[2].to_string(),
                            location: parts[3..].join(", "),
                        });
                        self.set_day(when.date());
                        self.save();
                        self.status = format!("Added appointment \"{}\"", parts[2]);
                    }
                    None => self.status = "Date must be YYYY-MM-DD HH:MM:SS".into(),
                }
            }
            Some("todo") if parts.len() >= 3 => {
                self.data.all_todos.push(Todo {
                    high_prio: parts[1].eq_ignore_ascii_case("true") || parts[1].eq_ignore_ascii_case("high"),
                    todo_name: parts[2..].join(", "),
                });
                self.save();
                self.status = format!("Added to-do \"{}\"", parts[2..].join(", "));
            }
            _ => {
                self.status =
                    "Commands: app, DATE TIME, Title, Location | todo, true/false, Title | find, DATE | today".into()
            }
        }
    }

    // ----- drawing -----

    fn confirm_message(&self) -> Option<String> {
        match &self.popup {
            Some(Popup::ConfirmAppointment(i)) => {
                let name = self.data.all_events.get(*i).map(|e| e.event_name.clone()).unwrap_or_default();
                Some(format!("Delete appointment \"{name}\"?"))
            }
            Some(Popup::ConfirmTodo(i)) => {
                let name = self.data.all_todos.get(*i).map(|t| t.todo_name.clone()).unwrap_or_default();
                Some(format!("Delete to-do \"{name}\"?"))
            }
            _ => None,
        }
    }

    /// Mouse: click a day, an appointment or a to-do (click the selected
    /// one again to edit it), ◄ ► on the month banner change month, the
    /// command box opens F9, the shortcut bar is clickable, the wheel
    /// moves the selection; dialogs get clickable fields and buttons.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> AppResult {
        if let Some(Popup::Form(form, _)) = &mut self.popup {
            return match form.handle_mouse(mouse, area) {
                FormResult::Submit => self.handle_key(key(KeyCode::Enter)),
                FormResult::Cancel => self.handle_key(key(KeyCode::Esc)),
                FormResult::Continue => AppResult::Continue,
            };
        }
        if let Some(message) = self.confirm_message() {
            if let Some(k) = confirm_click(area, &message, mouse) {
                return self.handle_key(k);
            }
            return AppResult::Continue;
        }
        if self.command.is_some() {
            if is_left_click(mouse) && !matches!(self.hits.at(mouse.column, mouse.row), Some(CalHit::Command)) {
                self.command = None;
            }
            return AppResult::Continue;
        }
        if let Some(k) = wheel_key(mouse).or_else(|| shortcut_click(area, self.shortcuts(), mouse)) {
            return self.handle_key(k);
        }
        if !is_left_click(mouse) {
            return AppResult::Continue;
        }
        match self.hits.at(mouse.column, mouse.row) {
            Some(CalHit::MonthBox) => self.focus = Focus::Month,
            Some(CalHit::PrevMonth) => self.shift_months(-1),
            Some(CalHit::NextMonth) => self.shift_months(1),
            Some(CalHit::Day(date)) => {
                if self.focus == Focus::Month && date == self.day {
                    self.focus = Focus::Appointments;
                } else {
                    self.focus = Focus::Month;
                    self.set_day(date);
                }
            }
            Some(CalHit::ApptBox) => self.focus = Focus::Appointments,
            Some(CalHit::Appt(pos)) => {
                if self.focus == Focus::Appointments && pos == self.appt_index {
                    return self.handle_key(key(KeyCode::Enter));
                }
                self.focus = Focus::Appointments;
                self.appt_index = pos;
            }
            Some(CalHit::TodoBox) => self.focus = Focus::Todos,
            Some(CalHit::Todo(pos)) => {
                if self.focus == Focus::Todos && pos == self.todo_index {
                    return self.handle_key(key(KeyCode::Enter));
                }
                self.focus = Focus::Todos;
                self.todo_index = pos;
            }
            Some(CalHit::Command) => self.command = Some(String::new()),
            None => {}
        }
        AppResult::Continue
    }

    fn shortcuts(&self) -> &'static [(&'static str, &'static str)] {
        match self.focus {
            Focus::Month => &[
                ("Esc", "Back"),
                ("Arrows", "Day"),
                ("PgUp/PgDn", "Month"),
                ("[ ]", "Year"),
                ("t", "Today"),
                ("a", "Add"),
                ("Tab", "Panel"),
                ("F9", "Command"),
            ],
            Focus::Appointments => &[
                ("Esc", "Back"),
                ("↑/↓", "Select"),
                ("←/→", "Day"),
                ("a", "Add"),
                ("Enter", "Edit"),
                ("d", "Delete"),
                ("Tab", "Panel"),
                ("F9", "Command"),
            ],
            Focus::Todos => &[
                ("Esc", "Back"),
                ("↑/↓", "Select"),
                ("a", "Add"),
                ("Enter", "Edit"),
                ("p", "Priority"),
                ("d", "Delete"),
                ("Tab", "Panel"),
                ("F9", "Command"),
            ],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, title: &str, pal: &Palette) {
        self.hits.clear();
        let shortcuts = self.shortcuts();
        let desk = screen_chrome(frame, area, &format!("{title} - {}", self.box_name), shortcuts, &self.status, pal);
        if desk.width < 30 || desk.height < 10 {
            return;
        }
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Length(1), Constraint::Min(10)])
            .split(desk);
        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(17), Constraint::Length(1), Constraint::Min(3)])
            .split(columns[0]);
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1), Constraint::Length(3)])
            .split(columns[2]);

        self.render_month(frame, left[0], pal);
        self.render_appointments(frame, left[2], pal);
        self.render_todos(frame, right[0], pal);
        self.render_command(frame, right[2], pal);

        match &self.popup {
            Some(Popup::Form(form, _)) => form.render(frame, area, pal),
            Some(Popup::ConfirmAppointment(i)) => {
                let name = self.data.all_events.get(*i).map(|e| e.event_name.clone()).unwrap_or_default();
                render_confirm(frame, area, &format!("Delete appointment \"{name}\"?"), pal);
            }
            Some(Popup::ConfirmTodo(i)) => {
                let name = self.data.all_todos.get(*i).map(|t| t.todo_name.clone()).unwrap_or_default();
                render_confirm(frame, area, &format!("Delete to-do \"{name}\"?"), pal);
            }
            None => {}
        }
    }

    fn render_month(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let title = format!("{} {}", MONTHS[self.day.month0() as usize], self.day.year());
        let inner = dm_box(frame, area, &title, pal, pal.fill(), self.focus == Focus::Month);
        self.hits.push(area, CalHit::MonthBox);
        // dm_box centers " ◄ TITLE ► " on the top border: make ◄ / ► clickable.
        if area.width > 6 {
            let band = fit_width(&format!(" ◄ {title} ► "), area.width as usize - 4);
            let band_width = UnicodeWidthStr::width(band.as_str()) as u16;
            if band.ends_with("► ") {
                let x = area.x + (area.width - band_width) / 2;
                self.hits.push(Rect { x, y: area.y, width: 3, height: 1 }, CalHit::PrevMonth);
                self.hits.push(Rect { x: x + band_width - 3, y: area.y, width: 3, height: 1 }, CalHit::NextMonth);
            }
        }
        if inner.width < 21 {
            return;
        }
        // Cells are as wide as the box allows, like the original's spread-out grid.
        let cell = (inner.width / 7).clamp(3, 8);
        let left = inner.x + (inner.width - cell * 7) / 2;
        let today = today();
        let header_style = pal.label();
        for (i, name) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
            frame.render_widget(
                Paragraph::new(format!("{name:>w$}", w = (cell - 1) as usize)).style(header_style),
                Rect { x: left + i as u16 * cell, y: inner.y + 1, width: cell, height: 1 },
            );
        }
        let rule = "─".repeat((cell * 7) as usize);
        frame.render_widget(
            Paragraph::new(rule).style(Style::default().fg(pal.accent).bg(pal.surface)),
            Rect { x: left, y: inner.y + 2, width: cell * 7, height: 1 },
        );
        for (row, week) in month_weeks(self.day).iter().enumerate() {
            let y = inner.y + 4 + row as u16 * 2;
            if y >= inner.y + inner.height {
                break;
            }
            for (col, cell_day) in week.iter().enumerate() {
                let Some(d) = cell_day else { continue };
                let date = self.day.with_day(*d).unwrap_or(self.day);
                let mut style = pal.fill();
                if date == today {
                    style = style.fg(pal.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                }
                if date == self.day {
                    style = pal.selected();
                }
                let marker = if self.data.has_events(date) { "•" } else { " " };
                self.hits.push(Rect { x: left + col as u16 * cell, y, width: cell, height: 1 }, CalHit::Day(date));
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("{d:>w$}", w = (cell - 1) as usize), style),
                        Span::styled(marker, Style::default().fg(pal.accent).bg(pal.surface)),
                    ])),
                    Rect { x: left + col as u16 * cell, y, width: cell, height: 1 },
                );
            }
        }
    }

    fn render_appointments(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let title = format!("APPOINTMENTS - {}", self.day.format("%a %-d %B %Y"));
        let focused = self.focus == Focus::Appointments;
        let inner = dm_box(frame, area, &title, pal, pal.fill(), focused);
        self.hits.push(area, CalHit::ApptBox);
        let todays = self.data.on_day(self.day);
        if todays.is_empty() {
            put_line(frame, inner, 0, " (no appointments - press a to add)", pal.fill().add_modifier(Modifier::ITALIC));
            return;
        }
        let mut row = 0u16;
        for (pos, i) in todays.iter().enumerate() {
            let e = &self.data.all_events[*i];
            let time = e.when().map(|w| w.format("%H:%M").to_string()).unwrap_or_default();
            let selected = focused && pos == self.appt_index;
            let style = if selected { pal.selected() } else { pal.fill() };
            let rows = if e.location.is_empty() { 1 } else { 2 };
            self.hits.push(
                Rect { x: inner.x, y: inner.y + row, width: inner.width, height: rows.min(inner.height.saturating_sub(row)) },
                CalHit::Appt(pos),
            );
            put_line(frame, inner, row, &format!(" {time}  {}", e.event_name), style.add_modifier(Modifier::BOLD));
            row += 1;
            if !e.location.is_empty() {
                put_line(frame, inner, row, &format!("        @ {}", e.location), style);
                row += 1;
            }
            if row >= inner.height {
                break;
            }
        }
    }

    fn render_todos(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let focused = self.focus == Focus::Todos;
        let inner = dm_box(frame, area, "TO-DO", pal, pal.fill(), focused);
        self.hits.push(area, CalHit::TodoBox);
        let order = self.data.todo_order();
        let high = order.iter().filter(|i| self.data.all_todos[**i].high_prio).count();
        let mut row = 0u16;
        let heading = |frame: &mut Frame, row: &mut u16, text: String| {
            put_line(frame, inner, *row, &format!(" {text}"), pal.label());
            *row += 1;
        };
        heading(frame, &mut row, format!("High priority: {high}"));
        for (pos, i) in order.iter().enumerate() {
            if pos == high {
                row += 1;
                heading(frame, &mut row, format!("Low priority: {}", order.len() - high));
            }
            if row >= inner.height {
                break;
            }
            let style = if focused && pos == self.todo_index { pal.selected() } else { pal.fill() };
            let number = if pos < high { pos } else { pos - high };
            let text = format!("  {number}. {}", self.data.all_todos[*i].todo_name);
            self.hits.push(Rect { x: inner.x, y: inner.y + row, width: inner.width, height: 1 }, CalHit::Todo(pos));
            put_line(frame, inner, row, &fit_width(&text, inner.width as usize), style);
            row += 1;
        }
        if high == order.len() && row < inner.height {
            row += 1;
            heading(frame, &mut row, "Low priority: 0".into());
        }
    }

    fn render_command(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let active = self.command.is_some();
        let inner = dm_box(frame, area, "COMMAND (F9)", pal, pal.fill(), active);
        self.hits.push(area, CalHit::Command);
        match &self.command {
            Some(text) => {
                let width = inner.width.saturating_sub(2) as usize;
                let chars: Vec<char> = text.chars().collect();
                let shown: String = chars[chars.len().saturating_sub(width.saturating_sub(1))..].iter().collect();
                put_line(frame, inner, 0, &format!("> {}", pad_width(&shown, width)), pal.fill());
                frame.set_cursor(inner.x + 2 + shown.chars().count() as u16, inner.y);
            }
            None => put_line(
                frame,
                inner,
                0,
                " F9: app, 2026-10-06 14:00:00, Title, Place | todo, true, Title | find, DATE | today",
                pal.fill().add_modifier(Modifier::ITALIC),
            ),
        }
    }
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

fn parse_time(text: &str) -> Option<NaiveTime> {
    let text = text.trim();
    if text.is_empty() {
        return Some(NaiveTime::MIN);
    }
    NaiveTime::parse_from_str(text, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M:%S"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_grid_is_sunday_first() {
        // September 2024 starts on a Sunday and has 30 days.
        let weeks = month_weeks(NaiveDate::from_ymd_opt(2024, 9, 12).unwrap());
        assert_eq!(weeks[0][0], Some(1));
        assert_eq!(weeks.len(), 5);
        assert_eq!(weeks[4][1], Some(30));
        // October 2026 starts on a Thursday.
        let weeks = month_weeks(NaiveDate::from_ymd_opt(2026, 10, 6).unwrap());
        assert_eq!(weeks[0][4], Some(1));
        assert_eq!(weeks[0][3], None);
    }

    #[test]
    fn commands_add_and_find() {
        let dir = std::env::temp_dir().join(format!("mm-cal-test-{}", std::process::id()));
        let path = dir.join("calendar.json");
        let mut app = CalendarApp::open("Cal", path.clone());
        app.run_command("app, 2024-09-14 13:14:50, Design Meeting, Lounge C");
        app.run_command("todo, true, Finish car repair");
        app.run_command("todo, false, Buy groceries");
        app.run_command("find, 2024-09-14");
        assert_eq!(app.day, NaiveDate::from_ymd_opt(2024, 9, 14).unwrap());
        assert_eq!(app.data.on_day(app.day).len(), 1);
        let saved = CalendarFile::load(&path);
        assert_eq!(saved.all_events[0].location, "Lounge C");
        assert_eq!(saved.all_todos.len(), 2);
        assert!(saved.all_todos[0].high_prio);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_original_appointments_format() {
        let json = r#"{"current_date":"2024-09-06 00:00:00","all_events":[{"date":"2024-09-05 09:00:00","event_name":"Intro","location":"Zoom"}]}"#;
        let data: CalendarFile = serde_json::from_str(json).unwrap();
        assert_eq!(data.on_day(NaiveDate::from_ymd_opt(2024, 9, 5).unwrap()), vec![0]);
    }
}
