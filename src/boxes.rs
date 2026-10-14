//! DeskMate-style desktop boxes: the box kinds, folder scanning for Folder /
//! Apps boxes, command building for launching a file with an app, and the
//! built-in multi-line editor used by Notes boxes.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

/// What a box on the desktop holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoxKind {
    /// A hand-made list of menu items (commands).
    #[default]
    Menu,
    /// Files in a folder, each opened with the box's app.
    Folder,
    /// A folder of programs; selecting one runs it.
    Apps,
    /// A single button that runs one command.
    Shortcut,
    /// Free-form text notes.
    Notes,
    /// Address book of vCard contacts.
    Contacts,
    /// Month calendar with appointments and a To-Do list.
    Calendar,
    /// Drawings in a paintings folder, opened in the built-in Paint.
    Paint,
    /// MIDI songs in a music folder, opened in the built-in music app.
    Music,
    /// A grid of cells with formulas, saved as CSV.
    Spreadsheet,
    /// RSS / news feeds in the built-in news reader.
    Rss,
    /// Text documents in a folder, edited in the built-in Notes editor.
    Documents,
    /// A pocket calculator.
    Calculator,
    /// Current conditions and a short forecast (Open-Meteo).
    Weather,
    /// A checkable to-do list (todui-style), each item a `[ ]` / `[x]` box.
    Todo,
}

pub const BOX_KINDS: [BoxKind; 15] = [
    BoxKind::Menu,
    BoxKind::Folder,
    BoxKind::Apps,
    BoxKind::Shortcut,
    BoxKind::Notes,
    BoxKind::Contacts,
    BoxKind::Calendar,
    BoxKind::Paint,
    BoxKind::Music,
    BoxKind::Spreadsheet,
    BoxKind::Rss,
    BoxKind::Documents,
    BoxKind::Calculator,
    BoxKind::Weather,
    BoxKind::Todo,
];

impl BoxKind {
    pub fn label(self) -> &'static str {
        match self {
            BoxKind::Menu => "Menu (list of commands)",
            BoxKind::Folder => "Folder (open files with an app)",
            BoxKind::Apps => "Apps folder (run programs in a folder)",
            BoxKind::Shortcut => "Shortcut (one app)",
            BoxKind::Notes => "Notes",
            BoxKind::Contacts => "Address Book (contact list)",
            BoxKind::Calendar => "Calendar (appointments and to-dos)",
            BoxKind::Paint => "Paint (drawings, opened in Paint)",
            BoxKind::Music => "Music (MIDI songs, opened in the music app)",
            BoxKind::Spreadsheet => "Spreadsheet (cells and formulas, .csv)",
            BoxKind::Rss => "RSS (news reader)",
            BoxKind::Documents => "Documents (text files, edited in Notes)",
            BoxKind::Calculator => "Calculator (pocket calculator)",
            BoxKind::Weather => "Weather (current conditions and forecast)",
            BoxKind::Todo => "To-Do (checkable task list)",
        }
    }

    pub fn is_menu(&self) -> bool {
        *self == BoxKind::Menu
    }

    pub fn uses_folder(self) -> bool {
        matches!(self, BoxKind::Folder | BoxKind::Apps)
    }

    /// Address Book / Calendar / Paint boxes keep their data in a file or
    /// folder that defaults to the config directory.
    pub fn uses_data_file(self) -> bool {
        matches!(
            self,
            BoxKind::Contacts
                | BoxKind::Calendar
                | BoxKind::Paint
                | BoxKind::Music
                | BoxKind::Spreadsheet
                | BoxKind::Rss
                | BoxKind::Documents
                | BoxKind::Weather
                | BoxKind::Todo
        )
    }

    /// For kinds whose target field is a *file* (Contacts, Calendar): the
    /// accepted file extensions, so the picker can filter by type.
    /// `None` means the field is a *folder* (Paint, Music, Folder, Apps).
    pub fn data_file_exts(self) -> Option<Vec<String>> {
        match self {
            BoxKind::Contacts => Some(vec![".vcf".to_string()]),
            BoxKind::Calendar => Some(vec![".json".to_string()]),
            BoxKind::Spreadsheet => Some(vec![".csv".to_string()]),
            BoxKind::Rss => Some(vec![".json".to_string()]),
            _ => None,
        }
    }

    pub fn launches(self) -> bool {
        matches!(self, BoxKind::Folder | BoxKind::Apps | BoxKind::Shortcut)
    }

    pub fn next(self) -> Self {
        let idx = BOX_KINDS.iter().position(|k| *k == self).unwrap_or(0);
        BOX_KINDS[(idx + 1) % BOX_KINDS.len()]
    }

    pub fn previous(self) -> Self {
        let idx = BOX_KINDS.iter().position(|k| *k == self).unwrap_or(0);
        BOX_KINDS[(idx + BOX_KINDS.len() - 1) % BOX_KINDS.len()]
    }
}

