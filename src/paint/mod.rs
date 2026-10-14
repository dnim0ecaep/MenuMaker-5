//! Paint: a small MS-Paint-style editor for terminal block-character art,
//! ported from Tui-Desktop and drawn DeskMate-style (theme-colored double
//! boxes, a function-key command bar, centered dialogs).
//!
//! The canvas (`canvas::Canvas`) is a fixed-resolution surface; the screen
//! is a scrollable, zoomable viewport onto it (`canvas::Viewport`).
//! Paintings are saved as `.ans` (truecolor ANSI), `.txt`, `.html`, `.svg`
//! or `.irc`, and `.ans`/`.txt` load back in; PNG/JPEG images can be
//! imported as half-block art.
//!
//! Public interface: [`PaintApp`] (full-screen app), [`Picture`] /
//! [`load_picture`] (draw a painting as an icon) and [`is_paintable`].

mod ansi;
mod canvas;
mod export;
mod filedialog;
mod history;
mod icons;
mod palette;
mod selection;
mod strings;
mod tools;
mod ui;

pub use ui::{FilePicker, PickResult};

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use ansi::PaintCellSer;
use canvas::{Canvas, Viewport};
use filedialog::{DialogPurpose, DialogResult, FileDialog};
use history::{CanvasSnapshot, Delta, History};
use palette::{ColorComposer, ColorPalette, RgbColor};
use selection::{Clipboard, Selection};
use tools::{DragState, Interaction, Tool};

const DEFAULT_BRUSHES: [char; 15] = [' ', '█', '▓', '▒', '░', '▀', '▄', '■', '●', '◆', '◇', '#', '*', '.', '+'];

/// A generous default so the viewport has room to scroll.
const DEFAULT_CANVAS: (u16, u16) = (100, 50);

/// What `handle_key`/`handle_mouse` ask the host to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintResult {
    Continue,
    Close,
}

/// What to do once the Save/Discard/Cancel prompt is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsavedAction {
    Close,
    New,
    Open,
    Import,
}

/// The Image Attributes dialog's three sub-modes (`Tab` cycles them; not
/// offered for New Image).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeMode {
    /// `resize_draft` is the new width/height in cells.
    Resize,
    /// `resize_draft` is a horizontal/vertical percentage (10-400).
    Stretch,
    /// Uses `skew_degrees` (signed).
    Skew,
}

impl ResizeMode {
    fn next(self) -> Self {
        match self {
            ResizeMode::Resize => ResizeMode::Stretch,
            ResizeMode::Stretch => ResizeMode::Skew,
            ResizeMode::Skew => ResizeMode::Resize,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ResizeMode::Resize => "Resize",
            ResizeMode::Stretch => "Stretch",
            ResizeMode::Skew => "Skew",
        }
    }
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

#[derive(Clone, Copy)]
enum MenuAction {
    Separator,
    New,
    Open,
    Save,
    SaveAs,
    ImportImage,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    ZoomIn,
    ZoomOut,
    NormalSize,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
    FlipH,
    FlipV,
    RotateCw,
    RotateCcw,
    Rotate180,
    Attributes,
    ClearImage,
    SwapColors,
    AddColor,
    EditColors,
    NextFg,
    PrevFg,
    NextBg,
    PrevBg,
    NextBrush,
    PrevBrush,
    CustomBrush,
    KeyHelp,
    About,
}

const MENU_FILE: &[MenuItem] = &[
    MenuItem::new("New...", "^N", MenuAction::New),
    MenuItem::new("Open...", "L", MenuAction::Open),
    MenuItem::new("Save", "^S", MenuAction::Save),
    MenuItem::new("Save As...", "S", MenuAction::SaveAs),
    MenuItem::sep(),
    MenuItem::new("Import Image...", "", MenuAction::ImportImage),
    MenuItem::sep(),
    MenuItem::new("Exit", "Esc", MenuAction::Exit),
];

const MENU_EDIT: &[MenuItem] = &[
    MenuItem::new("Undo", "Z", MenuAction::Undo),
    MenuItem::new("Redo", "Y", MenuAction::Redo),
    MenuItem::sep(),
    MenuItem::new("Cut", "X", MenuAction::Cut),
    MenuItem::new("Copy", "K", MenuAction::Copy),
    MenuItem::new("Paste", "V", MenuAction::Paste),
    MenuItem::new("Delete", "Del", MenuAction::Delete),
    MenuItem::sep(),
    MenuItem::new("Select All", "^A", MenuAction::SelectAll),
];

const MENU_VIEW: &[MenuItem] = &[
    MenuItem::new("Zoom In", "+", MenuAction::ZoomIn),
    MenuItem::new("Zoom Out", "-", MenuAction::ZoomOut),
    MenuItem::new("Normal Size", "", MenuAction::NormalSize),
    MenuItem::sep(),
    MenuItem::new("Scroll Up", "PgUp", MenuAction::ScrollUp),
    MenuItem::new("Scroll Down", "PgDn", MenuAction::ScrollDown),
    MenuItem::new("Scroll Left", "^←", MenuAction::ScrollLeft),
    MenuItem::new("Scroll Right", "^→", MenuAction::ScrollRight),
];

const MENU_IMAGE: &[MenuItem] = &[
    MenuItem::new("Flip Horizontal", "H", MenuAction::FlipH),
    MenuItem::new("Flip Vertical", "J", MenuAction::FlipV),
    MenuItem::new("Rotate 90\u{b0} CW", "G", MenuAction::RotateCw),
    MenuItem::new("Rotate 90\u{b0} CCW", "N", MenuAction::RotateCcw),
    MenuItem::new("Rotate 180\u{b0}", "M", MenuAction::Rotate180),
    MenuItem::sep(),
    MenuItem::new("Attributes...", "I", MenuAction::Attributes),
    MenuItem::new("Clear Image", "C", MenuAction::ClearImage),
];

const MENU_COLORS: &[MenuItem] = &[
    MenuItem::new("Swap Fg/Bg", "E", MenuAction::SwapColors),
    MenuItem::new("Add Color...", "A", MenuAction::AddColor),
    MenuItem::new("Edit Colors...", "D", MenuAction::EditColors),
    MenuItem::sep(),
    MenuItem::new("Next Fg Color", "]", MenuAction::NextFg),
    MenuItem::new("Prev Fg Color", "[", MenuAction::PrevFg),
    MenuItem::new("Next Bg Color", "}", MenuAction::NextBg),
    MenuItem::new("Prev Bg Color", "{", MenuAction::PrevBg),
    MenuItem::sep(),
    MenuItem::new("Next Brush", ".", MenuAction::NextBrush),
    MenuItem::new("Prev Brush", ",", MenuAction::PrevBrush),
    MenuItem::new("Custom Brush...", "", MenuAction::CustomBrush),
];

const MENU_HELP: &[MenuItem] = &[
    MenuItem::new("Keyboard Shortcuts", "", MenuAction::KeyHelp),
    MenuItem::new("About Paint", "", MenuAction::About),
];

/// A top-level menu and the function key that opens it.
struct Menu {
    label: &'static str,
    fkey: u8,
    items: &'static [MenuItem],
}

/// The DeskMate-style command bar: `File F2  Edit F3  View F4  Image F5
/// Colors F6  Help F1`.
const MENU_BAR: &[Menu] = &[
    Menu { label: "File", fkey: 2, items: MENU_FILE },
    Menu { label: "Edit", fkey: 3, items: MENU_EDIT },
    Menu { label: "View", fkey: 4, items: MENU_VIEW },
    Menu { label: "Image", fkey: 5, items: MENU_IMAGE },
    Menu { label: "Colors", fkey: 6, items: MENU_COLORS },
    Menu { label: "Help", fkey: 1, items: MENU_HELP },
];

const KEY_HELP: &[&str] = &[
    "Tools: 1 Pencil  2 Brush  3 Eraser  4 Color Eraser  5 Airbrush",
    "       6 Fill  7 Pick Color  8 Line  9 Rectangle  0 Round Rect",
    "       o Ellipse  u Curve  p Polygon  r Select  f Free-Form Select",
    "       t Text  q Magnifier",
    "Arrows move the cursor, Space paints/acts, Enter closes a polygon",
    "Mouse: left button = Fg ink, right = Bg ink, wheel scrolls",
    "[ ] Fg color   { } Bg color   , . brush   E swap Fg/Bg",
    "A add color   D edit palette   click Brush to type a brush char",
    "Z undo  Y redo  K copy  X cut  V paste  Del delete  ^A select all",
    "(^Z ^Y ^C ^X ^V also work)   C clear image",
    "+ - zoom   PgUp/PgDn ^Left/^Right scroll",
    "H/J flip   G/N rotate 90°   M rotate 180°   I attributes",
    "^N new   L or ^O open   ^S save   S save as",
    "F2 File  F3 Edit  F4 View  F5 Image  F6 Colors  F1 Help  (F10 = File)",
    "Esc cancels; with nothing in progress it closes Paint",
];

/// The Palette browser ("Edit Colors").
struct PalettePickerState {
    selected: usize,
    editing: Option<ColorComposer>,
}

/// A read-only message box (About / Keyboard Shortcuts); any key closes it.
struct InfoDialog {
    title: &'static str,
    lines: Vec<String>,
}

/// One clickable chip on the shortcut bar: the key it shows and the key
/// event a click synthesizes.
struct Chip {
    key: &'static str,
    label: &'static str,
    event: KeyEvent,
}

impl Chip {
    fn new(key: &'static str, label: &'static str, code: KeyCode) -> Self {
        Self { key, label, event: KeyEvent::new(code, KeyModifiers::NONE) }
    }
    fn ctrl(key: &'static str, label: &'static str, c: char) -> Self {
        Self { key, label, event: KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL) }
    }
}

