//! Headless tests for `PaintApp`: rendering at several sizes, mouse
//! painting through the rendered layout, saving/loading, menus and the
//! unsaved-changes flow.

use std::fs;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;

use super::*;
use crate::dmui::Palette;

fn pal() -> Palette {
    Palette {
        primary: Color::Blue,
        accent: Color::Yellow,
        highlight: Color::Yellow,
        background: Color::Cyan,
        surface: Color::Blue,
        text: Color::Yellow,
        bar_bg: Color::Gray,
        bar_key: Color::Red,
        bar_label: Color::Black,
        selection_text: Color::Black,
        focus_band: Some(Color::Red),
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mm-paint-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn draw(app: &mut PaintApp, w: u16, h: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| app.render(f, f.size(), "MenuMaker", &pal())).unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf.get(x, y).symbol().to_string()).collect()
}

fn press(app: &mut PaintApp, code: KeyCode) -> PaintResult {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn mouse(app: &mut PaintApp, kind: MouseEventKind, x: u16, y: u16, area: Rect) -> PaintResult {
    app.handle_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }, area)
}

fn type_text(app: &mut PaintApp, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

/// Every modal/menu state, so rendering is exercised for each of them.
fn each_state(dir: &std::path::Path, mut f: impl FnMut(&mut PaintApp)) {
    let fresh = || PaintApp::new(dir.to_path_buf());
    let mut states: Vec<PaintApp> = Vec::new();
    states.push(fresh());
    for fkey in [1, 2, 3, 4, 5, 6] {
        let mut app = fresh();
        press(&mut app, KeyCode::F(fkey));
        states.push(app);
    }
    for c in ['S', 'I', 'A', 'D', 'L'] {
        let mut app = fresh();
        press(&mut app, KeyCode::Char(c));
        states.push(app);
    }
    let mut app = fresh();
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    states.push(app);
    let mut app = fresh();
    press(&mut app, KeyCode::Char('D'));
    press(&mut app, KeyCode::Enter); // edit a swatch
    states.push(app);
    let mut app = fresh();
    press(&mut app, KeyCode::Char(' ')); // paint -> dirty
    press(&mut app, KeyCode::Esc); // unsaved prompt
    states.push(app);
    let mut app = fresh();
    app.begin_brush_char_capture();
    states.push(app);
    let mut app = fresh();
    app.request_open(UnsavedAction::Import);
    states.push(app);
    let mut app = fresh();
    app.overwrite_confirm = Some(dir.join("x.ans"));
    states.push(app);
    let mut app = fresh();
    app.activate_menu_item(5, 0); // keyboard help
    states.push(app);
    let mut app = fresh();
    press(&mut app, KeyCode::Char('-'));
    press(&mut app, KeyCode::Char('-'));
    states.push(app);
    for app in &mut states {
        f(app);
    }
}

#[test]
fn renders_every_state_at_large_and_tiny_sizes_without_panicking() {
    let dir = temp_dir("render");
    each_state(&dir, |app| {
        for (w, h) in [(120, 40), (40, 10), (80, 24), (20, 5), (1, 1), (200, 60)] {
            draw(app, w, h);
            // Mouse events against the recorded layout must not panic either.
            let area = Rect::new(0, 0, w, h);
            for (x, y) in [(0, 0), (w / 2, h / 2), (w.saturating_sub(1), h.saturating_sub(1)), (5, 3)] {
                mouse(app, MouseEventKind::Moved, x, y, area);
                mouse(app, MouseEventKind::ScrollDown, x, y, area);
            }
        }
    });
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn chrome_shows_title_menu_bar_and_boxes() {
    let dir = temp_dir("chrome");
    let mut app = PaintApp::new(dir.clone());
    let buf = draw(&mut app, 120, 40);
    assert!(row_text(&buf, 0).contains("MenuMaker - Paint - Untitled"));
    let menu = row_text(&buf, 3);
    assert!(menu.contains("File F2") && menu.contains("Colors F6") && menu.contains("Help F1"), "{menu}");
    let all: String = (0..40).map(|y| row_text(&buf, y)).collect();
    for band in ["◄ TOOLS ►", "◄ CANVAS 100x50 ►", "◄ COLORS ►"] {
        assert!(all.contains(band), "missing {band}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pencil_strokes_save_and_load_back_as_a_picture() {
    let dir = temp_dir("pencil");
    let mut app = PaintApp::new(dir.clone());
    let area = Rect::new(0, 0, 120, 40);
    draw(&mut app, 120, 40);
    let vp = app.layout.viewport;
    assert!(vp.width > 10 && vp.height > 5);
    // Drag a 3-cell stroke on row 1, then dot one cell on row 3.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), vp.x + 2, vp.y + 1, area);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), vp.x + 4, vp.y + 1, area);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), vp.x + 4, vp.y + 1, area);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Right), vp.x + 3, vp.y + 3, area);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Right), vp.x + 3, vp.y + 3, area);
    assert_eq!(app.canvas.cells.len(), 4);
    assert!(app.dirty);

    // Save As through the dialog, typing a full path.
    let path = dir.join("stroke.ans");
    press(&mut app, KeyCode::Char('S'));
    draw(&mut app, 120, 40);
    press(&mut app, KeyCode::Delete);
    type_text(&mut app, &path.to_string_lossy());
    press(&mut app, KeyCode::Enter);
    assert!(path.exists(), "status: {:?}", app.status);
    assert!(!app.dirty);
    assert_eq!(app.current_path.as_deref(), Some(path.as_path()));

    let pic = load_picture(&path).expect("picture loads");
    assert_eq!((pic.width, pic.height), (3, 3));
    let white = Color::Rgb(235, 235, 240);
    let black = Color::Rgb(20, 20, 24);
    for x in 0..3 {
        assert_eq!(pic.cell(x, 0), Some(('█', white, black)));
    }
    // Right button paints with Fg/Bg swapped.
    assert_eq!(pic.cell(1, 2), Some(('█', black, white)));
    assert_eq!(pic.cell(0, 1), None);

    // Reopening it gives the same cells and Save writes back to it.
    let mut reopened = PaintApp::open(dir.clone(), &path);
    assert_eq!(reopened.canvas.cells.len(), 4);
    assert_eq!(reopened.file_label(), "stroke.ans");
    reopened.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(reopened.status.as_deref(), Some("Saved stroke.ans"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn picture_draws_centered_and_shrinks_to_fit() {
    let dir = temp_dir("picture");
    let path = dir.join("p.txt");
    fs::write(&path, "abc\nd f\n").unwrap();
    let pic = load_picture(&path).unwrap();
    assert_eq!((pic.width, pic.height), (3, 2));
    let mut buf = Buffer::empty(Rect::new(0, 0, 7, 4));
    pic.draw(&mut buf, Rect::new(0, 0, 7, 4));
    assert_eq!(row_text(&buf, 1), "  abc  ");
    assert_eq!(row_text(&buf, 2), "  d f  ");
    // Too big for the area: shrunk (by sampling) instead of cropped, so the
    // outline of a large drawing still shows.
    let mut small = Buffer::empty(Rect::new(0, 0, 1, 1));
    pic.draw(&mut small, Rect::new(0, 0, 5, 5));
    assert_eq!(small.get(0, 0).symbol(), "a");
    let wide = dir.join("wide.txt");
    fs::write(&wide, "#....#\n#....#\n#....#\n#....#\n").unwrap();
    let wide = load_picture(&wide).unwrap();
    let mut half = Buffer::empty(Rect::new(0, 0, 3, 2));
    wide.draw(&mut half, Rect::new(0, 0, 3, 2));
    assert_eq!(row_text(&half, 0), "#.#");
    assert_eq!(row_text(&half, 1), "#.#");
    assert!(load_picture(&dir.join("missing.ans")).is_none());
    fs::write(dir.join("empty.ans"), "").unwrap();
    assert!(load_picture(&dir.join("empty.ans")).is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn paintable_extensions_are_case_insensitive() {
    for name in ["a.ans", "b.TXT", "c.Png", "d.jpg", "e.JPEG"] {
        assert!(is_paintable(std::path::Path::new(name)), "{name}");
    }
    for name in ["a.svg", "b.html", "c", "d.ans~"] {
        assert!(!is_paintable(std::path::Path::new(name)), "{name}");
    }
}

#[test]
fn function_keys_open_menus_and_esc_closes_then_exits() {
    let dir = temp_dir("menus");
    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::F(2));
    assert_eq!(app.menu_open, Some(0));
    press(&mut app, KeyCode::F(5));
    assert_eq!(app.menu_open, Some(3));
    press(&mut app, KeyCode::F(5));
    assert_eq!(app.menu_open, None);
    press(&mut app, KeyCode::F(1));
    assert_eq!(app.menu_open, Some(5));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.menu_open, None);
    press(&mut app, KeyCode::F(10));
    assert_eq!(app.menu_open, Some(0));
    press(&mut app, KeyCode::Esc);
    assert_eq!(press(&mut app, KeyCode::Esc), PaintResult::Close);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn menu_bar_and_dropdown_are_clickable() {
    let dir = temp_dir("menuclick");
    let mut app = PaintApp::new(dir.clone());
    let area = Rect::new(0, 0, 120, 40);
    draw(&mut app, 120, 40);
    let (image_rect, _) = app.layout.menu_bar[3];
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), image_rect.x + 1, image_rect.y, area);
    assert_eq!(app.menu_open, Some(3));
    draw(&mut app, 120, 40);
    // "Rotate 90° CW" is the third Image item.
    let (item_rect, item) = app.layout.dropdown[2];
    assert_eq!(item, 2);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), item_rect.x + 2, item_rect.y, area);
    assert_eq!(app.menu_open, None);
    assert_eq!((app.canvas.width, app.canvas.height), (50, 100));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn shortcut_chips_are_clickable() {
    let dir = temp_dir("chips");
    let mut app = PaintApp::new(dir.clone());
    let area = Rect::new(0, 0, 120, 40);
    let buf = draw(&mut app, 120, 40);
    // Find the "Z Undo" chip by its text on the shortcut row.
    let row = row_text(&buf, 1);
    let col = row.find("Z Undo").expect("undo chip shown");
    let col = row[..col].chars().count() as u16;
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.canvas.cells.len(), 1);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), col, 1, area);
    assert_eq!(app.canvas.cells.len(), 0, "clicking the chip undoes");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unsaved_prompt_discard_closes_and_save_goes_through_save_as() {
    let dir = temp_dir("unsaved");
    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(press(&mut app, KeyCode::Esc), PaintResult::Continue);
    assert!(app.pending_unsaved_action.is_some());
    press(&mut app, KeyCode::Esc); // cancel the prompt
    assert!(app.pending_unsaved_action.is_none());
    press(&mut app, KeyCode::Esc);
    assert_eq!(press(&mut app, KeyCode::Char('d')), PaintResult::Close);

    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('s')); // no file yet -> Save As
    assert!(app.save_as_draft.is_some());
    press(&mut app, KeyCode::Delete);
    type_text(&mut app, "keep");
    assert_eq!(press(&mut app, KeyCode::Enter), PaintResult::Close);
    assert!(dir.join("keep.ans").exists());

    // Typing over the suggested Save As name replaces it.
    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Char('S'));
    type_text(&mut app, "fresh");
    assert_eq!(app.save_as_draft.as_deref(), Some("fresh"));

    // Saving over an existing file asks first.
    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Char('S'));
    press(&mut app, KeyCode::Delete);
    type_text(&mut app, "keep.ans");
    press(&mut app, KeyCode::Enter);
    assert!(app.overwrite_confirm.is_some());
    press(&mut app, KeyCode::Char('n'));
    assert!(app.dirty);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn open_dialog_loads_a_painting_and_import_reads_png() {
    let dir = temp_dir("open");
    fs::write(dir.join("art.txt"), "hi\n").unwrap();
    // A 2x2 PNG: red/green over blue/white.
    let png = image::RgbImage::from_fn(2, 2, |x, y| match (x, y) {
        (0, 0) => image::Rgb([255, 0, 0]),
        (1, 0) => image::Rgb([0, 255, 0]),
        (0, 1) => image::Rgb([0, 0, 255]),
        _ => image::Rgb([255, 255, 255]),
    });
    png.save(dir.join("tiny.png")).unwrap();

    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::Char('L'));
    assert!(app.file_dialog.is_some());
    type_text(&mut app, "art");
    press(&mut app, KeyCode::Down); // past ".."
    press(&mut app, KeyCode::Enter);
    assert!(app.file_dialog.is_none());
    assert_eq!(app.canvas.get(1, 0).map(|c| c.ch), Some('i'));
    assert_eq!(app.file_label(), "art.txt");

    let imported = PaintApp::open(dir.clone(), &dir.join("tiny.png"));
    assert_eq!((imported.canvas.width, imported.canvas.height), (2, 1));
    let cell = imported.canvas.get(0, 0).unwrap();
    assert_eq!(cell.ch, '▀');
    assert!(imported.dirty);
    assert_eq!(imported.file_label(), "Untitled");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tools_shapes_selection_and_undo_via_keyboard() {
    let dir = temp_dir("tools");
    let mut app = PaintApp::new(dir.clone());
    press(&mut app, KeyCode::Char('9')); // rectangle
    press(&mut app, KeyCode::Char(' '));
    for _ in 0..3 {
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.canvas.cells.len(), 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Char('k'));
    press(&mut app, KeyCode::Delete);
    assert!(app.canvas.cells.is_empty());
    press(&mut app, KeyCode::Char('z'));
    assert_eq!(app.canvas.cells.len(), 12);
    press(&mut app, KeyCode::Char('y'));
    assert!(app.canvas.cells.is_empty());
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.canvas.cells.len(), 12);
    // Text tool: typed letters paint instead of switching tools.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char(' '));
    type_text(&mut app, "p1");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.tool, Tool::Text);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_non_painting_never_saves_over_it() {
    let dir = std::env::temp_dir().join(format!("mm-paint-md-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("notes.md");
    std::fs::write(&doc, "# Title\nhello").unwrap();
    let app = PaintApp::open(dir.clone(), &doc);
    assert!(app.current_path.is_none());
    assert_eq!(std::fs::read_to_string(&doc).unwrap(), "# Title\nhello");
    std::fs::remove_dir_all(&dir).unwrap();
}
