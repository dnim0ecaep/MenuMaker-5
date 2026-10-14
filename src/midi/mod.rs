//! Music: a MIDI composer (piano roll, project timeline, multi-track
//! mixing, SoundFont playback, MIDI/JSON/WAV import and export), ported from
//! miditui (<https://github.com/minimaxir/miditui>) and drawn DeskMate-style
//! (theme-colored double boxes, a function-key menu bar, centered dialogs).
//!
//! Module map (miditui file -> here):
//! - `src/app.rs` -> [`app`] (state, editing, sequencer, mouse handlers)
//! - `src/main.rs` key/mouse dispatch -> [`keys`] and [`mouse`]
//! - `src/midi/*` -> [`model`]; `src/audio/*` -> [`audio`];
//!   `src/history.rs` -> [`history`]; `src/ui/*` -> [`ui`]
//!
//! Public interface: [`MidiApp`], [`MidiResult`] and [`is_midi_file`].
//! The host must call [`MidiApp::tick`] every loop iteration (~30 ms) so
//! playback advances; nothing here draws outside `render`.

mod app;
mod audio;
mod filedialog;
mod history;
mod keys;
mod model;
mod mouse;
mod settings;
mod ui;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use app::{App, EditMode, SaveFormat};
use audio::PlaybackState;
use ui::{ctrl, key, HostLayout};

/// What `handle_key`/`handle_mouse` ask the host to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiResult {
    Continue,
    Close,
}

/// What to do once the Save/Discard/Cancel prompt is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsavedAction {
    Close,
    Open,
    RecoverAutosave,
}

/// One dropdown item; a blank label is a separator.
struct MenuItem {
    label: &'static str,
    /// The keyboard shortcut shown right-aligned in the dropdown.
    hint: &'static str,
    action: MenuAction,
}