pub struct PaintApp {
    paintings_dir: PathBuf,
    palette: ColorPalette,
    canvas: Canvas,
    viewport: Viewport,
    /// Canvas-space coordinates.
    cursor: (u16, u16),
    fg_idx: usize,
    bg_idx: usize,
    brushes: Vec<char>,
    brush_idx: usize,
    /// The next keypress becomes the brush character at `brush_idx`.
    awaiting_brush_char: bool,
    /// `true` (left click / keyboard) paints with fg as ink; `false`
    /// (right click) swaps fg/bg for the stroke.
    stroke_primary: bool,
    tool: Tool,
    history: History,
    /// The in-progress gesture's undo delta.
    pending: Option<Delta>,
    last_drag: Option<(u16, u16)>,
    shape: DragState,
    selection: Option<Selection>,
    /// Cells lifted out of the canvas while moving a selection.
    floating: HashMap<(u16, u16), PaintCellSer>,
    clipboard: Option<Clipboard>,
    /// `Some((w, h))` while the Image Attributes / New Image dialog is open.
    resize_draft: Option<(u16, u16)>,
    resize_is_new: bool,
    resize_mode: ResizeMode,
    skew_degrees: (i16, i16),
    /// `Some` while composing a new palette color ("Add Color").
    color_draft: Option<ColorComposer>,
    palette_picker: Option<PalettePickerState>,
    /// Swatches per row in the last-rendered Palette browser.
    palette_grid_cols: u16,
    rng_seed: u64,
    status: Option<String>,
    save_counter: u32,
    dirty: bool,
    current_path: Option<PathBuf>,
    /// `Some` while the Save As dialog is open (the typed name).
    save_as_draft: Option<String>,
    /// The Save As name is still the suggested one, so the first typed
    /// character replaces it instead of appending.
    save_as_suggested: bool,
    /// `Some(path)` while confirming an overwrite.
    overwrite_confirm: Option<PathBuf>,
    pending_unsaved_action: Option<UnsavedAction>,
    /// Carried through a Save As started from the unsaved-changes prompt,
    /// so the guarded action runs once the save succeeds.
    save_then: Option<UnsavedAction>,
    file_dialog: Option<FileDialog>,
    /// Folder the last image was imported from.
    import_dir: Option<PathBuf>,
    info: Option<InfoDialog>,
    menu_open: Option<usize>,
    menu_selected: usize,
    /// Visible canvas region (canvas cells) from the last render.
    last_visible: (u16, u16),
    /// A canvas gesture began with a mouse-down in the viewport, so drags
    /// and the release go to the canvas even if the pointer leaves it.
    mouse_capture: bool,
    /// Hit-test rectangles from the last render.
    layout: ui::LayoutCache,
}

