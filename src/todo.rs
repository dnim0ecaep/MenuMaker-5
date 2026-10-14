//! To-Do box: a checkable task list in the spirit of todui
//! (https://github.com/danimelchor/todui). Each item has a checkable box
//! `[ ]` / `[x]`, an optional priority (high/med/low) and a due date. Items
//! persist as JSON in the box's data path. A DeskMate-style full screen with
//! add (`a`), delete (`d`), toggle (Space / `x`) and an all/open/done filter.

use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::Frame;
use serde::{Deserialize, Serialize};

use crate::boxes::expand_tilde;
use crate::dmui::{
    is_left_click, put_line, screen_chrome, shortcut_click, wheel_key, FieldForm, FormField,
    FormResult, Hits, Palette,
};

const SHORTCUTS: &[(&str, &str)] = &[
    ("Esc", "Back"),
    ("↑/↓", "Move"),
    ("Space", "Toggle"),
    ("a", "Add"),
    ("d", "Delete"),
    ("f", "Filter"),
];

/// Priority of a to-do item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    #[default]
    Med,
    Low,
}

impl Priority {
    fn label(self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Med => "med",
            Priority::Low => "low",
        }
    }

    fn from_label(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Priority::High,
            "low" => Priority::Low,
            _ => Priority::Med,
        }
    }

    /// Sort key: high first.
    fn rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Med => 1,
            Priority::Low => 2,
        }
    }
}

/// One to-do item.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub priority: Priority,
    /// Optional due date, free text (e.g. `2026-10-12`). Empty = none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub due: String,
}

impl TodoItem {
    /// How a row is drawn: `[x] text   (high, 2026-10-12)`.
    fn render_text(&self) -> String {
        let check = if self.done { "[x]" } else { "[ ]" };
        let mut meta = Vec::new();
        if self.priority != Priority::Med {
            meta.push(self.priority.label().to_string());
        }
        if !self.due.is_empty() {
            meta.push(self.due.clone());
        }
        if meta.is_empty() {
            format!("  {check} {}", self.text)
        } else {
            format!("  {check} {}   ({})", self.text, meta.join(", "))
        }
    }
}

/// The persisted list of items.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub items: Vec<TodoItem>,
}

impl Store {
    fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(&text) {
            Ok(store) => store,
            Err(_) => {
                // Unreadable: set it aside rather than overwrite it on the
                // next save.
                let _ = fs::rename(path, path.with_extension("json.bad"));
                Self::default()
            }
        }
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }
}

/// Which items the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Open,
    Done,
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Open => "open",
            Filter::Done => "done",
        }
    }

    fn next(self) -> Self {
        match self {
            Filter::All => Filter::Open,
            Filter::Open => Filter::Done,
            Filter::Done => Filter::All,
        }
    }

    fn keeps(self, item: &TodoItem) -> bool {
        match self {
            Filter::All => true,
            Filter::Open => !item.done,
            Filter::Done => item.done,
        }
    }
}

pub enum AppResult {
    Continue,
    Close,
}

/// A row the mouse can hit.
#[derive(Clone, Copy)]
enum Hit {
    /// Select the item at this store index.
    Row(usize),
    /// Toggle done on the item at this store index.
    Check(usize),
}

pub struct TodoApp {
    box_name: String,
    path: PathBuf,
    store: Store,
    /// Selected store index (not filtered position).
    sel: usize,
    filter: Filter,
    /// Active add dialog, if any.
    adding: Option<FieldForm>,
    status: String,
    scroll: usize,
    hits: Hits<Hit>,
    /// Inner list area at the last render, for click math.
    list_area: std::cell::Cell<Rect>,
}

impl TodoApp {
    pub fn open(box_name: &str, path: PathBuf) -> Self {
        let path = if path.to_string_lossy().trim().is_empty() {
            path
        } else {
            expand_tilde(&path.to_string_lossy())
        };
        let store = Store::load(&path);
        let count = store.items.len();
        Self {
            box_name: box_name.to_string(),
            path,
            store,
            sel: 0,
            filter: Filter::default(),
            adding: None,
            status: if count == 0 {
                "No to-dos yet. Press a to add one.".into()
            } else {
                format!("{count} to-do(s).")
            },
            scroll: 0,
            hits: Hits::default(),
            list_area: std::cell::Cell::new(Rect::default()),
        }
    }