/// Expand a leading `~` to the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if trimmed == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(trimmed)
}

/// Split a filter such as `*.md, *.txt` into individual patterns.
fn filter_patterns(filter: &str) -> Vec<String> {
    filter
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Case-insensitive glob match supporting `*` and `?`.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ni;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("exe" | "bat" | "cmd" | "com")
    )
}

/// List the files a Folder / Apps box shows: regular, non-hidden files,
/// matching `filter` (blank = everything), and for Apps boxes only
/// executables. Sorted by name, case-insensitively.
pub fn list_folder(folder: &str, filter: &str, executables_only: bool) -> Result<Vec<PathBuf>, String> {
    if folder.trim().is_empty() {
        return Err("no folder set".into());
    }
    let dir = expand_tilde(folder);
    let read = fs::read_dir(&dir).map_err(|_| "folder not found".to_string())?;
    let patterns = filter_patterns(filter);
    let mut files: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            if name.starts_with('.') {
                return false;
            }
            patterns.is_empty() || patterns.iter().any(|p| glob_match(p, name))
        })
        .filter(|path| !executables_only || is_executable(path))
        .collect();
    files.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    Ok(files)
}

/// True for a file the Notes editor can edit: valid UTF-8 text with no NUL
/// bytes in its first 8 KiB (an empty file counts as text).
pub fn is_text_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let Ok(n) = file.read(&mut buf) else {
        return false;
    };
    let head = &buf[..n];
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // A multi-byte character cut off at the end of the buffer is fine.
        Err(err) => n == buf.len() && err.error_len().is_none(),
    }
}

/// Quote a string for safe use as one `sh -c` word.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Build the shell command that opens `file` with `app`. A `{}` in the app
/// command marks where the file goes (e.g. `vim -R {}`); otherwise the file is
/// appended.
pub fn open_with_command(app: &str, file: &Path) -> String {
    let quoted = shell_quote(&file.to_string_lossy());
    let app = app.trim();
    if app.contains("{}") {
        app.replace("{}", &quoted)
    } else {
        format!("{app} {quoted}")
    }
}

/// Word-wrap `text` to `width` columns, keeping blank lines.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in raw.split_whitespace() {
            let mut word: String = word.to_string();
            loop {
                let line_len = line.chars().count();
                let word_len = word.chars().count();
                let needed = if line.is_empty() { word_len } else { line_len + 1 + word_len };
                if needed <= width {
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(&word);
                    break;
                }
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                    continue;
                }
                // Single word longer than the box: hard-break it.
                let head: String = word.chars().take(width).collect();
                let tail: String = word.chars().skip(width).collect();
                out.push(head);
                word = tail;
                if word.is_empty() {
                    break;
                }
            }
        }
        out.push(line);
    }
    out
}

/// What a key press did inside the notes editor.
pub enum NotesKeyResult {
    Continue,
    Save,
    Discard,
}

/// A small multi-line text editor for Notes boxes.
pub struct NotesEditor {
    pub category_index: usize,
    pub box_name: String,
    /// For a Documents box: the file being edited (saved there instead of
    /// into the box's own notes).
    pub file: Option<PathBuf>,
    pub lines: Vec<Vec<char>>,
    pub row: usize,
    pub col: usize,
    pub scroll_row: usize,
    pub scroll_col: usize,
    /// Size of the text area at the last draw, used to keep the cursor in view.
    pub view: Cell<(usize, usize)>,
}

