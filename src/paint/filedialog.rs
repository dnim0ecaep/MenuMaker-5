//! The built-in DeskMate-style "Open Painting" / "Import Image" dialog that
//! replaces Tui-Desktop's file-manager pick. State and key handling live
//! here; drawing is in `ui.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogPurpose {
    /// `.ans`, `.txt` and images (images are imported).
    Open,
    /// Images only.
    Import,
    /// Drawings (`.ans`, `.txt`) for a desktop shortcut icon.
    Icon,
    /// Folders only; `o` confirms the selected folder.
    Folder,
    /// A single data file with an extension set on the dialog (e.g. a
    /// `.vcf` address book or a `.json` calendar); Enter opens it.
    DataFile,
}

impl DialogPurpose {
    pub fn title(self) -> &'static str {
        match self {
            DialogPurpose::Open => "Open Painting",
            DialogPurpose::Import => "Import Image",
            DialogPurpose::Icon => "Choose Icon Picture",
            DialogPurpose::Folder => "Choose Folder",
            DialogPurpose::DataFile => "Choose File",
        }
    }

    /// Whether a file with `path` is an acceptable *file* pick for this
    /// purpose. `DataFile` matches by extension, so it is handled on the
    /// dialog (which carries the extension set), not here.
    pub fn accepts(self, path: &Path) -> bool {
        match self {
            DialogPurpose::Open => super::is_paintable(path),
            DialogPurpose::Import => super::is_image(path),
            DialogPurpose::Icon => super::is_paintable(path) && !super::is_image(path),
            // Folder picks list directories only.
            DialogPurpose::Folder => false,
            // DataFile is decided by the dialog's extension set.
            DialogPurpose::DataFile => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub enum DialogResult {
    Continue,
    Cancel,
    Open(PathBuf),
}

pub struct FileDialog {
    pub purpose: DialogPurpose,
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    /// For [`DialogPurpose::DataFile`]: the lower-cased extensions (with
    /// leading dot, e.g. `".vcf"`) an acceptable file must end in.
    pub file_exts: Vec<String>,
    /// Typed text: filters the list, or — when it looks like a path
    /// (contains `/` or starts with `~`) — is opened as a path on Enter.
    pub input: String,
    /// Index into `visible()`.
    pub selected: usize,
    pub scroll: usize,
    /// Rows shown by the last render (for PgUp/PgDn).
    pub page: usize,
    pub error: Option<String>,
}

impl FileDialog {
    pub fn new(purpose: DialogPurpose, dir: PathBuf) -> Self {
        Self::new_with_exts(purpose, dir, Vec::new())
    }

    /// Like [`Self::new`] but with an explicit extension set (used by
    /// [`DialogPurpose::DataFile`] to filter the file list).
    pub fn new_with_exts(purpose: DialogPurpose, dir: PathBuf, file_exts: Vec<String>) -> Self {
        let mut dialog = Self {
            purpose,
            dir,
            entries: Vec::new(),
            file_exts,
            input: String::new(),
            selected: 0,
            scroll: 0,
            page: 10,
            error: None,
        };
        dialog.refresh();
        dialog
    }

    /// A data file must match one of `file_exts`.
    fn accepts_ext(&self, path: &Path) -> bool {
        if self.purpose != DialogPurpose::DataFile {
            return self.purpose.accepts(path);
        }
        if self.file_exts.is_empty() {
            return true;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false
        };
        let ext = format!(".{}", ext.to_lowercase());
        self.file_exts.iter().any(|a| a == &ext)
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        self.selected = 0;
        self.scroll = 0;
        if let Some(parent) = self.dir.parent() {
            self.entries.push(Entry { name: "..".into(), path: parent.to_path_buf(), is_dir: true });
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        match fs::read_dir(&self.dir) {
            Ok(read) => {
                for entry in read.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(Entry { name, path, is_dir: true });
                    } else if self.accepts_ext(&path) {
                        files.push(Entry { name, path, is_dir: false });
                    }
                }
            }
            Err(e) => self.error = Some(format!("Cannot read folder: {e}")),
        }
        dirs.sort_by_key(|e| e.name.to_lowercase());
        files.sort_by_key(|e| e.name.to_lowercase());
        self.entries.extend(dirs);
        self.entries.extend(files);
    }

    fn input_is_path(&self) -> bool {
        self.input.contains('/') || self.input.starts_with('~')
    }

    /// Indices into `entries` matching the typed filter (`..` always stays).
    pub fn visible(&self) -> Vec<usize> {
        if self.input.is_empty() || self.input_is_path() {
            return (0..self.entries.len()).collect();
        }
        let needle = self.input.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name == ".." || e.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir.canonicalize().unwrap_or(dir);
        self.input.clear();
        self.error = None;
        self.refresh();
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.dir.parent() {
            let child = self.dir.file_name().map(|n| n.to_string_lossy().to_string());
            self.set_dir(parent.to_path_buf());
            // Land on the folder we just left.
            if let Some(child) = child {
                if let Some(pos) = self.entries.iter().position(|e| e.name == child) {
                    self.selected = pos;
                }
            }
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }

    /// Opens the `idx`-th visible entry (folder: enter it; file: open it).
    pub fn activate(&mut self, idx: usize) -> DialogResult {
        let visible = self.visible();
        let Some(&entry_idx) = visible.get(idx) else { return DialogResult::Continue };
        let entry = self.entries[entry_idx].clone();
        if entry.is_dir {
            if entry.name == ".." {
                self.go_up();
            } else {
                self.set_dir(entry.path);
            }
            DialogResult::Continue
        } else {
            // Folder picks confirm a file's parent directory (Windows-style).
            if self.purpose == DialogPurpose::Folder {
                return DialogResult::Open(entry.path.parent().map(|p| p.to_path_buf()).unwrap_or(entry.path));
            }
            DialogResult::Open(entry.path)
        }
    }

    /// Confirms the selection as a folder: a directory is confirmed as-is, a
    /// file as its parent directory.
    pub fn confirm_selected(&mut self) -> DialogResult {
        let visible = self.visible();
        let Some(&entry_idx) = visible.get(self.selected.min(visible.len().saturating_sub(1))) else {
            return DialogResult::Continue
        };
        let entry = self.entries[entry_idx].clone();
        if entry.is_dir {
            if entry.name == ".." {
                DialogResult::Open(self.dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| self.dir.clone()))
            } else {
                DialogResult::Open(entry.path)
            }
        } else {
            DialogResult::Open(entry.path.parent().map(|p| p.to_path_buf()).unwrap_or(entry.path))
        }
    }

    fn open_typed_path(&mut self) -> DialogResult {
        let typed = self.input.trim().to_string();
        let expanded = if let Some(rest) = typed.strip_prefix('~') {
            match dirs::home_dir() {
                Some(home) => home.join(rest.trim_start_matches('/')),
                None => PathBuf::from(&typed),
            }
        } else {
            PathBuf::from(&typed)
        };
        let path = if expanded.is_absolute() { expanded } else { self.dir.join(expanded) };
        if path.is_dir() {
            self.set_dir(path);
            DialogResult::Continue
        } else if path.is_file() {
            if self.purpose == DialogPurpose::Folder {
                // Typed file path: confirm its parent folder.
                DialogResult::Open(path.parent().map(|p| p.to_path_buf()).unwrap_or(path))
            } else if self.accepts_ext(&path) {
                DialogResult::Open(path)
            } else {
                self.error = Some(
                    "That file type isn't allowed here".to_string(),
                );
                DialogResult::Continue
            }
        } else {
            self.error = Some(format!("Not found: {}", path.display()));
            DialogResult::Continue
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        self.error = None;
        let page = self.page.max(1) as i32;
        match key.code {
            KeyCode::Esc => return DialogResult::Cancel,
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-page),
            KeyCode::PageDown => self.move_selection(page),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.move_selection(i32::MAX / 2),
            KeyCode::Enter => {
                if self.input_is_path() {
                    return self.open_typed_path();
                }
                return self.activate(self.selected);
            }
            KeyCode::Backspace => {
                if self.input.pop().is_none() {
                    self.go_up();
                } else {
                    self.selected = 0;
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O')
                if self.purpose == DialogPurpose::Folder
                    && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                return self.confirm_selected();
            }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.input.push(c);
                self.selected = 0;
                self.scroll = 0;
            }
            _ => {}
        }
        DialogResult::Continue
    }

    /// A click on the `idx`-th visible row: selects it, or opens it if it
    /// was already selected (a stand-in for double-click).
    pub fn click(&mut self, idx: usize) -> DialogResult {
        if idx == self.selected {
            return self.activate(idx);
        }
        self.selected = idx;
        DialogResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mm-paint-dlg-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["b.ans", "a.TXT", "pic.png", "notes.md", ".hidden.ans"] {
            fs::write(dir.join(name), "x").unwrap();
        }
        dir
    }

    fn names(d: &FileDialog) -> Vec<String> {
        d.visible().iter().map(|&i| d.entries[i].name.clone()).collect()
    }

    #[test]
    fn lists_folders_then_paintable_files() {
        let dir = temp_tree("list");
        let d = FileDialog::new(DialogPurpose::Open, dir.clone());
        assert_eq!(names(&d), ["..", "sub", "a.TXT", "b.ans", "pic.png"]);
        let d = FileDialog::new(DialogPurpose::Import, dir.clone());
        assert_eq!(names(&d), ["..", "sub", "pic.png"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_file_filters_by_extension() {
        let dir = std::env::temp_dir().join(format!("mm-df-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("people.vcf"), "x").unwrap();
        fs::write(dir.join("cal.json"), "x").unwrap();
        fs::write(dir.join("notes.md"), "x").unwrap();

        // Contacts: only .vcf listed.
        let d = FileDialog::new_with_exts(
            DialogPurpose::DataFile,
            dir.clone(),
            vec![".vcf".to_string()],
        );
        assert_eq!(names(&d), ["..", "people.vcf"]);
        // Calendar: only .json listed.
        let d = FileDialog::new_with_exts(
            DialogPurpose::DataFile,
            dir.clone(),
            vec![".json".to_string()],
        );
        assert_eq!(names(&d), ["..", "cal.json"]);
        // A typed file of the wrong extension is rejected.
        let mut d = FileDialog::new_with_exts(
            DialogPurpose::DataFile,
            dir.clone(),
            vec![".vcf".to_string()],
        );
        d.input = dir.join("notes.md").to_string_lossy().to_string();
        d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(d.error.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_filters_and_enter_opens() {
        let dir = temp_tree("filter");
        let mut d = FileDialog::new(DialogPurpose::Open, dir.clone());
        d.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        d.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(names(&d), ["..", "b.ans"]);
        d.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        match d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            DialogResult::Open(p) => assert_eq!(p, dir.join("b.ans")),
            _ => panic!("expected Open"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn enter_on_folder_navigates_and_backspace_goes_up() {
        let dir = temp_tree("nav");
        let mut d = FileDialog::new(DialogPurpose::Open, dir.clone());
        d.selected = 1; // "sub"
        assert!(matches!(d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), DialogResult::Continue));
        assert!(d.dir.ends_with("sub"));
        d.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(d.dir, dir.canonicalize().unwrap());
        assert_eq!(names(&d)[d.selected], "sub");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typed_path_opens_file_or_reports_errors() {
        let dir = temp_tree("path");
        let mut d = FileDialog::new(DialogPurpose::Open, PathBuf::from("/"));
        for c in dir.join("b.ans").to_string_lossy().chars() {
            d.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(matches!(d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), DialogResult::Open(_)));
        d.input = dir.join("notes.md").to_string_lossy().to_string();
        d.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(d.error.is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