    /// Store indices that pass the current filter, in display order
    /// (undone first, then by priority).
    fn visible(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = self
            .store
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| self.filter.keeps(it))
            .map(|(i, _)| i)
            .collect();
        idx.sort_by_key(|&i| {
            let it = &self.store.items[i];
            (it.done, it.priority.rank(), i)
        });
        idx
    }

    fn clamp_sel(&mut self) {
        let vis = self.visible();
        if vis.is_empty() {
            self.sel = 0;
            return;
        }
        if !vis.contains(&self.sel) {
            self.sel = vis[0];
        }
    }

    fn move_sel(&mut self, down: bool) {
        let vis = self.visible();
        if vis.is_empty() {
            return;
        }
        let pos = vis.iter().position(|&i| i == self.sel).unwrap_or(0);
        let next = if down {
            (pos + 1).min(vis.len() - 1)
        } else {
            pos.saturating_sub(1)
        };
        self.sel = vis[next];
    }

    fn save(&mut self) {
        match self.store.save(&self.path) {
            Ok(()) => {}
            Err(err) => self.status = format!("Save failed: {err}"),
        }
    }

    fn start_add(&mut self) {
        self.adding = Some(FieldForm::new(
            "New To-Do",
            vec![
                FormField::text("Task", ""),
                FormField::choice("Priority", "med", &["high", "med", "low"]),
                FormField::text("Due (optional)", ""),
            ],
        ));
    }

    fn finish_add(&mut self) {
        let Some(form) = self.adding.as_ref() else {
            return;
        };
        let text = form.value(0).trim().to_string();
        if text.is_empty() {
            if let Some(form) = self.adding.as_mut() {
                form.error = Some("Task text is required".into());
            }
            return;
        }
        let item = TodoItem {
            text,
            done: false,
            priority: Priority::from_label(form.value(1)),
            due: form.value(2).trim().to_string(),
        };
        self.store.items.push(item);
        self.sel = self.store.items.len() - 1;
        self.adding = None;
        self.save();
        self.status = format!("{} to-do(s).", self.store.items.len());
        self.clamp_sel();
    }

    fn toggle(&mut self, index: usize) {
        if let Some(it) = self.store.items.get_mut(index) {
            it.done = !it.done;
            let done = it.done;
            self.save();
            self.status = if done { "Marked done." } else { "Marked open." }.into();
        }
        self.clamp_sel();
    }

    fn delete_selected(&mut self) {
        if self.store.items.is_empty() {
            return;
        }
        if self.sel < self.store.items.len() {
            self.store.items.remove(self.sel);
            self.save();
            self.status = format!("Deleted. {} left.", self.store.items.len());
        }
        self.clamp_sel();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        if self.adding.is_some() {
            let result = self.adding.as_mut().unwrap().handle_key(key);
            match result {
                FormResult::Continue => {}
                FormResult::Cancel => {
                    self.adding = None;
                    self.status = "Add cancelled.".into();
                }
                FormResult::Submit => self.finish_add(),
            }
            return AppResult::Continue;
        }
        match key.code {
            KeyCode::Esc => return AppResult::Close,
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(true),
            KeyCode::Char(' ') | KeyCode::Char('x') | KeyCode::Enter => {
                let sel = self.sel;
                self.toggle(sel);
            }
            KeyCode::Char('a') => self.start_add(),
            KeyCode::Char('d') => self.delete_selected(),
            KeyCode::Char('f') => {
                self.filter = self.filter.next();
                self.status = format!("Filter: {}", self.filter.label());
                self.clamp_sel();
            }
            _ => {}
        }
        AppResult::Continue
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> AppResult {
        if let Some(form) = self.adding.as_mut() {
            match form.handle_mouse(mouse, area) {
                FormResult::Continue => {}
                FormResult::Cancel => self.adding = None,
                FormResult::Submit => self.finish_add(),
            }
            return AppResult::Continue;
        }
        if let Some(key) = shortcut_click(area, SHORTCUTS, mouse) {
            return self.handle_key(key);
        }
        if let Some(key) = wheel_key(mouse) {
            self.handle_key(key);
            return AppResult::Continue;
        }
        if is_left_click(mouse) {
            if let Some(hit) = self.hits.at(mouse.column, mouse.row) {
                match hit {
                    Hit::Check(i) => self.toggle(i),
                    Hit::Row(i) => {
                        self.sel = i;
                        self.status = "Selected.".into();
                    }
                }
            }
        }
        AppResult::Continue
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _title: &str, pal: &Palette) {
        self.hits.clear();
        let title = format!("To-Do — {}", self.box_name);
        let status = format!("{}   [filter: {}]", self.status, self.filter.label());
        let inner = screen_chrome(frame, area, &title, SHORTCUTS, &status, pal);
        self.list_area.set(inner);
        frame.render_widget(
            ratatui::widgets::Block::default().style(pal.fill()),
            inner,
        );

        let vis = self.visible();
        if vis.is_empty() {
            put_line(
                frame,
                inner,
                0,
                "  (no items — press a to add)",
                pal.fill(),
            );
        }

        let height = inner.height as usize;
        // Keep the selected row in view.
        let sel_pos = vis.iter().position(|&i| i == self.sel).unwrap_or(0);
        let mut scroll = self.scroll;
        if sel_pos < scroll {
            scroll = sel_pos;
        } else if height > 0 && sel_pos >= scroll + height {
            scroll = sel_pos + 1 - height;
        }

        for (row, &store_idx) in vis.iter().skip(scroll).take(height).enumerate() {
            let item = &self.store.items[store_idx];
            let selected = store_idx == self.sel;
            let mut style = if selected { pal.selected() } else { pal.fill() };
            if item.done {
                style = style.add_modifier(Modifier::DIM | Modifier::CROSSED_OUT);
            }
            put_line(frame, inner, row as u16, &item.render_text(), style);
            let y = inner.y + row as u16;
            // Whole row selects; the checkbox cell (indented 2) toggles.
            self.hits.push(
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                },
                Hit::Row(store_idx),
            );
            self.hits.push(
                Rect {
                    x: inner.x + 2,
                    y,
                    width: 3,
                    height: 1,
                },
                Hit::Check(store_idx),
            );
        }

        if let Some(form) = self.adding.as_ref() {
            form.render(frame, area, pal);
        }
    }
}