impl NotesEditor {
    pub fn new(category_index: usize, box_name: String, text: &str) -> Self {
        let mut lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        Self {
            category_index,
            box_name,
            file: None,
            lines,
            row: 0,
            col: 0,
            scroll_row: 0,
            scroll_col: 0,
            view: Cell::new((0, 0)),
        }
    }

    /// Edit a document file; a missing file starts empty.
    pub fn for_file(category_index: usize, file: PathBuf) -> Self {
        let text = fs::read_to_string(&file).unwrap_or_default();
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut editor = Self::new(category_index, name, &text);
        editor.file = Some(file);
        editor
    }

    pub fn text(&self) -> String {
        let joined: Vec<String> = self.lines.iter().map(|l| l.iter().collect()).collect();
        joined.join("\n").trim_end().to_string()
    }

    fn clamp_col(&mut self) {
        self.col = self.col.min(self.lines[self.row].len());
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> NotesKeyResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return NotesKeyResult::Save,
            KeyCode::Char('s') if ctrl => return NotesKeyResult::Save,
            KeyCode::Char('x') if ctrl => return NotesKeyResult::Discard,
            KeyCode::Char(c) if !ctrl => {
                self.lines[self.row].insert(self.col, c);
                self.col += 1;
            }
            KeyCode::Tab => {
                for _ in 0..4 {
                    self.lines[self.row].insert(self.col, ' ');
                    self.col += 1;
                }
            }
            KeyCode::Enter => {
                let rest = self.lines[self.row].split_off(self.col);
                self.lines.insert(self.row + 1, rest);
                self.row += 1;
                self.col = 0;
            }
            KeyCode::Backspace => {
                if self.col > 0 {
                    self.col -= 1;
                    self.lines[self.row].remove(self.col);
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                    self.lines[self.row].extend(line);
                }
            }
            KeyCode::Delete => {
                if self.col < self.lines[self.row].len() {
                    self.lines[self.row].remove(self.col);
                } else if self.row + 1 < self.lines.len() {
                    let line = self.lines.remove(self.row + 1);
                    self.lines[self.row].extend(line);
                }
            }
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                }
            }
            KeyCode::Right => {
                if self.col < self.lines[self.row].len() {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Up => {
                self.row = self.row.saturating_sub(1);
                self.clamp_col();
            }
            KeyCode::Down => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.clamp_col();
            }
            KeyCode::PageUp => {
                self.row = self.row.saturating_sub(10);
                self.clamp_col();
            }
            KeyCode::PageDown => {
                self.row = (self.row + 10).min(self.lines.len() - 1);
                self.clamp_col();
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.lines[self.row].len(),
            _ => {}
        }
        let (width, height) = self.view.get();
        self.scroll_into_view(width, height);
        NotesKeyResult::Continue
    }

    /// Move the cursor to a visible (row, col) clicked inside the text area.
    pub fn click(&mut self, visible_row: usize, visible_col: usize) {
        self.row = (self.scroll_row + visible_row).min(self.lines.len() - 1);
        self.col = self.scroll_col + visible_col;
        self.clamp_col();
    }

    /// Adjust scrolling so the cursor is inside a `width` x `height` view.
    pub fn scroll_into_view(&mut self, width: usize, height: usize) {
        if height > 0 {
            if self.row < self.scroll_row {
                self.scroll_row = self.row;
            } else if self.row >= self.scroll_row + height {
                self.scroll_row = self.row + 1 - height;
            }
        }
        if width > 0 {
            if self.col < self.scroll_col {
                self.scroll_col = self.col;
            } else if self.col >= self.scroll_col + width {
                self.scroll_col = self.col + 1 - width;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_box_kinds() {
        assert_eq!(serde_json::to_string(&BoxKind::Spreadsheet).unwrap(), "\"spreadsheet\"");
        assert_eq!(serde_json::from_str::<BoxKind>("\"rss\"").unwrap(), BoxKind::Rss);
        assert!(BOX_KINDS.contains(&BoxKind::Spreadsheet) && BOX_KINDS.contains(&BoxKind::Rss));
        assert_eq!(BoxKind::Rss.next(), BoxKind::Documents);
        assert_eq!(BoxKind::Documents.next(), BoxKind::Calculator);
        assert_eq!(BoxKind::Calculator.next(), BoxKind::Weather);
        assert_eq!(BoxKind::Weather.next(), BoxKind::Todo);
        assert_eq!(BoxKind::Todo.next(), BoxKind::Menu);
        assert_eq!(serde_json::to_string(&BoxKind::Documents).unwrap(), "\"documents\"");
        assert_eq!(BoxKind::Documents.data_file_exts(), None);
        assert_eq!(BoxKind::Spreadsheet.data_file_exts(), Some(vec![".csv".to_string()]));
        assert_eq!(BoxKind::Rss.data_file_exts(), Some(vec![".json".to_string()]));
    }


    #[test]
    fn glob_matching() {
        assert!(glob_match("*.txt", "Notes.TXT"));
        assert!(glob_match("report-??.md", "report-01.md"));
        assert!(!glob_match("*.md", "notes.txt"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn open_command_quotes_and_substitutes() {
        let path = Path::new("/tmp/it's here.txt");
        assert_eq!(open_with_command("nano", path), r"nano '/tmp/it'\''s here.txt'");
        assert_eq!(
            open_with_command("vim -R {} +1", path),
            r"vim -R '/tmp/it'\''s here.txt' +1"
        );
    }

    #[test]
    fn wraps_text() {
        assert_eq!(wrap_text("hello world foo", 11), vec!["hello world", "foo"]);
        assert_eq!(wrap_text("a\n\nb", 5), vec!["a", "", "b"]);
        assert_eq!(wrap_text("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn lists_folder_with_filter_and_executables() {
        let dir = std::env::temp_dir().join(format!("mm-boxes-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("b.txt"), "").unwrap();
        fs::write(dir.join("A.md"), "").unwrap();
        fs::write(dir.join(".hidden"), "").unwrap();
        let names = |v: Vec<PathBuf>| {
            v.iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
                .collect::<Vec<_>>()
        };
        let path = dir.to_string_lossy().to_string();
        assert_eq!(names(list_folder(&path, "", false).unwrap()), vec!["A.md", "b.txt"]);
        assert_eq!(names(list_folder(&path, "*.txt", false).unwrap()), vec!["b.txt"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.join("b.txt"), fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(names(list_folder(&path, "", true).unwrap()), vec!["b.txt"]);
        }
        assert!(list_folder(&dir.join("missing").to_string_lossy(), "", false).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn notes_editor_edits() {
        let mut ed = NotesEditor::new(0, "n".into(), "ab");
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        ed.handle_key(key(KeyCode::End));
        ed.handle_key(key(KeyCode::Enter));
        ed.handle_key(key(KeyCode::Char('c')));
        assert_eq!(ed.text(), "ab\nc");
        ed.handle_key(key(KeyCode::Home));
        ed.handle_key(key(KeyCode::Backspace));
        assert_eq!(ed.text(), "abc");
        assert_eq!((ed.row, ed.col), (0, 2));
    }

    #[test]
    fn notes_editor_for_file_reads_and_names_the_file() {
        let dir = std::env::temp_dir().join(format!("mm-notes-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("todo.txt");
        fs::write(&file, "one\ntwo\n").unwrap();
        let ed = NotesEditor::for_file(3, file.clone());
        assert_eq!(ed.box_name, "todo.txt");
        assert_eq!(ed.file.as_deref(), Some(file.as_path()));
        assert_eq!(ed.text(), "one\ntwo");
        assert!(is_text_file(&file));
        fs::write(dir.join("bin"), [0u8, 1, 2]).unwrap();
        assert!(!is_text_file(&dir.join("bin")));
        fs::remove_dir_all(dir).unwrap();
    }
}