impl PaintApp {
    /// Blank canvas; Save/Open default to `paintings_dir` (created if
    /// missing).
    pub fn new(paintings_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&paintings_dir);
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            | 1;
        let palette = ColorPalette::load(Some(paintings_dir.join(".paint-palette.json")));
        Self {
            paintings_dir,
            palette,
            canvas: Canvas::new(DEFAULT_CANVAS.0, DEFAULT_CANVAS.1),
            viewport: Viewport::new(),
            cursor: (0, 0),
            fg_idx: 7,
            bg_idx: 0,
            brushes: DEFAULT_BRUSHES.to_vec(),
            brush_idx: 1,
            awaiting_brush_char: false,
            stroke_primary: true,
            tool: Tool::Pencil,
            history: History::new(),
            pending: None,
            last_drag: None,
            shape: DragState::None,
            selection: None,
            floating: HashMap::new(),
            clipboard: None,
            resize_draft: None,
            resize_is_new: false,
            resize_mode: ResizeMode::Resize,
            skew_degrees: (0, 0),
            color_draft: None,
            palette_picker: None,
            palette_grid_cols: 1,
            rng_seed: seed,
            status: None,
            save_counter: 0,
            dirty: false,
            current_path: None,
            save_as_draft: None,
            save_as_suggested: false,
            overwrite_confirm: None,
            pending_unsaved_action: None,
            save_then: None,
            file_dialog: None,
            import_dir: None,
            info: None,
            menu_open: None,
            menu_selected: 0,
            last_visible: (0, 0),
            mouse_capture: false,
            layout: ui::LayoutCache::default(),
        }
    }

    /// Opens `file`: `.ans`/`.txt` load as the painting (Save writes back to
    /// it; a missing `.ans`/`.txt` becomes the save target of a blank
    /// canvas); images are imported onto a new canvas.
    pub fn open(paintings_dir: PathBuf, file: &Path) -> Self {
        let mut app = Self::new(paintings_dir);
        if is_image(file) {
            app.import_image(file);
        } else if !file.exists() && is_paintable(file) {
            app.current_path = Some(file.to_path_buf());
            app.status = Some(format!("New painting {}", file_name(file)));
        } else {
            app.load_file(file);
            if !is_paintable(file) && app.current_path.is_some() {
                // Never write paint data back over e.g. a .md document:
                // keep the picture but make saving go through Save As.
                app.current_path = None;
                app.status = Some(format!("Opened {} as a new drawing (Save As to keep it)", file_name(file)));
            }
        }
        app
    }

    fn file_label(&self) -> String {
        match &self.current_path {
            Some(p) => file_name(p),
            None => "Untitled".into(),
        }
    }

    fn is_modal(&self) -> bool {
        self.info.is_some()
            || self.file_dialog.is_some()
            || self.awaiting_brush_char
            || self.palette_picker.is_some()
            || self.pending_unsaved_action.is_some()
            || self.save_as_draft.is_some()
            || self.overwrite_confirm.is_some()
            || self.resize_draft.is_some()
            || self.color_draft.is_some()
    }

    // ----- colors and brushes -----

    fn cycle_fg(&mut self, dir: i32) {
        self.fg_idx = cycle(self.fg_idx, self.palette.len(), dir);
    }

    fn cycle_bg(&mut self, dir: i32) {
        self.bg_idx = cycle(self.bg_idx, self.palette.len(), dir);
    }

    fn cycle_brush(&mut self, dir: i32) {
        self.brush_idx = cycle(self.brush_idx, self.brushes.len(), dir);
    }

    fn begin_brush_char_capture(&mut self) {
        self.awaiting_brush_char = true;
        self.status = Some("Type a character to use as the brush".into());
    }

    fn handle_brush_char_key(&mut self, key: KeyEvent) {
        self.awaiting_brush_char = false;
        match key.code {
            KeyCode::Char(c) => self.set_custom_brush(c),
            KeyCode::Esc => self.status = Some("Cancelled".into()),
            _ => {}
        }
    }

    /// Overwrites the selected brush slot in place, so `,`/`.` keep
    /// cycling the same rotation.
    fn set_custom_brush(&mut self, ch: char) {
        if let Some(slot) = self.brushes.get_mut(self.brush_idx) {
            *slot = ch;
        }
        self.status = Some(format!("Brush: '{ch}'"));
    }

    fn swap_colors(&mut self) {
        std::mem::swap(&mut self.fg_idx, &mut self.bg_idx);
        self.status = Some("Swapped Fg/Bg".into());
    }

    // ----- whole-canvas transforms (each one undo step) -----

    fn clear_canvas(&mut self) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        self.canvas.clear();
        self.mark_dirty();
        self.status = Some("Canvas cleared".into());
    }

    fn flip_horizontal(&mut self) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let w = self.canvas.width;
        let cells = std::mem::take(&mut self.canvas.cells);
        self.canvas.cells = cells.into_iter().map(|((x, y), c)| ((w - 1 - x, y), c)).collect();
        self.mark_dirty();
        self.status = Some("Flipped horizontally".into());
    }

    fn flip_vertical(&mut self) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let h = self.canvas.height;
        let cells = std::mem::take(&mut self.canvas.cells);
        self.canvas.cells = cells.into_iter().map(|((x, y), c)| ((x, h - 1 - y), c)).collect();
        self.mark_dirty();
        self.status = Some("Flipped vertically".into());
    }

    fn rotate_90_cw(&mut self) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let (w, h) = (self.canvas.width, self.canvas.height);
        let cells = std::mem::take(&mut self.canvas.cells);
        self.canvas.cells = cells.into_iter().map(|((x, y), c)| ((h - 1 - y, x), c)).collect();
        self.canvas.width = h;
        self.canvas.height = w;
        self.clamp_cursor_to_canvas();
        self.mark_dirty();
        self.status = Some("Rotated 90° CW".into());
    }

    fn rotate_90_ccw(&mut self) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let (w, h) = (self.canvas.width, self.canvas.height);
        let cells = std::mem::take(&mut self.canvas.cells);
        self.canvas.cells = cells.into_iter().map(|((x, y), c)| ((y, w - 1 - x), c)).collect();
        self.canvas.width = h;
        self.canvas.height = w;
        self.clamp_cursor_to_canvas();
        self.mark_dirty();
        self.status = Some("Rotated 90° CCW".into());
    }

    fn rotate_180(&mut self) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let (w, h) = (self.canvas.width, self.canvas.height);
        let cells = std::mem::take(&mut self.canvas.cells);
        self.canvas.cells = cells.into_iter().map(|((x, y), c)| ((w - 1 - x, h - 1 - y), c)).collect();
        self.mark_dirty();
        self.status = Some("Rotated 180°".into());
    }

    fn clamp_cursor_to_canvas(&mut self) {
        self.cursor.0 = self.cursor.0.min(self.canvas.width.saturating_sub(1));
        self.cursor.1 = self.cursor.1.min(self.canvas.height.saturating_sub(1));
    }

    // ----- menus -----

    fn open_menu(&mut self, idx: usize) {
        // A text edit in progress is committed, like clicking away from it.
        if matches!(self.shape, DragState::Text { .. }) {
            self.end_gesture();
            self.shape = DragState::None;
        }
        self.menu_open = Some(idx);
        self.menu_selected = first_selectable(MENU_BAR[idx].items);
    }

    fn close_menu(&mut self) {
        self.menu_open = None;
    }

    fn menu_move_selection(&mut self, dir: i32) {
        let Some(idx) = self.menu_open else { return };
        let items = MENU_BAR[idx].items;
        if items.is_empty() {
            return;
        }
        let mut sel = self.menu_selected as i32;
        for _ in 0..items.len() {
            sel = (sel + dir).rem_euclid(items.len() as i32);
            if !items[sel as usize].is_separator() {
                break;
            }
        }
        self.menu_selected = sel as usize;
    }

    fn menu_switch(&mut self, dir: i32) {
        let Some(idx) = self.menu_open else { return };
        let new_idx = (idx as i32 + dir).rem_euclid(MENU_BAR.len() as i32) as usize;
        self.open_menu(new_idx);
    }

    fn handle_menu_key(&mut self, key: KeyEvent) -> PaintResult {
        if let Some(idx) = menu_for_key(key.code) {
            if self.menu_open == Some(idx) {
                self.close_menu();
            } else {
                self.open_menu(idx);
            }
            return PaintResult::Continue;
        }
        match key.code {
            KeyCode::Left => self.menu_switch(-1),
            KeyCode::Right => self.menu_switch(1),
            KeyCode::Up => self.menu_move_selection(-1),
            KeyCode::Down => self.menu_move_selection(1),
            KeyCode::Home => {
                if let Some(idx) = self.menu_open {
                    self.menu_selected = first_selectable(MENU_BAR[idx].items);
                }
            }
            KeyCode::End => {
                if let Some(idx) = self.menu_open {
                    let items = MENU_BAR[idx].items;
                    self.menu_selected = items.iter().rposition(|i| !i.is_separator()).unwrap_or(0);
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
        PaintResult::Continue
    }

    /// Runs a dropdown item and closes the menu. Every arm calls the same
    /// method as the item's keyboard shortcut.
    fn activate_menu_item(&mut self, menu_idx: usize, item_idx: usize) -> PaintResult {
        self.menu_open = None;
        let Some(menu) = MENU_BAR.get(menu_idx) else { return PaintResult::Continue };
        let Some(item) = menu.items.get(item_idx) else { return PaintResult::Continue };
        match item.action {
            MenuAction::Separator => {}
            MenuAction::New => self.request_new(),
            MenuAction::Open => self.request_open(UnsavedAction::Open),
            MenuAction::Save => return self.save(),
            MenuAction::SaveAs => self.begin_save_as(),
            MenuAction::ImportImage => self.request_open(UnsavedAction::Import),
            MenuAction::Exit => return self.request_close(),
            MenuAction::Undo => self.undo(),
            MenuAction::Redo => self.redo(),
            MenuAction::Cut => self.copy_selection(true),
            MenuAction::Copy => self.copy_selection(false),
            MenuAction::Paste => self.paste(),
            MenuAction::Delete => self.delete_selection(),
            MenuAction::SelectAll => self.select_all(),
            MenuAction::ZoomIn => self.zoom_by(true),
            MenuAction::ZoomOut => self.zoom_by(false),
            MenuAction::NormalSize => {
                self.viewport.zoom = 1;
                self.status = Some("Zoom: 100%".into());
            }
            MenuAction::ScrollUp => self.scroll_page(0, -1),
            MenuAction::ScrollDown => self.scroll_page(0, 1),
            MenuAction::ScrollLeft => self.scroll_page(-1, 0),
            MenuAction::ScrollRight => self.scroll_page(1, 0),
            MenuAction::FlipH => self.flip_horizontal(),
            MenuAction::FlipV => self.flip_vertical(),
            MenuAction::RotateCw => self.rotate_90_cw(),
            MenuAction::RotateCcw => self.rotate_90_ccw(),
            MenuAction::Rotate180 => self.rotate_180(),
            MenuAction::Attributes => self.begin_resize(),
            MenuAction::ClearImage => self.clear_canvas(),
            MenuAction::SwapColors => self.swap_colors(),
            MenuAction::AddColor => self.begin_add_color(),
            MenuAction::EditColors => self.begin_palette_picker(),
            MenuAction::NextFg => self.cycle_fg(1),
            MenuAction::PrevFg => self.cycle_fg(-1),
            MenuAction::NextBg => self.cycle_bg(1),
            MenuAction::PrevBg => self.cycle_bg(-1),
            MenuAction::NextBrush => self.cycle_brush(1),
            MenuAction::PrevBrush => self.cycle_brush(-1),
            MenuAction::CustomBrush => self.begin_brush_char_capture(),
            MenuAction::KeyHelp => {
                self.info = Some(InfoDialog {
                    title: "Keyboard Shortcuts",
                    lines: KEY_HELP.iter().map(|s| s.to_string()).collect(),
                })
            }
            MenuAction::About => {
                self.info = Some(InfoDialog {
                    title: "About Paint",
                    lines: vec![
                        "Paint".into(),
                        String::new(),
                        "A tiny MS-Paint-style editor for block-character art.".into(),
                        "Saves .ans .txt .html .svg .irc; opens .ans .txt;".into(),
                        "imports PNG and JPEG images.".into(),
                    ],
                })
            }
        }
        PaintResult::Continue
    }

    // ----- Image Attributes / New Image -----

    fn begin_resize(&mut self) {
        self.resize_draft = Some((self.canvas.width, self.canvas.height));
        self.resize_is_new = false;
        self.resize_mode = ResizeMode::Resize;
        self.status = Some(self.resize_status_text());
    }

    fn cycle_resize_mode(&mut self) {
        self.resize_mode = self.resize_mode.next();
        match self.resize_mode {
            ResizeMode::Resize => self.resize_draft = Some((self.canvas.width, self.canvas.height)),
            ResizeMode::Stretch => self.resize_draft = Some((100, 100)),
            ResizeMode::Skew => {
                self.resize_draft = Some((0, 0));
                self.skew_degrees = (0, 0);
            }
        }
        self.status = Some(self.resize_status_text());
    }

    fn resize_status_text(&self) -> String {
        match self.resize_mode {
            ResizeMode::Skew => {
                let (sx, sy) = self.skew_degrees;
                format!("Skew: {sx}° horiz, {sy}° vert")
            }
            ResizeMode::Stretch => {
                let (w, h) = self.resize_draft.unwrap_or((100, 100));
                format!("Stretch: {w}% x, {h}% y")
            }
            ResizeMode::Resize => {
                let (w, h) = self.resize_draft.unwrap_or((self.canvas.width, self.canvas.height));
                let verb = if self.resize_is_new { "New Image" } else { "Image Attributes" };
                format!("{verb}: {w}x{h}")
            }
        }
    }

    /// Resizes the canvas in place (dropping cells outside it) or, for New
    /// Image, replaces it with a blank one and forgets `current_path`.
    fn apply_resize_draft(&mut self, w: u16, h: u16) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        if self.resize_is_new {
            self.canvas = Canvas::new(w, h);
            self.current_path = None;
            self.cursor = (0, 0);
            self.selection = None;
            self.status = Some(format!("New {w}x{h} image"));
            self.clamp_cursor_to_canvas();
            // A brand-new blank image has nothing to save yet.
            self.dirty = false;
            return;
        }
        self.canvas.width = w;
        self.canvas.height = h;
        self.canvas.cells.retain(|&(x, y), _| x < w && y < h);
        self.status = Some(format!("Canvas resized to {w}x{h}"));
        self.clamp_cursor_to_canvas();
        self.mark_dirty();
    }

    /// Nearest-neighbor resamples the content to `w_pct`/`h_pct` percent.
    fn apply_stretch(&mut self, w_pct: u16, h_pct: u16) {
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let old_w = self.canvas.width.max(1) as u32;
        let old_h = self.canvas.height.max(1) as u32;
        let new_w = ((old_w * w_pct as u32) / 100).clamp(1, u16::MAX as u32) as u16;
        let new_h = ((old_h * h_pct as u32) / 100).clamp(1, u16::MAX as u32) as u16;
        let mut new_cells = HashMap::new();
        for ny in 0..new_h {
            let oy = ((ny as u32 * old_h) / new_h as u32).min(old_h - 1) as u16;
            for nx in 0..new_w {
                let ox = ((nx as u32 * old_w) / new_w as u32).min(old_w - 1) as u16;
                if let Some(cell) = self.canvas.get(ox, oy) {
                    new_cells.insert((nx, ny), cell);
                }
            }
        }
        self.canvas.width = new_w;
        self.canvas.height = new_h;
        self.canvas.cells = new_cells;
        self.clamp_cursor_to_canvas();
        self.mark_dirty();
        self.status = Some(format!("Stretched to {new_w}x{new_h}"));
    }

    /// Shears rows/columns by `tan(angle)`, growing the canvas to fit.
    fn apply_skew(&mut self, sx_deg: i16, sy_deg: i16) {
        if sx_deg == 0 && sy_deg == 0 {
            self.status = Some("Skew: 0°, nothing to do".into());
            return;
        }
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        let tan_x = (sx_deg as f64).to_radians().tan();
        let tan_y = (sy_deg as f64).to_radians().tan();
        let old_cells = std::mem::take(&mut self.canvas.cells);
        let shifted: Vec<((i32, i32), PaintCellSer)> = old_cells
            .into_iter()
            .map(|((x, y), cell)| {
                let dx = (y as f64 * tan_x).round() as i32;
                let dy = (x as f64 * tan_y).round() as i32;
                ((x as i32 + dx, y as i32 + dy), cell)
            })
            .collect();
        let min_x = shifted.iter().map(|((x, _), _)| *x).min().unwrap_or(0);
        let min_y = shifted.iter().map(|((_, y), _)| *y).min().unwrap_or(0);
        let max_x = shifted.iter().map(|((x, _), _)| *x).max().unwrap_or(0);
        let max_y = shifted.iter().map(|((_, y), _)| *y).max().unwrap_or(0);
        let new_w = (max_x - min_x + 1).clamp(1, u16::MAX as i32) as u16;
        let new_h = (max_y - min_y + 1).clamp(1, u16::MAX as i32) as u16;
        let new_cells = shifted
            .into_iter()
            .filter(|((x, y), _)| x - min_x < new_w as i32 && y - min_y < new_h as i32)
            .map(|((x, y), cell)| (((x - min_x) as u16, (y - min_y) as u16), cell))
            .collect();
        self.canvas.width = new_w;
        self.canvas.height = new_h;
        self.canvas.cells = new_cells;
        self.clamp_cursor_to_canvas();
        self.mark_dirty();
        self.status = Some(format!("Skewed {sx_deg}°/{sy_deg}°, canvas now {new_w}x{new_h}"));
    }

    fn handle_resize_key(&mut self, key: KeyEvent) {
        if !self.resize_is_new && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.cycle_resize_mode();
            return;
        }
        if !self.resize_is_new && self.resize_mode == ResizeMode::Skew {
            self.handle_skew_key(key);
            return;
        }
        let Some((mut w, mut h)) = self.resize_draft else { return };
        let stretch = self.resize_mode == ResizeMode::Stretch && !self.resize_is_new;
        let (min_v, max_v, step) = if stretch { (10, 400, 5) } else { (1, 2000, 1) };
        // Shift+arrow moves in bigger steps.
        let step = if key.modifiers.contains(KeyModifiers::SHIFT) { step * 10 } else { step };
        match key.code {
            KeyCode::Up => h = h.saturating_sub(step).max(min_v),
            KeyCode::Down => h = (h + step).min(max_v),
            KeyCode::Left => w = w.saturating_sub(step).max(min_v),
            KeyCode::Right => w = (w + step).min(max_v),
            KeyCode::Enter => {
                self.resize_draft = None;
                if stretch {
                    self.apply_stretch(w, h);
                } else {
                    self.apply_resize_draft(w, h);
                }
                return;
            }
            KeyCode::Esc => {
                self.resize_draft = None;
                self.status = Some("Cancelled".into());
                return;
            }
            _ => {}
        }
        self.resize_draft = Some((w, h));
        self.status = Some(self.resize_status_text());
    }

    fn handle_skew_key(&mut self, key: KeyEvent) {
        let (mut sx, mut sy) = self.skew_degrees;
        match key.code {
            KeyCode::Up => sy = (sy - 1).max(-89),
            KeyCode::Down => sy = (sy + 1).min(89),
            KeyCode::Left => sx = (sx - 1).max(-89),
            KeyCode::Right => sx = (sx + 1).min(89),
            KeyCode::Enter => {
                self.resize_draft = None;
                self.apply_skew(sx, sy);
                return;
            }
            KeyCode::Esc => {
                self.resize_draft = None;
                self.status = Some("Cancelled".into());
                return;
            }
            _ => {}
        }
        self.skew_degrees = (sx, sy);
        self.status = Some(self.resize_status_text());
    }

    // ----- Add Color / Edit Colors -----

    fn begin_add_color(&mut self) {
        self.color_draft = Some(ColorComposer::new(self.palette.color_at(self.fg_idx)));
        self.status = Some("Add Color: ←/→ adjust, Tab channel, # hex, Enter adds".into());
    }

    /// Shared by Add Color and the Palette browser's edit mode. Returns
    /// `Some(true)` for "apply", `Some(false)` for "cancel".
    fn composer_key(composer: &mut ColorComposer, key: KeyEvent) -> Option<bool> {
        if composer.hex_input.is_some() {
            match key.code {
                KeyCode::Char(c) => composer.push_hex_char(c),
                KeyCode::Backspace => composer.backspace_hex(),
                KeyCode::Enter => composer.commit_hex(),
                KeyCode::Esc => composer.cancel_hex_input(),
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Tab => composer.cycle_channel(),
            KeyCode::BackTab => {
                composer.cycle_channel();
                composer.cycle_channel();
            }
            KeyCode::Left => composer.adjust(-1),
            KeyCode::Right => composer.adjust(1),
            KeyCode::PageDown => composer.adjust(-10),
            KeyCode::PageUp => composer.adjust(10),
            KeyCode::Char('#') => composer.begin_hex_input(),
            KeyCode::Enter => return Some(true),
            KeyCode::Esc => return Some(false),
            _ => {}
        }
        None
    }

    fn handle_color_key(&mut self, key: KeyEvent) {
        let Some(draft) = &mut self.color_draft else { return };
        match Self::composer_key(draft, key) {
            Some(true) => {
                let color = draft.color;
                self.color_draft = None;
                self.fg_idx = self.palette.add_color(color);
                self.status = Some(format!("Added #{:02X}{:02X}{:02X} to the palette", color.0, color.1, color.2));
            }
            Some(false) => {
                self.color_draft = None;
                self.status = Some("Cancelled".into());
            }
            None => self.status = Some(draft.status()),
        }
    }

    fn begin_palette_picker(&mut self) {
        let selected = self.fg_idx.min(self.palette.len().saturating_sub(1));
        self.palette_picker = Some(PalettePickerState { selected, editing: None });
        self.status = Some("Palette: arrows browse, Enter edits, F/B pick Fg/Bg, Esc closes".into());
    }

    fn begin_palette_edit(&mut self) {
        let Some(picker) = &mut self.palette_picker else { return };
        picker.editing = Some(ColorComposer::new(self.palette.color_at(picker.selected)));
        self.status = Some("Editing: ←/→ adjust, Tab channel, # hex, Enter applies".into());
    }

    fn palette_move_selection(&mut self, delta: i32) {
        let len = self.palette.len() as i32;
        let Some(picker) = &mut self.palette_picker else { return };
        if len == 0 {
            return;
        }
        picker.selected = (picker.selected as i32 + delta).clamp(0, len - 1) as usize;
    }

    fn handle_palette_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.palette_picker else { return };
        if let Some(editing) = &mut picker.editing {
            match Self::composer_key(editing, key) {
                Some(true) => {
                    let (selected, color) = (picker.selected, editing.color);
                    picker.editing = None;
                    self.palette.set_color_at(selected, color);
                    self.status = Some(format!("Updated swatch {selected} to #{:02X}{:02X}{:02X}", color.0, color.1, color.2));
                }
                Some(false) => {
                    picker.editing = None;
                    self.status = Some("Palette: arrows browse, Enter edits, F/B pick Fg/Bg, Esc closes".into());
                }
                None => self.status = Some(editing.status()),
            }
            return;
        }
        let cols = self.palette_grid_cols.max(1) as i32;
        let selected = picker.selected;
        match key.code {
            KeyCode::Left => self.palette_move_selection(-1),
            KeyCode::Right => self.palette_move_selection(1),
            KeyCode::Up => self.palette_move_selection(-cols),
            KeyCode::Down => self.palette_move_selection(cols),
            KeyCode::Enter => self.begin_palette_edit(),
            KeyCode::Char('f') | KeyCode::Char('F') => {
                self.fg_idx = selected;
                self.status = Some(format!("Fg color: swatch {selected}"));
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.bg_idx = selected;
                self.status = Some(format!("Bg color: swatch {selected}"));
            }
            KeyCode::Esc => {
                self.palette_picker = None;
                self.status = None;
            }
            _ => {}
        }
    }

    // ----- gestures and painting -----

    fn begin_gesture(&mut self) {
        self.pending = Some(Delta::new());
    }

    fn end_gesture(&mut self) {
        if let Some(delta) = self.pending.take() {
            let touched = !delta.is_empty();
            self.history.push(delta);
            if touched {
                self.mark_dirty();
            }
        }
    }

    fn undo(&mut self) {
        self.abort_gesture();
        self.status = Some(if self.history.undo(&mut self.canvas) { "Undo".into() } else { "Nothing to undo".into() });
        self.clamp_cursor_to_canvas();
    }

    fn redo(&mut self) {
        self.abort_gesture();
        self.status = Some(if self.history.redo(&mut self.canvas) { "Redo".into() } else { "Nothing to redo".into() });
        self.clamp_cursor_to_canvas();
    }

    /// Sets a cell, recording its prior value into the in-progress gesture.
    fn paint_cell(&mut self, x: u16, y: u16, cell: Option<PaintCellSer>) {
        if !self.canvas.in_bounds(x, y) {
            return;
        }
        if let Some(pending) = &mut self.pending {
            pending.record(&self.canvas, x, y);
        }
        self.canvas.set(x, y, cell);
        self.dirty = true;
    }

    /// Flags unsaved changes and, once a file is open, writes a `<path>~`
    /// crash-recovery backup next to it (best effort).
    fn mark_dirty(&mut self) {
        self.dirty = true;
        if let Some(path) = &self.current_path {
            let backup = path.with_extension(format!("{}~", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
            let _ = export::save(&self.canvas, &backup);
        }
    }

    /// The fg/bg colors the current stroke paints with.
    fn stroke_colors(&self) -> (RgbColor, RgbColor) {
        let (fg_idx, bg_idx) = if self.stroke_primary { (self.fg_idx, self.bg_idx) } else { (self.bg_idx, self.fg_idx) };
        (self.palette.color_at(fg_idx), self.palette.color_at(bg_idx))
    }

    fn brush_cell(&self) -> Option<PaintCellSer> {
        let (fg, bg) = self.stroke_colors();
        let ch = self.brushes[self.brush_idx];
        if ch == ' ' {
            None
        } else {
            Some(PaintCellSer { ch, fg, bg })
        }
    }

    /// Applies a freehand or one-shot tool at one canvas cell.
    fn apply_tool(&mut self, x: u16, y: u16) {
        if !self.canvas.in_bounds(x, y) {
            return;
        }
        let (fg, bg) = self.stroke_colors();
        let brush_ch = self.brushes[self.brush_idx];
        match self.tool {
            Tool::Pencil => self.paint_cell(x, y, Some(PaintCellSer { ch: '█', fg, bg })),
            Tool::Brush => {
                let cell = self.brush_cell();
                self.paint_cell(x, y, cell);
            }
            Tool::Eraser => self.paint_cell(x, y, None),
            Tool::ColorEraser => {
                if self.canvas.get(x, y).map(|c| c.fg) == Some(fg) {
                    self.paint_cell(x, y, None);
                }
            }
            Tool::Airbrush => {
                self.paint_cell(x, y, Some(PaintCellSer { ch: brush_ch, fg, bg }));
                for (dx, dy) in [(-1i32, -1i32), (1, -1), (-1, 1), (1, 1), (2, 0), (-2, 0), (0, 2), (0, -2)] {
                    if tools::xorshift(&mut self.rng_seed) < 0.35 {
                        let nx = (x as i32 + dx).clamp(0, self.canvas.width.saturating_sub(1) as i32) as u16;
                        let ny = (y as i32 + dy).clamp(0, self.canvas.height.saturating_sub(1) as i32) as u16;
                        self.paint_cell(nx, ny, Some(PaintCellSer { ch: brush_ch, fg, bg }));
                    }
                }
            }
            Tool::Fill => {
                let mut delta = Delta::new();
                let replacement = self.brush_cell();
                tools::flood_fill(&mut self.canvas, &mut delta, x, y, replacement);
                let touched = !delta.is_empty();
                self.history.push(delta);
                if touched {
                    self.mark_dirty();
                }
            }
            Tool::Picker => {
                if let Some(cell) = self.canvas.get(x, y) {
                    let idx = self.palette.nearest_index(cell.fg);
                    if self.stroke_primary {
                        self.fg_idx = idx;
                    } else {
                        self.bg_idx = idx;
                    }
                    self.status = Some(format!("Picked swatch {idx}"));
                }
            }
            Tool::Line
            | Tool::Rect
            | Tool::RoundRect
            | Tool::Ellipse
            | Tool::Curve
            | Tool::Polygon
            | Tool::SelectRect
            | Tool::SelectFree
            | Tool::Text
            | Tool::Zoom => {}
        }
    }

    fn shape_cells(&self, start: (u16, u16), end: (u16, u16)) -> Vec<(u16, u16)> {
        match self.tool {
            Tool::Line => tools::line_cells(start.0, start.1, end.0, end.1),
            Tool::Rect => tools::rect_outline_cells(start.0, start.1, end.0, end.1),
            Tool::RoundRect => tools::rounded_rect_outline_cells(start.0, start.1, end.0, end.1),
            Tool::Ellipse => tools::ellipse_cells(start.0, start.1, end.0, end.1),
            _ => Vec::new(),
        }
    }

    /// Paints `cells` with the current brush as one undo step.
    fn commit_cells(&mut self, cells: &[(u16, u16)]) {
        let cell = self.brush_cell();
        self.begin_gesture();
        for &(x, y) in cells {
            self.paint_cell(x, y, cell.clone());
        }
        self.end_gesture();
    }

    /// The live overlay for an in-progress shape/curve/polygon/select.
    fn preview_cells(&self) -> Vec<(u16, u16)> {
        match &self.shape {
            DragState::None => Vec::new(),
            DragState::Shape { start } => self.shape_cells(*start, self.cursor),
            DragState::CurveLine { start } => tools::line_cells(start.0, start.1, self.cursor.0, self.cursor.1),
            DragState::CurveBend { start, end } => tools::quad_bezier_cells(*start, self.cursor, *end),
            DragState::Polygon { points } | DragState::Lasso { points } => {
                let mut cells = Vec::new();
                for w in points.windows(2) {
                    cells.extend(tools::line_cells(w[0].0, w[0].1, w[1].0, w[1].1));
                }
                if let Some(&last) = points.last() {
                    cells.extend(tools::line_cells(last.0, last.1, self.cursor.0, self.cursor.1));
                }
                cells
            }
            DragState::Marquee { start } => tools::rect_outline_cells(start.0, start.1, self.cursor.0, self.cursor.1),
            DragState::MovingFloating { .. } | DragState::Text { .. } => Vec::new(),
        }
    }

    /// The keyboard equivalent of a click at the cursor.
    fn act(&mut self) {
        self.stroke_primary = true;
        let (x, y) = self.cursor;
        match self.tool.interaction() {
            Interaction::Freehand => {
                self.begin_gesture();
                self.apply_tool(x, y);
                self.end_gesture();
            }
            Interaction::OneShot => self.apply_tool(x, y),
            Interaction::Shape => {
                self.shape = match std::mem::take(&mut self.shape) {
                    DragState::Shape { start } => {
                        let cells = self.shape_cells(start, (x, y));
                        self.commit_cells(&cells);
                        DragState::None
                    }
                    _ => DragState::Shape { start: (x, y) },
                };
            }
            Interaction::Curve => {
                self.shape = match std::mem::take(&mut self.shape) {
                    DragState::None => DragState::CurveLine { start: (x, y) },
                    DragState::CurveLine { start } => DragState::CurveBend { start, end: (x, y) },
                    DragState::CurveBend { start, end } => {
                        let cells = tools::quad_bezier_cells(start, (x, y), end);
                        self.commit_cells(&cells);
                        DragState::None
                    }
                    other => other,
                };
            }
            Interaction::Polygon => {
                let mut points = match std::mem::take(&mut self.shape) {
                    DragState::Polygon { points } => points,
                    _ => Vec::new(),
                };
                points.push((x, y));
                self.shape = DragState::Polygon { points };
            }
            Interaction::Select => {
                let inside = self.selection.as_ref().is_some_and(|s| s.contains(x, y));
                match std::mem::take(&mut self.shape) {
                    DragState::MovingFloating { anchor } => self.commit_move(anchor, (x, y)),
                    DragState::Marquee { start } => self.selection = Some(Selection::rect(start, (x, y))),
                    DragState::Lasso { mut points } => {
                        points.push((x, y));
                        self.shape = DragState::Lasso { points };
                    }
                    DragState::None if inside => self.lift_selection((x, y)),
                    DragState::None => {
                        self.selection = None;
                        self.shape = match self.tool {
                            Tool::SelectFree => DragState::Lasso { points: vec![(x, y)] },
                            _ => DragState::Marquee { start: (x, y) },
                        };
                    }
                    other => self.shape = other,
                }
            }
            Interaction::Text => {
                if matches!(self.shape, DragState::Text { .. }) {
                    self.end_gesture();
                    self.shape = DragState::None;
                } else {
                    self.begin_gesture();
                    self.shape = DragState::Text { origin: (x, y) };
                }
            }
            Interaction::Zoom => self.zoom_at((x, y), true),
        }
    }

    fn zoom_at(&mut self, (x, y): (u16, u16), zoom_in: bool) {
        if zoom_in {
            self.viewport.zoom_in();
        } else {
            self.viewport.zoom_out();
        }
        if self.last_visible != (0, 0) {
            self.viewport.ensure_visible(x, y, self.last_visible);
        }
        self.status = Some(format!("Zoom: {}%", self.viewport.zoom_percent()));
    }

    fn zoom_by(&mut self, zoom_in: bool) {
        if zoom_in {
            self.viewport.zoom_in();
        } else {
            self.viewport.zoom_out();
        }
        self.status = Some(format!("Zoom: {}%", self.viewport.zoom_percent()));
    }

    fn close_polygon(&mut self) {
        let DragState::Polygon { points } = std::mem::take(&mut self.shape) else { return };
        if points.len() < 2 {
            return;
        }
        let mut cells = Vec::new();
        for w in points.windows(2) {
            cells.extend(tools::line_cells(w[0].0, w[0].1, w[1].0, w[1].1));
        }
        let first = points[0];
        let last = points[points.len() - 1];
        cells.extend(tools::line_cells(last.0, last.1, first.0, first.1));
        self.commit_cells(&cells);
    }

    fn close_lasso(&mut self) {
        let DragState::Lasso { points } = std::mem::take(&mut self.shape) else { return };
        self.finish_lasso(points);
    }

    /// Closes the traced loop, rasterizes it into a mask and selects it.
    fn finish_lasso(&mut self, mut points: Vec<(u16, u16)>) {
        if points.len() < 3 {
            return;
        }
        let first = points[0];
        let last = points[points.len() - 1];
        points.extend(tools::line_cells(last.0, last.1, first.0, first.1));
        let mask = selection::lasso_mask(&points);
        let (Some(x0), Some(x1), Some(y0), Some(y1)) = (
            mask.iter().map(|(x, _)| *x).min(),
            mask.iter().map(|(x, _)| *x).max(),
            mask.iter().map(|(_, y)| *y).min(),
            mask.iter().map(|(_, y)| *y).max(),
        ) else {
            return;
        };
        self.selection = Some(Selection { x0, y0, x1, y1, mask: Some(mask) });
        self.status = Some("Selected".into());
    }

    /// Resets any in-progress gesture, reverting uncommitted changes,
    /// without touching a completed selection.
    fn abort_gesture(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.revert(&mut self.canvas);
        }
        self.floating.clear();
        self.shape = DragState::None;
        self.last_drag = None;
        self.mouse_capture = false;
    }

    /// Esc on the canvas: cancel a gesture, else drop the selection.
    /// Returns `false` when there was nothing to cancel.
    fn cancel_gesture(&mut self) -> bool {
        let had_gesture = self.shape != DragState::None || self.pending.is_some();
        self.abort_gesture();
        if had_gesture {
            self.status = Some("Cancelled".into());
            true
        } else if self.selection.take().is_some() {
            self.status = Some("Deselected".into());
            true
        } else {
            false
        }
    }

    fn lift_selection(&mut self, anchor: (u16, u16)) {
        let Some(sel) = self.selection.clone() else { return };
        self.begin_gesture();
        self.floating.clear();
        for y in sel.y0..=sel.y1 {
            for x in sel.x0..=sel.x1 {
                if sel.contains(x, y) {
                    if let Some(cell) = self.canvas.get(x, y) {
                        self.floating.insert((x, y), cell);
                    }
                    self.paint_cell(x, y, None);
                }
            }
        }
        self.shape = DragState::MovingFloating { anchor };
    }

    fn commit_move(&mut self, anchor: (u16, u16), end: (u16, u16)) {
        let dx = end.0 as i32 - anchor.0 as i32;
        let dy = end.1 as i32 - anchor.1 as i32;
        for (&(x, y), cell) in &std::mem::take(&mut self.floating) {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if (0..=u16::MAX as i32).contains(&nx) && (0..=u16::MAX as i32).contains(&ny) {
                self.paint_cell(nx as u16, ny as u16, Some(cell.clone()));
            }
        }
        self.end_gesture();
        if let Some(sel) = self.selection.take() {
            self.selection = Some(sel.translate(dx, dy));
        }
    }

    fn select_all(&mut self) {
        self.abort_gesture();
        self.selection = Some(Selection {
            x0: 0,
            y0: 0,
            x1: self.canvas.width.saturating_sub(1),
            y1: self.canvas.height.saturating_sub(1),
            mask: None,
        });
        self.tool = Tool::SelectRect;
        self.status = Some("Selected all".into());
    }

    fn copy_selection(&mut self, cut: bool) {
        let Some(sel) = self.selection.clone() else {
            self.status = Some("Nothing selected".into());
            return;
        };
        let mut cells = HashMap::new();
        for y in sel.y0..=sel.y1 {
            for x in sel.x0..=sel.x1 {
                if sel.contains(x, y) {
                    if let Some(cell) = self.canvas.get(x, y) {
                        cells.insert((x - sel.x0, y - sel.y0), cell);
                    }
                }
            }
        }
        self.clipboard = Some(Clipboard { width: sel.x1 - sel.x0 + 1, height: sel.y1 - sel.y0 + 1, cells });
        if cut {
            self.erase_selection(&sel);
            self.status = Some("Cut".into());
        } else {
            self.status = Some("Copied".into());
        }
    }

    fn erase_selection(&mut self, sel: &Selection) {
        self.begin_gesture();
        for y in sel.y0..=sel.y1 {
            for x in sel.x0..=sel.x1 {
                if sel.contains(x, y) {
                    self.paint_cell(x, y, None);
                }
            }
        }
        self.end_gesture();
    }

    fn delete_selection(&mut self) {
        let Some(sel) = self.selection.clone() else {
            self.status = Some("Nothing selected".into());
            return;
        };
        self.erase_selection(&sel);
        self.status = Some("Deleted".into());
    }

    /// Stamps the clipboard at the cursor (one undo step) and selects the
    /// pasted rectangle so it can be dragged into place.
    fn paste(&mut self) {
        let Some(clip) = self.clipboard.clone() else {
            self.status = Some("Clipboard is empty".into());
            return;
        };
        let (ox, oy) = self.cursor;
        self.begin_gesture();
        for (&(dx, dy), cell) in &clip.cells {
            let (x, y) = (ox.saturating_add(dx), oy.saturating_add(dy));
            if self.canvas.in_bounds(x, y) {
                self.paint_cell(x, y, Some(cell.clone()));
            }
        }
        self.end_gesture();
        let x1 = ox.saturating_add(clip.width - 1).min(self.canvas.width.saturating_sub(1));
        let y1 = oy.saturating_add(clip.height - 1).min(self.canvas.height.saturating_sub(1));
        self.selection = Some(Selection { x0: ox, y0: oy, x1, y1, mask: None });
        self.tool = Tool::SelectRect;
        self.status = Some(format!("Pasted {}x{}", clip.width, clip.height));
    }

    fn move_cursor(&mut self, dx: i32, dy: i32) {
        let nx = (self.cursor.0 as i32 + dx).clamp(0, self.canvas.width.saturating_sub(1) as i32);
        let ny = (self.cursor.1 as i32 + dy).clamp(0, self.canvas.height.saturating_sub(1) as i32);
        self.cursor = (nx as u16, ny as u16);
        if self.last_visible != (0, 0) {
            self.viewport.ensure_visible(self.cursor.0, self.cursor.1, self.last_visible);
        }
    }

    /// Pans the viewport without moving the cursor.
    fn scroll_viewport(&mut self, dx: i32, dy: i32) {
        let (vis_w, vis_h) = self.last_visible;
        let max_x = self.canvas.width.saturating_sub(vis_w);
        let max_y = self.canvas.height.saturating_sub(vis_h);
        self.viewport.scroll_x = (self.viewport.scroll_x as i32 + dx).clamp(0, max_x as i32) as u16;
        self.viewport.scroll_y = (self.viewport.scroll_y as i32 + dy).clamp(0, max_y as i32) as u16;
    }

    /// Scrolls by a whole visible page in the given direction.
    fn scroll_page(&mut self, dx: i32, dy: i32) {
        let (vis_w, vis_h) = self.last_visible;
        self.scroll_viewport(dx * vis_w.max(1) as i32, dy * vis_h.max(1) as i32);
    }

    // ----- saving and loading -----

    fn auto_save_name(&mut self) -> String {
        self.save_counter += 1;
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        format!("painting_{ts}_{}.ans", self.save_counter)
    }

    /// The folder Save As writes a bare file name into.
    fn save_dir(&self) -> PathBuf {
        self.current_path
            .as_ref()
            .and_then(|p| p.parent())
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.paintings_dir.clone())
    }

    /// Writes the canvas (format by extension) and, on success, adopts
    /// `path` as the current file.
    fn save_to(&mut self, path: PathBuf) -> bool {
        match export::save(&self.canvas, &path) {
            Ok(()) => {
                self.status = Some(format!("Saved {}", file_name(&path)));
                self.current_path = Some(path);
                self.dirty = false;
                true
            }
            Err(e) => {
                self.status = Some(format!("Save failed: {e}"));
                false
            }
        }
    }

    /// File > Save / Ctrl+S: write back to the open file, or ask for a name.
    fn save(&mut self) -> PaintResult {
        match self.current_path.clone() {
            Some(path) => {
                self.save_to(path);
            }
            None => self.begin_save_as(),
        }
        PaintResult::Continue
    }

    fn begin_save_as(&mut self) {
        let default = match &self.current_path {
            Some(p) => file_name(p),
            None => self.auto_save_name(),
        };
        self.save_as_draft = Some(default);
        self.save_as_suggested = true;
        self.status = Some("Save As: type a name (.ans .txt .html .svg .irc)".into());
    }

    /// A typed name: `~/…` and absolute paths are used as-is, a bare name
    /// goes in `save_dir()`, and `.ans` is added when there's no extension.
    fn resolve_save_name(&self, name: &str) -> PathBuf {
        let mut path = if let Some(rest) = name.strip_prefix("~/") {
            dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(name))
        } else {
            PathBuf::from(name)
        };
        if !path.is_absolute() {
            path = self.save_dir().join(path);
        }
        if path.extension().is_none() {
            path.set_extension("ans");
        }
        path
    }

    fn handle_save_as_key(&mut self, key: KeyEvent) -> PaintResult {
        let suggested = std::mem::take(&mut self.save_as_suggested);
        let Some(draft) = &mut self.save_as_draft else { return PaintResult::Continue };
        match key.code {
            KeyCode::Enter => return self.attempt_save_as(),
            KeyCode::Esc => {
                self.save_as_draft = None;
                self.save_then = None;
                self.status = Some("Cancelled".into());
            }
            KeyCode::Backspace => {
                draft.pop();
            }
            KeyCode::Delete => draft.clear(),
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                if suggested {
                    draft.clear();
                }
                draft.push(c)
            }
            _ => {}
        }
        PaintResult::Continue
    }

    fn attempt_save_as(&mut self) -> PaintResult {
        let Some(name) = self.save_as_draft.take() else { return PaintResult::Continue };
        let name = name.trim().to_string();
        if name.is_empty() {
            self.status = Some("Name required".into());
            self.save_as_draft = Some(name);
            return PaintResult::Continue;
        }
        let path = self.resolve_save_name(&name);
        if path.exists() {
            self.status = Some(format!("{} already exists — overwrite? Y/N", file_name(&path)));
            self.overwrite_confirm = Some(path);
            PaintResult::Continue
        } else if self.save_to(path) {
            self.finish_save_then()
        } else {
            self.save_then = None;
            PaintResult::Continue
        }
    }

    fn finish_save_then(&mut self) -> PaintResult {
        match self.save_then.take() {
            Some(action) => self.proceed_unsaved(action),
            None => PaintResult::Continue,
        }
    }

    fn handle_overwrite_key(&mut self, key: KeyEvent) -> PaintResult {
        let Some(path) = self.overwrite_confirm.take() else { return PaintResult::Continue };
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if self.save_to(path) {
                    return self.finish_save_then();
                }
                self.save_then = None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.save_then = None;
                self.status = Some("Cancelled".into());
            }
            // Ignore other keys and keep asking.
            _ => self.overwrite_confirm = Some(path),
        }
        PaintResult::Continue
    }

    fn unsaved_prompt(&mut self, action: UnsavedAction) {
        self.pending_unsaved_action = Some(action);
        self.status = Some("Unsaved changes — S) Save  D) Discard  Esc) Cancel".into());
    }

    /// Esc on an idle canvas / File > Exit: close, via the unsaved prompt.
    fn request_close(&mut self) -> PaintResult {
        if self.dirty {
            self.unsaved_prompt(UnsavedAction::Close);
            PaintResult::Continue
        } else {
            PaintResult::Close
        }
    }

    fn request_new(&mut self) {
        if self.dirty {
            self.unsaved_prompt(UnsavedAction::New);
        } else {
            self.begin_new();
        }
    }

    fn begin_new(&mut self) {
        self.begin_resize();
        self.resize_is_new = true;
        self.status = Some(self.resize_status_text());
    }

    /// Open or Import, both of which replace the canvas.
    fn request_open(&mut self, action: UnsavedAction) {
        if self.dirty {
            self.unsaved_prompt(action);
        } else {
            self.proceed_unsaved(action);
        }
    }

    fn resolve_unsaved_key(&mut self, key: KeyEvent) -> PaintResult {
        let Some(action) = self.pending_unsaved_action else { return PaintResult::Continue };
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Enter => {
                self.pending_unsaved_action = None;
                match self.current_path.clone() {
                    Some(path) => {
                        if self.save_to(path) {
                            return self.proceed_unsaved(action);
                        }
                    }
                    None => {
                        self.save_then = Some(action);
                        self.begin_save_as();
                    }
                }
                PaintResult::Continue
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.pending_unsaved_action = None;
                self.dirty = false;
                self.proceed_unsaved(action)
            }
            KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C') => {
                self.pending_unsaved_action = None;
                self.status = Some("Cancelled".into());
                PaintResult::Continue
            }
            _ => PaintResult::Continue,
        }
    }

    fn proceed_unsaved(&mut self, action: UnsavedAction) -> PaintResult {
        match action {
            UnsavedAction::Close => return PaintResult::Close,
            UnsavedAction::New => self.begin_new(),
            UnsavedAction::Open => {
                self.file_dialog = Some(FileDialog::new(DialogPurpose::Open, self.paintings_dir.clone()));
            }
            UnsavedAction::Import => {
                let dir = self
                    .import_dir
                    .clone()
                    .filter(|d| d.is_dir())
                    .or_else(|| dirs::picture_dir().filter(|d| d.is_dir()))
                    .or_else(dirs::home_dir)
                    .unwrap_or_else(|| self.paintings_dir.clone());
                self.file_dialog = Some(FileDialog::new(DialogPurpose::Import, dir));
            }
        }
        PaintResult::Continue
    }

    fn handle_file_dialog_key(&mut self, key: KeyEvent) -> PaintResult {
        let Some(dialog) = &mut self.file_dialog else { return PaintResult::Continue };
        let result = dialog.handle_key(key);
        self.finish_file_dialog(result);
        PaintResult::Continue
    }

    fn finish_file_dialog(&mut self, result: DialogResult) {
        match result {
            DialogResult::Continue => {}
            DialogResult::Cancel => {
                self.file_dialog = None;
                self.status = Some("Cancelled".into());
            }
            DialogResult::Open(path) => {
                self.file_dialog = None;
                if is_image(&path) {
                    self.import_dir = path.parent().map(Path::to_path_buf);
                    self.import_image(&path);
                } else {
                    self.load_file(&path);
                }
            }
        }
    }

    /// Loads a `.ans`/`.txt` painting, replacing the canvas (undoable).
    fn load_file(&mut self, path: &Path) {
        let is_txt = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("txt"));
        let result = if is_txt { ansi::load_txt(path, self.palette.color_at(self.fg_idx)) } else { ansi::load_ans(path) };
        match result {
            Ok(cells) => {
                self.abort_gesture();
                self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
                // Grow to fit rather than truncating loaded art.
                if let Some((_, _, max_x, max_y)) = ansi::bounds_of(&cells) {
                    self.canvas.width = self.canvas.width.max(max_x.saturating_add(1));
                    self.canvas.height = self.canvas.height.max(max_y.saturating_add(1));
                }
                self.canvas.cells = cells;
                self.selection = None;
                self.clamp_cursor_to_canvas();
                self.current_path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = Some(format!("Opened {}", file_name(path)));
            }
            Err(e) => self.status = Some(format!("Open failed: {e}")),
        }
    }

    /// Imports a PNG/JPEG as half-block art: each cell is `▀` with fg = the
    /// upper pixel and bg = the lower one, downscaled to at most 160x100
    /// cells. Replaces the canvas (undoable) and leaves it unsaved.
    fn import_image(&mut self, path: &Path) {
        let img = match image::open(path) {
            Ok(img) => img,
            Err(e) => {
                self.status = Some(format!("Import failed: {e}"));
                return;
            }
        };
        let (orig_w, orig_h) = (img.width(), img.height());
        if orig_w == 0 || orig_h == 0 {
            self.status = Some("Import failed: empty image".into());
            return;
        }
        const MAX_COLS: u32 = 160;
        const MAX_ROWS: u32 = 100;
        let scale = (MAX_COLS as f64 / orig_w as f64).min((MAX_ROWS * 2) as f64 / orig_h as f64).min(1.0);
        let target_w = ((orig_w as f64 * scale).round() as u32).max(1);
        let target_h = (((orig_h as f64 * scale).round() as u32).max(2) + 1) & !1; // round up to even
        let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3).into_rgb8();

        let cell_w = target_w as u16;
        let cell_h = (target_h / 2) as u16;
        let mut cells = HashMap::with_capacity(cell_w as usize * cell_h as usize);
        for cy in 0..cell_h {
            for cx in 0..cell_w {
                let top = resized.get_pixel(cx as u32, cy as u32 * 2);
                let bot = resized.get_pixel(cx as u32, cy as u32 * 2 + 1);
                cells.insert(
                    (cx, cy),
                    PaintCellSer {
                        ch: '▀',
                        fg: RgbColor::new(top[0], top[1], top[2]),
                        bg: RgbColor::new(bot[0], bot[1], bot[2]),
                    },
                );
            }
        }

        self.abort_gesture();
        self.history.push_canvas(CanvasSnapshot::capture(&self.canvas));
        self.canvas = Canvas::new(cell_w.max(1), cell_h.max(1));
        self.canvas.cells = cells;
        self.selection = None;
        self.current_path = None;
        self.dirty = true;
        self.clamp_cursor_to_canvas();
        self.status = Some(format!("Imported {} ({orig_w}x{orig_h}) as {cell_w}x{cell_h}", file_name(path)));
    }

    // ----- keyboard -----

    pub fn handle_key(&mut self, key: KeyEvent) -> PaintResult {
        if key.kind == KeyEventKind::Release {
            return PaintResult::Continue;
        }
        if self.info.is_some() {
            self.info = None;
            return PaintResult::Continue;
        }
        if self.file_dialog.is_some() {
            return self.handle_file_dialog_key(key);
        }
        if self.awaiting_brush_char {
            self.handle_brush_char_key(key);
            return PaintResult::Continue;
        }
        if self.palette_picker.is_some() {
            self.handle_palette_picker_key(key);
            return PaintResult::Continue;
        }
        if self.pending_unsaved_action.is_some() {
            return self.resolve_unsaved_key(key);
        }
        if self.save_as_draft.is_some() {
            return self.handle_save_as_key(key);
        }
        if self.overwrite_confirm.is_some() {
            return self.handle_overwrite_key(key);
        }
        if self.resize_draft.is_some() {
            self.handle_resize_key(key);
            return PaintResult::Continue;
        }
        if self.color_draft.is_some() {
            self.handle_color_key(key);
            return PaintResult::Continue;
        }
        if self.menu_open.is_some() {
            return self.handle_menu_key(key);
        }
        if let Some(idx) = menu_for_key(key.code) {
            self.open_menu(idx);
            return PaintResult::Continue;
        }
        // While composing text every key is text (typing "p" types "p").
        if let DragState::Text { origin } = self.shape {
            self.handle_text_key(origin, key);
            return PaintResult::Continue;
        }
        self.handle_canvas_key(key)
    }

    fn handle_canvas_key(&mut self, key: KeyEvent) -> PaintResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::PageUp => self.scroll_page(0, -1),
            KeyCode::PageDown => self.scroll_page(0, 1),
            KeyCode::Left if ctrl => self.scroll_page(-1, 0),
            KeyCode::Right if ctrl => self.scroll_page(1, 0),
            KeyCode::Up => self.move_cursor(0, -1),
            KeyCode::Down => self.move_cursor(0, 1),
            KeyCode::Left => self.move_cursor(-1, 0),
            KeyCode::Right => self.move_cursor(1, 0),
            KeyCode::Char(' ') if !ctrl => self.act(),
            KeyCode::Enter => match self.shape {
                DragState::Polygon { .. } => self.close_polygon(),
                DragState::Lasso { .. } => self.close_lasso(),
                _ => self.act(),
            },
            KeyCode::Esc => {
                if !self.cancel_gesture() {
                    return self.request_close();
                }
            }
            KeyCode::Delete => self.delete_selection(),
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'n' => self.request_new(),
                'a' => self.select_all(),
                's' => return self.save(),
                'o' => self.request_open(UnsavedAction::Open),
                'z' => self.undo(),
                'y' => self.redo(),
                'c' => self.copy_selection(false),
                'x' => self.copy_selection(true),
                'v' => self.paste(),
                _ => {}
            },
            KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::ALT) => {}
            KeyCode::Char(c) => self.handle_canvas_char(c),
            _ => {}
        }
        PaintResult::Continue
    }

    fn handle_canvas_char(&mut self, c: char) {
        if let Some(tool) = Tool::from_key(c) {
            self.tool = tool;
            self.abort_gesture();
            self.status = Some(format!("Tool: {}", tool.label()));
            return;
        }
        match c {
            'k' | 'K' => self.copy_selection(false),
            'x' | 'X' => self.copy_selection(true),
            'v' | 'V' => self.paste(),
            'z' | 'Z' => self.undo(),
            'y' | 'Y' => self.redo(),
            '[' => self.cycle_fg(-1),
            ']' => self.cycle_fg(1),
            '{' => self.cycle_bg(-1),
            '}' => self.cycle_bg(1),
            ',' => self.cycle_brush(-1),
            '.' => self.cycle_brush(1),
            'c' | 'C' => self.clear_canvas(),
            's' | 'S' => self.begin_save_as(),
            'l' | 'L' => self.request_open(UnsavedAction::Open),
            '+' | '=' => self.zoom_by(true),
            '-' | '_' => self.zoom_by(false),
            'h' | 'H' => self.flip_horizontal(),
            'j' | 'J' => self.flip_vertical(),
            'g' | 'G' => self.rotate_90_cw(),
            'n' | 'N' => self.rotate_90_ccw(),
            'm' | 'M' => self.rotate_180(),
            'e' | 'E' => self.swap_colors(),
            'i' | 'I' => self.begin_resize(),
            'a' | 'A' => self.begin_add_color(),
            'd' | 'D' => self.begin_palette_picker(),
            _ => {}
        }
    }

    fn handle_text_key(&mut self, origin: (u16, u16), key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.end_gesture();
                self.shape = DragState::None;
            }
            KeyCode::Esc => {
                self.cancel_gesture();
            }
            KeyCode::Backspace => {
                if self.cursor.0 > origin.0 {
                    self.cursor.0 -= 1;
                    self.paint_cell(self.cursor.0, self.cursor.1, None);
                }
            }
            KeyCode::Left => {
                if self.cursor.0 > origin.0 {
                    self.cursor.0 -= 1;
                }
            }
            KeyCode::Right => {
                let nx = self.cursor.0.saturating_add(1);
                if self.canvas.in_bounds(nx, self.cursor.1) {
                    self.cursor.0 = nx;
                }
            }
            KeyCode::Char(c)
                if self.canvas.in_bounds(self.cursor.0, self.cursor.1)
                    && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let fg = self.palette.color_at(self.fg_idx);
                let bg = self.palette.color_at(self.bg_idx);
                self.paint_cell(self.cursor.0, self.cursor.1, Some(PaintCellSer { ch: c, fg, bg }));
                let nx = self.cursor.0.saturating_add(1);
                if self.canvas.in_bounds(nx, self.cursor.1) {
                    self.cursor.0 = nx;
                }
            }
            _ => {}
        }
    }

    /// The shortcut-bar chips for the current mode (the original's legend).
    /// Each chip is clickable; a click synthesizes its key.
    fn chips(&self) -> Vec<Chip> {
        use KeyCode::*;
        if self.info.is_some() {
            return vec![Chip::new("Enter", "Close", Enter)];
        }
        if self.file_dialog.is_some() {
            return vec![
                Chip::new("Enter", "Open", Enter),
                Chip::new("Bksp", "Up", Backspace),
                Chip::new("↑", "Up", Up),
                Chip::new("↓", "Down", Down),
                Chip::new("PgUp", "Page Up", PageUp),
                Chip::new("PgDn", "Page Down", PageDown),
                Chip::new("Esc", "Cancel", Esc),
            ];
        }
        if self.awaiting_brush_char {
            return vec![Chip::new("Esc", "Cancel", Esc)];
        }
        if let Some(picker) = &self.palette_picker {
            if let Some(editing) = &picker.editing {
                return composer_chips(editing, "Apply");
            }
            return vec![
                Chip::new("←", "Prev", Left),
                Chip::new("→", "Next", Right),
                Chip::new("↑", "Row-", Up),
                Chip::new("↓", "Row+", Down),
                Chip::new("Enter", "Edit", Enter),
                Chip::new("F", "Set Fg", Char('f')),
                Chip::new("B", "Set Bg", Char('b')),
                Chip::new("Esc", "Close", Esc),
            ];
        }
        if self.pending_unsaved_action.is_some() {
            return vec![Chip::new("S", "Save", Char('s')), Chip::new("D", "Discard", Char('d')), Chip::new("Esc", "Cancel", Esc)];
        }
        if self.save_as_draft.is_some() {
            return vec![Chip::new("Enter", "Save", Enter), Chip::new("Del", "Clear", Delete), Chip::new("Esc", "Cancel", Esc)];
        }
        if self.overwrite_confirm.is_some() {
            return vec![Chip::new("Y", "Overwrite", Char('y')), Chip::new("N", "Cancel", Char('n'))];
        }
        if self.resize_draft.is_some() {
            let apply_label = if self.resize_is_new { "Create" } else { "Apply" };
            let (left, right, up, down) = match self.resize_mode {
                _ if self.resize_is_new => ("Width-", "Width+", "Height-", "Height+"),
                ResizeMode::Resize => ("Width-", "Width+", "Height-", "Height+"),
                ResizeMode::Stretch => ("X%-", "X%+", "Y%-", "Y%+"),
                ResizeMode::Skew => ("X°-", "X°+", "Y°-", "Y°+"),
            };
            let mut chips = vec![
                Chip::new("←", left, Left),
                Chip::new("→", right, Right),
                Chip::new("↑", up, Up),
                Chip::new("↓", down, Down),
            ];
            if !self.resize_is_new {
                chips.push(Chip::new("Tab", self.resize_mode.next().label(), Tab));
            }
            chips.push(Chip::new("Enter", apply_label, Enter));
            chips.push(Chip::new("Esc", "Cancel", Esc));
            return chips;
        }
        if let Some(draft) = &self.color_draft {
            return composer_chips(draft, "Add Color");
        }
        if self.menu_open.is_some() {
            return vec![
                Chip::new("←", "Prev Menu", Left),
                Chip::new("→", "Next Menu", Right),
                Chip::new("↑", "Up", Up),
                Chip::new("↓", "Down", Down),
                Chip::new("Enter", "Select", Enter),
                Chip::new("Esc", "Close Menu", Esc),
            ];
        }
        let closable = matches!(self.shape, DragState::Polygon { .. } | DragState::Lasso { .. });
        let texting = matches!(self.shape, DragState::Text { .. });
        let gesture_active = self.shape != DragState::None || self.selection.is_some();
        let selecting = matches!(self.tool.interaction(), Interaction::Select);
        let space_label = if closable {
            "Add Point"
        } else if texting {
            "Commit"
        } else if selecting {
            "Select/Lift"
        } else if self.tool == Tool::Text {
            "Start Text"
        } else {
            "Paint"
        };
        let mut chips = vec![Chip::new("Esc", if gesture_active { "Cancel" } else { "Close" }, Esc)];
        if !texting {
            chips.push(Chip::new("Space", space_label, Char(' ')));
        }
        if closable || texting {
            chips.push(Chip::new("Enter", if texting { "Commit Text" } else { "Close Shape" }, Enter));
        }
        if texting {
            return chips;
        }
        if self.selection.is_some() {
            chips.push(Chip::new("K", "Copy", Char('k')));
            chips.push(Chip::new("X", "Cut", Char('x')));
            if self.shape == DragState::None {
                chips.push(Chip::new("Del", "Delete", Delete));
            }
        }
        if self.clipboard.is_some() {
            chips.push(Chip::new("V", "Paste", Char('v')));
        }
        chips.extend([
            Chip::new("Z", "Undo", Char('z')),
            Chip::new("Y", "Redo", Char('y')),
            Chip::ctrl("^S", "Save", 's'),
            Chip::new("L", "Open", Char('l')),
            Chip::new("[", "Fg-", Char('[')),
            Chip::new("]", "Fg+", Char(']')),
            Chip::new("{", "Bg-", Char('{')),
            Chip::new("}", "Bg+", Char('}')),
            Chip::new(",", "Br-", Char(',')),
            Chip::new(".", "Br+", Char('.')),
            Chip::new("+", "Zoom In", Char('+')),
            Chip::new("-", "Zoom Out", Char('-')),
            Chip::new("E", "Swap", Char('e')),
            Chip::ctrl("^A", "All", 'a'),
            Chip::new("F1", "Help", F(1)),
        ]);
        chips
    }

    /// The status-bar text: the last message plus tool/cursor/canvas info.
    fn status_text(&self) -> String {
        let info = format!(
            "{} | {},{} | {}x{} | {}%{}",
            self.tool.label(),
            self.cursor.0,
            self.cursor.1,
            self.canvas.width,
            self.canvas.height,
            self.viewport.zoom_percent(),
            if self.dirty { " | Modified" } else { "" }
        );
        match &self.status {
            Some(s) => format!("{s}  —  {info}"),
            None => info,
        }
    }
}