/// A few preview rows for the desktop box body.
pub fn preview_lines(path: &Path) -> Vec<(String, Option<(usize, usize)>)> {
    let store = Store::load(path);
    if store.items.is_empty() {
        return Vec::new();
    }
    let mut items: Vec<&TodoItem> = store.items.iter().collect();
    items.sort_by_key(|it| (it.done, it.priority.rank()));
    items
        .into_iter()
        .take(8)
        .map(|it| {
            let check = if it.done { "[x]" } else { "[ ]" };
            (format!("{check} {}", it.text), None)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_checkbox_rows() {
        let open = TodoItem {
            text: "Buy milk".into(),
            done: false,
            priority: Priority::High,
            due: "2026-10-12".into(),
        };
        assert_eq!(open.render_text(), "  [ ] Buy milk   (high, 2026-10-12)");
        let done = TodoItem {
            text: "Call mom".into(),
            done: true,
            priority: Priority::Med,
            due: String::new(),
        };
        assert_eq!(done.render_text(), "  [x] Call mom");
    }

    #[test]
    fn filter_and_sort() {
        let store = Store {
            items: vec![
                TodoItem { text: "a".into(), done: true, priority: Priority::Low, due: String::new() },
                TodoItem { text: "b".into(), done: false, priority: Priority::High, due: String::new() },
            ],
        };
        assert!(Filter::Open.keeps(&store.items[1]));
        assert!(!Filter::Open.keeps(&store.items[0]));
        assert!(Filter::Done.keeps(&store.items[0]));
    }

    #[test]
    fn app_add_toggle_persist() {
        let dir = std::env::temp_dir().join(format!("mm-todoapp-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("todos.json");
        // Add two items, toggle one done.
        let mut app = TodoApp::open("Tasks", path.clone());
        app.start_add();
        app.adding.as_mut().unwrap().fields[0].value = "Buy milk".into();
        app.finish_add();
        app.start_add();
        app.adding.as_mut().unwrap().fields[0].value = "Call mom".into();
        app.finish_add();
        assert_eq!(app.store.items.len(), 2);
        let idx = app.store.items.iter().position(|i| i.text == "Buy milk").unwrap();
        app.toggle(idx);
        // Reopen from disk: state survived the "restart".
        let reopened = TodoApp::open("Tasks", path.clone());
        assert_eq!(reopened.store.items.len(), 2);
        assert!(reopened.store.items.iter().any(|i| i.text == "Buy milk" && i.done));
        assert!(reopened.store.items.iter().any(|i| i.text == "Call mom" && !i.done));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_file_is_set_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("todo-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(Store::load(&path).items.is_empty());
        assert_eq!(fs::read_to_string(dir.join("todos.json.bad")).unwrap(), "{ not json");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn round_trips_json() {
        let dir = std::env::temp_dir().join(format!("mm-todo-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("todos.json");
        let store = Store {
            items: vec![TodoItem {
                text: "task".into(),
                done: true,
                priority: Priority::High,
                due: "2026-12-31".into(),
            }],
        };
        store.save(&path).unwrap();
        let back = Store::load(&path);
        assert_eq!(back.items.len(), 1);
        assert!(back.items[0].done);
        assert_eq!(back.items[0].priority, Priority::High);
        assert_eq!(back.items[0].due, "2026-12-31");
        let _ = fs::remove_dir_all(&dir);
    }
}