impl MenuItem {
    const fn new(label: &'static str, hint: &'static str, action: MenuAction) -> Self {
        Self { label, hint, action }
    }
    const fn sep() -> Self {
        Self { label: "", hint: "", action: MenuAction::Separator }
    }
    fn is_separator(&self) -> bool {
        self.label.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAction {
    Separator,
    New,
    Open,
    Save,
    RecoverAutosave,
    ExportWav,
    ExportMidi,
    LoadSoundFont,
    Exit,
    Undo,
    Redo,
    InsertMode,
    SelectMode,
    NormalMode,
    PlaceNote,
    DeleteNote,
    DeleteSelected,
    ClearSelection,
    CycleView,
    ToggleTrackView,
    CycleHighlight,
    ZoomIn,
    ZoomOut,
    CycleFocus,
    AddTrack,
    DeleteTrack,
    RenameTrack,
    Mute,
    Solo,
    NextTrack,
    PrevTrack,
    NextInstrument,
    PrevInstrument,
    VolumeUp,
    VolumeDown,
    PanLeft,
    PanRight,
    PlayPause,
    Restart,
    Stop,
    GoStart,
    GoEnd,
    TempoUp,
    TempoDown,
    TimeSigUp,
    TimeSigDown,
    TimeSigDenominator,
    OctaveUp,
    OctaveDown,
    KeyHelp,
    About,
}

const MENU_FILE: &[MenuItem] = &[
    MenuItem::new("New Project...", "^N", MenuAction::New),
    MenuItem::new("Open...", "^O", MenuAction::Open),
    MenuItem::new("Save...", "^S", MenuAction::Save),
    MenuItem::new("Recover Autosave", "", MenuAction::RecoverAutosave),
    MenuItem::sep(),
    MenuItem::new("Export WAV", "e", MenuAction::ExportWav),
    MenuItem::new("Export MIDI", "^M", MenuAction::ExportMidi),
    MenuItem::new("Load SoundFont...", "^L", MenuAction::LoadSoundFont),
    MenuItem::sep(),
    MenuItem::new("Exit", "Esc", MenuAction::Exit),
];

const MENU_EDIT: &[MenuItem] = &[
    MenuItem::new("Undo", "^Z", MenuAction::Undo),
    MenuItem::new("Redo", "^Y", MenuAction::Redo),
    MenuItem::sep(),
    MenuItem::new("Insert Mode", "i", MenuAction::InsertMode),
    MenuItem::new("Select Mode", "v", MenuAction::SelectMode),
    MenuItem::new("Normal Mode", "Esc", MenuAction::NormalMode),
    MenuItem::sep(),
    MenuItem::new("Place Note", "Enter", MenuAction::PlaceNote),
    MenuItem::new("Delete Note", "Del", MenuAction::DeleteNote),
    MenuItem::new("Delete Selected Notes", "x (Sel)", MenuAction::DeleteSelected),
    MenuItem::new("Clear Selection", "c (Sel)", MenuAction::ClearSelection),
];

const MENU_VIEW: &[MenuItem] = &[
    MenuItem::new("Cycle View", "g", MenuAction::CycleView),
    MenuItem::new("Compact/Expanded Tracks", "t", MenuAction::ToggleTrackView),
    MenuItem::new("Playback Highlight", "W", MenuAction::CycleHighlight),
    MenuItem::sep(),
    MenuItem::new("Zoom In", "=", MenuAction::ZoomIn),
    MenuItem::new("Zoom Out", "-", MenuAction::ZoomOut),
    MenuItem::sep(),
    MenuItem::new("Next Panel", "Tab", MenuAction::CycleFocus),
];

const MENU_TRACK: &[MenuItem] = &[
    MenuItem::new("Add Track", "a", MenuAction::AddTrack),
    MenuItem::new("Delete Track", "d", MenuAction::DeleteTrack),
    MenuItem::new("Rename Track", "r", MenuAction::RenameTrack),
    MenuItem::sep(),
    MenuItem::new("Mute", "m", MenuAction::Mute),
    MenuItem::new("Solo", "s", MenuAction::Solo),
    MenuItem::new("Next Track", "J", MenuAction::NextTrack),
    MenuItem::new("Previous Track", "K", MenuAction::PrevTrack),
    MenuItem::sep(),
    MenuItem::new("Next Instrument", ">", MenuAction::NextInstrument),
    MenuItem::new("Previous Instrument", "<", MenuAction::PrevInstrument),
    MenuItem::new("Volume Up", "'", MenuAction::VolumeUp),
    MenuItem::new("Volume Down", ";", MenuAction::VolumeDown),
    MenuItem::new("Pan Left", "(", MenuAction::PanLeft),
    MenuItem::new("Pan Right", ")", MenuAction::PanRight),
];

const MENU_PLAY: &[MenuItem] = &[
    MenuItem::new("Play / Pause", "Space", MenuAction::PlayPause),
    MenuItem::new("Restart", "Sh+Space", MenuAction::Restart),
    MenuItem::new("Stop", ".", MenuAction::Stop),
    MenuItem::new("Go to Start", "0", MenuAction::GoStart),
    MenuItem::new("Go to End", "$", MenuAction::GoEnd),
    MenuItem::sep(),
    MenuItem::new("Tempo +5", "]", MenuAction::TempoUp),
    MenuItem::new("Tempo -5", "[", MenuAction::TempoDown),
    MenuItem::new("Beats per Measure +", "}", MenuAction::TimeSigUp),
    MenuItem::new("Beats per Measure -", "{", MenuAction::TimeSigDown),
    MenuItem::new("Cycle Beat Unit", "|", MenuAction::TimeSigDenominator),
    MenuItem::sep(),
    MenuItem::new("Octave Up", "/", MenuAction::OctaveUp),
    MenuItem::new("Octave Down", ",", MenuAction::OctaveDown),
];

const MENU_HELP: &[MenuItem] = &[
    MenuItem::new("Keyboard Shortcuts", "?", MenuAction::KeyHelp),
    MenuItem::new("About Music", "", MenuAction::About),
];

/// A top-level menu and the function key that opens it.
struct Menu {
    label: &'static str,
    fkey: u8,
    items: &'static [MenuItem],
}

/// The DeskMate-style command bar: `File F2  Edit F3  View F4  Track F5
/// Play F6  Help F1`.
const MENU_BAR: &[Menu] = &[
    Menu { label: "File", fkey: 2, items: MENU_FILE },
    Menu { label: "Edit", fkey: 3, items: MENU_EDIT },
    Menu { label: "View", fkey: 4, items: MENU_VIEW },
    Menu { label: "Track", fkey: 5, items: MENU_TRACK },
    Menu { label: "Play", fkey: 6, items: MENU_PLAY },
    Menu { label: "Help", fkey: 1, items: MENU_HELP },
];

/// A shortcut-bar chip: (key shown, label, key event a click sends).
type Chip = (&'static str, &'static str, KeyEvent);

/// Tracks the last click position and time for double-click detection.
struct ClickTracker {
    last_pos: Option<(u16, u16)>,
    last_time: Option<std::time::Instant>,
}

/// Time threshold for detecting double-clicks (in milliseconds).
const DOUBLE_CLICK_THRESHOLD_MS: u128 = 400;

impl ClickTracker {
    fn new() -> Self {
        Self { last_pos: None, last_time: None }
    }

    /// Records a click and returns true if it's a double-click.
    fn record_click(&mut self, x: u16, y: u16) -> bool {
        let now = std::time::Instant::now();
        let is_double = match (self.last_pos, self.last_time) {
            (Some((lx, ly)), Some(lt)) => lx == x && ly == y && now.duration_since(lt).as_millis() < DOUBLE_CLICK_THRESHOLD_MS,
            _ => false,
        };
        if is_double {
            self.last_pos = None;
            self.last_time = None;
        } else {
            self.last_pos = Some((x, y));
            self.last_time = Some(now);
        }
        is_double
    }
}

/// The Music app: miditui's `App` plus the DeskMate host chrome (menus,
/// dialogs, unsaved-changes prompt, clickable shortcut bar).
pub struct MidiApp {
    app: App,
    show_help: bool,
    /// A read-only message box (About); any key closes it.
    info: Option<(&'static str, Vec<String>)>,
    menu_open: Option<usize>,
    menu_selected: usize,
    unsaved: Option<UnsavedAction>,
    /// Carried through a Save started from the unsaved-changes prompt, so
    /// the guarded action runs once the save succeeds.
    save_then: Option<UnsavedAction>,
    click_tracker: ClickTracker,
    /// Where the left button went down (for piano-key release).
    last_mouse_pos: Option<(u16, u16)>,
    /// Hit-test rectangles from the last render.
    layout: HostLayout,
}

impl MidiApp {
    /// Blank project. Files/config default to `data_dir` (create it if missing).
    pub fn new(data_dir: PathBuf) -> Self {
        Self::build(data_dir, None, true)
    }

    /// Open a .mid/.midi/.json/.oxm file (a missing .mid path starts blank and saves there).
    pub fn open(data_dir: PathBuf, file: &Path) -> Self {
        Self::build(data_dir, Some(file), true)
    }

    /// `open_output: false` keeps the audio device closed (tests).
    fn build(data_dir: PathBuf, file: Option<&Path>, open_output: bool) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let soundfont = settings::detect_soundfont(&data_dir);
        let found = soundfont.is_some();
        let mut app = App::new(soundfont, data_dir, open_output);
        if let Some(file) = file {
            if file.exists() {
                app.load_project(file.to_path_buf());
            } else {
                // A new file: start blank and save there.
                if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
                    app.project_mut().name = stem.to_string();
                }
                app.project_path = Some(file.to_path_buf());
                app.set_status(format!("New file: {}", file_name(file)));
            }
        }
        if !found && app.soundfont_path.is_none() {
            // miditui's first-run SoundFont prompt (Esc continues silently).
            app.open_soundfont_dialog(true);
        }
        Self {
            app,
            show_help: false,
            info: None,
            menu_open: None,
            menu_selected: 0,
            unsaved: None,
            save_then: None,
            click_tracker: ClickTracker::new(),
            last_mouse_pos: None,
            layout: HostLayout::default(),
        }
    }

    /// Called by the host every loop iteration (~every 30 ms while open):
    /// advance playback position, autosave timers, etc.
    pub fn tick(&mut self) {
        self.app.tick();
    }

    fn file_label(&self) -> String {
        match &self.app.project_path {
            Some(p) => file_name(p),
            None => "Untitled".to_string(),
        }
    }

    /// A dialog or overlay owns the keyboard.
    fn is_modal(&self) -> bool {
        self.info.is_some()
            || self.show_help
            || self.app.file_dialog.is_some()
            || self.unsaved.is_some()
            || self.app.save_dialog.open
            || self.app.new_project_dialog.open
            || self.app.renaming_track
    }

    // ----- closing and the unsaved-changes prompt -----

    fn unsaved_prompt(&mut self, action: UnsavedAction) {
        self.unsaved = Some(action);
        self.app.set_status("Unsaved changes - S) Save  D) Discard  Esc) Cancel");
    }

    /// Esc / q / Ctrl+C / File > Exit: close, via the unsaved prompt.
    fn request_close(&mut self) -> MidiResult {
        if self.app.dirty {
            self.unsaved_prompt(UnsavedAction::Close);
            MidiResult::Continue
        } else {
            self.proceed(UnsavedAction::Close)
        }
    }

    fn request(&mut self, action: UnsavedAction) -> MidiResult {
        if self.app.dirty {
            self.unsaved_prompt(action);
            MidiResult::Continue
        } else {
            self.proceed(action)
        }
    }

    fn proceed(&mut self, action: UnsavedAction) -> MidiResult {
        match action {
            UnsavedAction::Close => {
                // The audio stream lives only while the app is open.
                self.app.release_all_notes();
                self.app.audio.shutdown();
                return MidiResult::Close;
            }
            UnsavedAction::Open => self.app.open_file_browser(),
            UnsavedAction::RecoverAutosave => {
                if self.app.has_autosave() {
                    self.app.try_load_autosave();
                } else {
                    self.app.set_status("No autosave to recover");
                }
            }
        }
        MidiResult::Continue
    }

    fn resolve_unsaved_key(&mut self, key: KeyEvent) -> MidiResult {
        let Some(action) = self.unsaved else { return MidiResult::Continue };
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Enter => {
                self.unsaved = None;
                let current = self.app.project_path.clone();
                match current.as_deref().and_then(|p| SaveFormat::from_path(p).map(|f| (p.to_path_buf(), f))) {
                    Some((path, format)) => {
                        if self.app.save_to(path, format) {
                            return self.proceed(action);
                        }
                    }
                    None => {
                        self.save_then = Some(action);
                        self.app.open_save_dialog();
                    }
                }
                MidiResult::Continue
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.unsaved = None;
                self.app.dirty = false;
                self.proceed(action)
            }
            KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C') => {
                self.unsaved = None;
                self.app.set_status("Cancelled");
                MidiResult::Continue
            }
            _ => MidiResult::Continue,
        }
    }

    /// Runs the action that waited for a save, if the save went through.
    fn finish_save_then(&mut self, saved: bool) -> MidiResult {
        match self.save_then.take() {
            Some(action) if saved => self.proceed(action),
            _ => MidiResult::Continue,
        }
    }

    // ----- menus -----

    fn open_menu(&mut self, idx: usize) {
        self.menu_open = Some(idx);
        self.menu_selected = first_selectable(MENU_BAR[idx].items);
    }

    fn close_menu(&mut self) {
        self.menu_open = None;
    }

    fn menu_move_selection(&mut self, dir: i32) {
        let Some(idx) = self.menu_open else { return };
        let items = MENU_BAR[idx].items;
        let mut sel = self.menu_selected as i32;
        for _ in 0..items.len() {
            sel = (sel + dir).rem_euclid(items.len() as i32);
            if !items[sel as usize].is_separator() {
                break;
            }
        }
        self.menu_selected = sel as usize;
    }

    fn handle_menu_key(&mut self, key: KeyEvent) -> MidiResult {
        if let Some(idx) = menu_for_key(key.code) {
            if self.menu_open == Some(idx) {
                self.close_menu();
            } else {
                self.open_menu(idx);
            }
            return MidiResult::Continue;
        }
        match key.code {
            KeyCode::Left | KeyCode::Right => {
                if let Some(idx) = self.menu_open {
                    let dir = if key.code == KeyCode::Left { -1 } else { 1 };
                    self.open_menu((idx as i32 + dir).rem_euclid(MENU_BAR.len() as i32) as usize);
                }
            }
            KeyCode::Up => self.menu_move_selection(-1),
            KeyCode::Down => self.menu_move_selection(1),
            KeyCode::Home => {
                if let Some(idx) = self.menu_open {
                    self.menu_selected = first_selectable(MENU_BAR[idx].items);
                }
            }
            KeyCode::End => {
                if let Some(idx) = self.menu_open {
                    self.menu_selected = MENU_BAR[idx].items.iter().rposition(|i| !i.is_separator()).unwrap_or(0);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(idx) = self.menu_open {
                    return self.activate_menu_item(idx, self.menu_selected);
                }
            }
            KeyCode::Esc => self.close_menu(),
            _ => {}
        }
        MidiResult::Continue
    }

    /// Runs a dropdown item and closes the menu. Each item does the same as
    /// its keyboard shortcut.
    fn activate_menu_item(&mut self, menu_idx: usize, item_idx: usize) -> MidiResult {
        self.menu_open = None;
        let Some(item) = MENU_BAR.get(menu_idx).and_then(|m| m.items.get(item_idx)) else {
            return MidiResult::Continue;
        };
        use MenuAction as A;
        let normal = |c: char| key(KeyCode::Char(c));
        match item.action {
            A::Separator => MidiResult::Continue,
            A::New => self.dispatch(ctrl('n')),
            A::Open => self.dispatch(ctrl('o')),
            A::Save => self.dispatch(ctrl('s')),
            A::RecoverAutosave => self.request(UnsavedAction::RecoverAutosave),
            A::ExportWav => self.dispatch(ctrl('e')),
            A::ExportMidi => self.dispatch(ctrl('m')),
            A::LoadSoundFont => self.dispatch(ctrl('l')),
            A::Exit => self.request_close(),
            A::Undo => self.dispatch(ctrl('z')),
            A::Redo => self.dispatch(ctrl('y')),
            A::InsertMode => {
                self.app.edit_mode = EditMode::Insert;
                self.app.set_status("Insert mode - keys play & insert notes");
                MidiResult::Continue
            }
            A::SelectMode => {
                self.app.edit_mode = EditMode::Select;
                self.app.set_status("Select mode");
                MidiResult::Continue
            }
            A::NormalMode => {
                self.leave_mode();
                MidiResult::Continue
            }
            A::PlaceNote => {
                self.app.place_note();
                MidiResult::Continue
            }
            A::DeleteNote => {
                self.app.delete_note_at_cursor();
                MidiResult::Continue
            }
            A::DeleteSelected => {
                self.delete_selected_notes();
                MidiResult::Continue
            }
            A::ClearSelection => {
                self.app.selected_notes.clear();
                self.app.set_status("Selection cleared");
                MidiResult::Continue
            }
            A::CycleFocus => self.dispatch(key(KeyCode::Tab)),
            A::PlayPause => self.dispatch(normal(' ')),
            A::Restart => self.dispatch(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SHIFT)),
            A::KeyHelp => {
                self.show_help = true;
                MidiResult::Continue
            }
            A::About => {
                self.info = Some((
                    "About Music",
                    vec![
                        "Music - a terminal MIDI composer".into(),
                        String::new(),
                        "Ported from miditui by Max Woolf (MIT license).".into(),
                        "Piano roll, multi-track timeline, SoundFont playback,".into(),
                        "MIDI / JSON / OXM files and WAV export.".into(),
                    ],
                ));
                MidiResult::Continue
            }
            // Normal-mode single-key commands.
            other => {
                let c = match other {
                    A::CycleView => 'g',
                    A::ToggleTrackView => 't',
                    A::CycleHighlight => 'W',
                    A::ZoomIn => '=',
                    A::ZoomOut => '-',
                    A::AddTrack => 'a',
                    A::DeleteTrack => 'd',
                    A::RenameTrack => 'r',
                    A::Mute => 'm',
                    A::Solo => 's',
                    A::NextTrack => 'J',
                    A::PrevTrack => 'K',
                    A::NextInstrument => '>',
                    A::PrevInstrument => '<',
                    A::VolumeUp => '\'',
                    A::VolumeDown => ';',
                    A::PanLeft => '(',
                    A::PanRight => ')',
                    A::Stop => '.',
                    A::GoStart => '0',
                    A::GoEnd => '$',
                    A::TempoUp => ']',
                    A::TempoDown => '[',
                    A::TimeSigUp => '}',
                    A::TimeSigDown => '{',
                    A::TimeSigDenominator => '|',
                    A::OctaveUp => '/',
                    A::OctaveDown => ',',
                    _ => return MidiResult::Continue,
                };
                self.normal_mode_command(c)
            }
        }
    }

    // ----- status and shortcut bar -----

    /// The shortcut-bar chips for the current mode; each is clickable.
    fn chips(&self) -> Vec<Chip> {
        use KeyCode::*;
        if self.info.is_some() {
            return vec![("Enter", "Close", key(Enter))];
        }
        if self.show_help {
            return vec![("↑", "Up", key(Up)), ("↓", "Down", key(Down)), ("PgUp", "Page Up", key(PageUp)), ("PgDn", "Page Down", key(PageDown)), ("Esc", "Close", key(Esc))];
        }
        if self.app.file_dialog.is_some() {
            return vec![
                ("Enter", "Open", key(Enter)),
                ("Bksp", "Up", key(Backspace)),
                ("↑", "Up", key(Up)),
                ("↓", "Down", key(Down)),
                ("PgUp", "Page Up", key(PageUp)),
                ("PgDn", "Page Down", key(PageDown)),
                ("Esc", "Cancel", key(Esc)),
            ];
        }
        if self.unsaved.is_some() {
            return vec![("S", "Save", key(Char('s'))), ("D", "Discard", key(Char('d'))), ("Esc", "Cancel", key(Esc))];
        }
        if self.app.save_dialog.open {
            if self.app.save_dialog.overwrite.is_some() {
                return vec![("Y", "Overwrite", key(Char('y'))), ("N", "Cancel", key(Char('n')))];
            }
            return vec![("Enter", "Save", key(Enter)), ("Tab", "Format", key(Tab)), ("Esc", "Cancel", key(Esc))];
        }
        if self.app.new_project_dialog.open {
            return vec![("Y", "Yes", key(Char('y'))), ("N", "No", key(Char('n'))), ("Enter", "Confirm", key(Enter)), ("Esc", "Cancel", key(Esc))];
        }
        if self.app.renaming_track {
            return vec![("Enter", "Rename", key(Enter)), ("Esc", "Cancel", key(Esc))];
        }
        if self.menu_open.is_some() {
            return vec![
                ("←", "Prev Menu", key(Left)),
                ("→", "Next Menu", key(Right)),
                ("↑", "Up", key(Up)),
                ("↓", "Down", key(Down)),
                ("Enter", "Select", key(Enter)),
                ("Esc", "Close Menu", key(Esc)),
            ];
        }
        let playing = self.app.audio.is_playing();
        let play = ("Space", if playing { "Pause" } else { "Play" }, key(Char(' ')));
        match self.app.edit_mode {
            EditMode::Normal => vec![
                ("Esc", "Close", key(Esc)),
                play,
                (".", "Stop", key(Char('.'))),
                ("i", "Insert", key(Char('i'))),
                ("v", "Select", key(Char('v'))),
                ("Enter", "Note", key(Enter)),
                ("a", "Add Track", key(Char('a'))),
                ("g", "View", key(Char('g'))),
                ("^Z", "Undo", ctrl('z')),
                ("^S", "Save", ctrl('s')),
                ("^O", "Open", ctrl('o')),
                ("Tab", "Panel", key(Tab)),
                ("?", "Help", key(Char('?'))),
                ("F2", "Menu", key(F(2))),
            ],
            EditMode::Insert => vec![
                ("Esc", "Normal", key(Esc)),
                play,
                (",", "Oct-", key(Char(','))),
                ("/", "Oct+", key(Char('/'))),
                ("<", "Inst-", key(Char('<'))),
                (">", "Inst+", key(Char('>'))),
                ("^Z", "Undo", ctrl('z')),
                ("?", "Help", key(Char('?'))),
            ],
            EditMode::Select => vec![
                ("Esc", "Normal", key(Esc)),
                ("Enter", "Toggle", key(Enter)),
                ("x", "Delete", key(Char('x'))),
                ("c", "Clear", key(Char('c'))),
                ("w", "Up", key(Char('w'))),
                ("s", "Down", key(Char('s'))),
                ("a", "Left", key(Char('a'))),
                ("d", "Right", key(Char('d'))),
                ("A", "Shorter", key(Char('A'))),
                ("D", "Longer", key(Char('D'))),
                ("^Z", "Undo", ctrl('z')),
            ],
        }
    }

    /// The status-bar text: the last message plus mode/position/audio info.
    fn status_text(&self) -> String {
        let mode = match self.app.edit_mode {
            EditMode::Normal => "NORMAL",
            EditMode::Insert => "INSERT",
            EditMode::Select => "SELECT",
        };
        let play = match self.app.audio.playback_state() {
            PlaybackState::Playing => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Stopped => "Stopped",
        };
        let audio = match self.app.audio.output_problem() {
            Some(problem) => problem.to_string(),
            None => self
                .app
                .soundfont_path
                .as_deref()
                .map(file_name)
                .unwrap_or_default(),
        };
        let info = format!(
            "{mode} | {play} {} | {} BPM | {} track(s) | {audio}{}",
            self.app.position_string(),
            self.app.project().tempo,
            self.app.project().track_count(),
            if self.app.dirty { " | Modified" } else { "" }
        );
        match &self.app.status_message {
            Some((s, _)) => format!("{s}  —  {info}"),
            None => info,
        }
    }
}

/// F1-F6 open their menus; F10 (the classic menu-bar key) opens File.
fn menu_for_key(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::F(10) => Some(0),
        KeyCode::F(n) => MENU_BAR.iter().position(|m| m.fkey == n),
        _ => None,
    }
}

fn first_selectable(items: &[MenuItem]) -> usize {
    items.iter().position(|i| !i.is_separator()).unwrap_or(0)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string())
}

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// True for files the music app opens (.mid, .midi, .oxm; case-insensitive).
/// (.json is accepted by `open` but not listed, since JSON is too generic.)
pub fn is_midi_file(path: &Path) -> bool {
    has_extension(path, &["mid", "midi", "oxm"])
}

/// What the Open dialog lists: MIDI files plus `.json` projects.
fn is_openable(path: &Path) -> bool {
    has_extension(path, &["mid", "midi", "oxm", "json"])
}