fn composer_chips(composer: &ColorComposer, apply: &'static str) -> Vec<Chip> {
    use KeyCode::*;
    if composer.hex_input.is_some() {
        return vec![Chip::new("Enter", "Apply Hex", Enter), Chip::new("Bksp", "Erase", Backspace), Chip::new("Esc", "Back", Esc)];
    }
    vec![
        Chip::new("Tab", "Channel", Tab),
        Chip::new("←", "-1", Left),
        Chip::new("→", "+1", Right),
        Chip::new("PgDn", "-10", PageDown),
        Chip::new("PgUp", "+10", PageUp),
        Chip::new("#", "Hex", Char('#')),
        Chip::new("Enter", apply, Enter),
        Chip::new("Esc", "Cancel", Esc),
    ]
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

fn cycle(idx: usize, len: usize, dir: i32) -> usize {
    if len == 0 {
        return 0;
    }
    ((idx as i32 + dir).rem_euclid(len as i32)) as usize
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string())
}

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// True for files Paint can open (.ans, .txt, .png, .jpg, .jpeg;
/// case-insensitive).
pub fn is_paintable(path: &Path) -> bool {
    has_extension(path, &["ans", "txt", "png", "jpg", "jpeg"])
}

fn is_image(path: &Path) -> bool {
    has_extension(path, &["png", "jpg", "jpeg"])
}

/// A loaded painting, for drawing a picture as a desktop shortcut icon.
pub struct Picture {
    pub width: u16,
    pub height: u16,
    /// Row-major, `width * height`; `None` = transparent.
    cells: Vec<Option<PaintCellSer>>,
}

impl Picture {
    fn from_cells(cells: &ansi::Cells) -> Option<Self> {
        let (min_x, min_y, max_x, max_y) = ansi::bounds_of(cells)?;
        let width = max_x - min_x + 1;
        let height = max_y - min_y + 1;
        let mut out = vec![None; width as usize * height as usize];
        for (&(x, y), cell) in cells {
            out[(y - min_y) as usize * width as usize + (x - min_x) as usize] = Some(cell.clone());
        }
        Some(Self { width, height, cells: out })
    }

    /// `(char, fg, bg)` at `(x, y)`, or `None` if transparent/out of range.
    pub fn cell(&self, x: u16, y: u16) -> Option<(char, ratatui::style::Color, ratatui::style::Color)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells[y as usize * self.width as usize + x as usize]
            .as_ref()
            .map(|c| (c.ch, c.fg.into(), c.bg.into()))
    }

    /// Draws the picture centered in `area`. A picture larger than `area` is
    /// shrunk to fit (keeping its proportions) by sampling cells, so the
    /// whole drawing stays visible. Transparent cells are left as they are.
    pub fn draw(&self, buf: &mut Buffer, area: Rect) {
        let area = area.intersection(buf.area);
        if area.width == 0 || area.height == 0 || self.width == 0 || self.height == 0 {
            return;
        }
        // Scale factor as a fraction num/den <= 1.
        let (num, den) = if self.width <= area.width && self.height <= area.height {
            (1u32, 1u32)
        } else if area.width as u32 * self.height as u32 <= area.height as u32 * self.width as u32 {
            (area.width as u32, self.width as u32)
        } else {
            (area.height as u32, self.height as u32)
        };
        let shown_w = ((self.width as u32 * num / den) as u16).clamp(1, area.width);
        let shown_h = ((self.height as u32 * num / den) as u16).clamp(1, area.height);
        let dst_x = area.x + (area.width - shown_w) / 2;
        let dst_y = area.y + (area.height - shown_h) / 2;
        for dy in 0..shown_h {
            for dx in 0..shown_w {
                // Sample so the first and last rows/columns are kept: a
                // shrunk outline keeps all four of its edges.
                let sample = |d: u16, shown: u16, full: u16| -> u16 {
                    if shown <= 1 || shown >= full {
                        d.min(full - 1)
                    } else {
                        ((d as u32 * (full as u32 - 1) + (shown as u32 - 1) / 2) / (shown as u32 - 1)) as u16
                    }
                };
                let src_x = sample(dx, shown_w, self.width);
                let src_y = sample(dy, shown_h, self.height);
                if let Some((ch, fg, bg)) = self.cell(src_x, src_y) {
                    let cell = buf.get_mut(dst_x + dx, dst_y + dy);
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(fg).bg(bg));
                }
            }
        }
    }
}

/// Loads a `.ans` or `.txt` painting (None if unreadable or empty).
pub fn load_picture(path: &Path) -> Option<Picture> {
    let cells = if has_extension(path, &["txt"]) {
        ansi::load_txt(path, palette::PAINT_PALETTE[7]).ok()?
    } else if has_extension(path, &["ans"]) {
        ansi::load_ans(path).ok()?
    } else {
        return None;
    };
    Picture::from_cells(&cells)
}

#[cfg(test)]
mod tests;
