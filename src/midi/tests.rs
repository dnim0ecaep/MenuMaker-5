//! Integration tests for the Music app (the ported miditui unit tests live
//! next to their modules).

use super::app::EditMode;
use super::*;
use crossterm::event::{KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;
use std::fs;

const EXAMPLE_MID: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/midi/testdata/epic_final_boss.mid");

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mm-midi-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn palette() -> crate::dmui::Palette {
    crate::dmui::Palette {
        primary: Color::Blue,
        accent: Color::Cyan,
        highlight: Color::Yellow,
        background: Color::Black,
        surface: Color::DarkGray,
        text: Color::White,
        bar_bg: Color::Gray,
        bar_key: Color::Red,
        bar_label: Color::Black,
        selection_text: Color::Black,
        focus_band: None,
    }
}

/// A MidiApp without an audio device; a first-run SoundFont prompt (no
/// SoundFont on this machine) is dismissed.
fn test_app(dir: &Path, file: Option<&Path>) -> MidiApp {
    let mut app = MidiApp::build(dir.to_path_buf(), file, false);
    if app.app.file_dialog.is_some() {
        app.handle_key(key(KeyCode::Esc));
    }
    assert!(app.app.file_dialog.is_none());
    app
}

fn press(app: &mut MidiApp, code: KeyCode) -> MidiResult {
    app.handle_key(key(code))
}

fn chr(app: &mut MidiApp, c: char) -> MidiResult {
    press(app, KeyCode::Char(c))
}

fn draw(app: &mut MidiApp, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let pal = palette();
    terminal
        .draw(|frame| {
            let area = frame.size();
            app.render(frame, area, "MenuMaker", &pal);
        })
        .unwrap();
    terminal
}

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf.get(x, y).symbol());
        }
        out.push('\n');
    }
    out
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
}

#[test]
fn renders_at_many_sizes_without_panic() {
    let dir = temp_dir("render");
    let mut app = test_app(&dir, Some(Path::new(EXAMPLE_MID)));
    let sizes = [(120, 40), (40, 10), (20, 5), (1, 1), (80, 24), (200, 60), (3, 3), (10, 2), (61, 13)];
    for &(w, h) in &sizes {
        draw(&mut app, w, h);
    }
    // Every view, mode, overlay and dialog at each size.
    let setups: Vec<Box<dyn Fn(&mut MidiApp)>> = vec![
        Box::new(|a| {
            chr(a, 'g');
        }),
        Box::new(|a| {
            chr(a, 'g');
        }),
        Box::new(|a| {
            chr(a, 't');
        }),
        Box::new(|a| {
            chr(a, 'i');
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            chr(a, 'v');
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            chr(a, '?');
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            press(a, KeyCode::F(2));
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            a.handle_key(ctrl('s'));
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            a.handle_key(ctrl('n'));
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            a.handle_key(ctrl('o'));
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            a.handle_key(ctrl('l'));
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            chr(a, 'r');
        }),
        Box::new(|a| {
            press(a, KeyCode::Esc);
            chr(a, ' ');
            std::thread::sleep(std::time::Duration::from_millis(40));
            a.tick();
        }),
        Box::new(|a| {
            chr(a, 'a');
            a.app.dirty = true;
            press(a, KeyCode::Esc);
        }),
    ];
    for setup in &setups {
        setup(&mut app);
        for &(w, h) in &sizes {
            draw(&mut app, w, h);
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_deskmate_chrome_and_panels() {
    let dir = temp_dir("chrome");
    let mut app = test_app(&dir, Some(Path::new(EXAMPLE_MID)));
    let terminal = draw(&mut app, 120, 40);
    let text = screen_text(&terminal);
    assert!(text.contains("MenuMaker - Music - epic_final_boss.mid"), "{text}");
    for needle in ["File F2", "Edit F3", "View F4", "Track F5", "Play F6", "Help F1", "◄ Transport ►", "◄ Tracks ►", "Piano Roll", "Project Timeline", "Keyboard (Octave: +0)"] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
    // Layout regions for mouse hit-testing were recorded.
    assert!(app.app.layout.piano_roll_grid.width > 0);
    assert!(app.app.layout.visible_pitches > 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn opens_example_midi_saves_and_reloads() {
    let dir = temp_dir("roundtrip");
    let mut app = test_app(&dir, Some(Path::new(EXAMPLE_MID)));
    let project = app.app.project();
    assert!(project.track_count() >= 2, "tracks: {}", project.track_count());
    let notes: usize = project.tracks().iter().map(|t| t.notes().len()).sum();
    assert!(notes > 50, "notes: {notes}");
    assert!(!app.app.dirty);
    assert_eq!(app.file_label(), "epic_final_boss.mid");
    let pitches: Vec<Vec<(u8, u32)>> = project
        .tracks()
        .iter()
        .map(|t| t.notes().iter().map(|n| (n.pitch, n.start_tick)).collect())
        .collect();

    // Save As a .mid in a temp folder through the Save dialog.
    let out = dir.join("copy.mid");
    app.app.project_path = Some(dir.join("x.mid"));
    app.handle_key(ctrl('s'));
    assert!(app.app.save_dialog.open);
    assert_eq!(app.app.save_dialog.format, SaveFormat::Midi);
    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "copy".chars() {
        chr(&mut app, c);
    }
    press(&mut app, KeyCode::Enter);
    assert!(!app.app.save_dialog.open);
    assert!(out.is_file(), "{:?}", app.app.status_message);
    assert_eq!(app.app.project_path.as_deref(), Some(out.as_path()));

    let reloaded = test_app(&dir, Some(&out));
    let project2 = reloaded.app.project();
    // Same notes per track (order among notes starting together may differ).
    let non_empty = |p: &Vec<Vec<(u8, u32)>>| {
        p.iter()
            .filter(|t| !t.is_empty())
            .map(|t| {
                let mut t = t.clone();
                t.sort();
                t
            })
            .collect::<Vec<_>>()
    };
    let pitches2: Vec<Vec<(u8, u32)>> = project2
        .tracks()
        .iter()
        .map(|t| t.notes().iter().map(|n| (n.pitch, n.start_tick)).collect())
        .collect();
    assert_eq!(non_empty(&pitches), non_empty(&pitches2));
    assert_eq!(app.app.project().tempo, project2.tempo);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn json_and_oxm_round_trip_and_open_dialog_lists_them() {
    let dir = temp_dir("formats");
    let mut app = test_app(&dir, None);
    chr(&mut app, 'n'); // place a note
    assert!(app.app.dirty);
    for (format_tabs, name) in [(0, "song.json"), (1, "song.oxm")] {
        app.handle_key(ctrl('s'));
        for _ in 0..format_tabs {
            press(&mut app, KeyCode::Tab);
        }
        for _ in 0..20 {
            press(&mut app, KeyCode::Backspace);
        }
        for c in "song".chars() {
            chr(&mut app, c);
        }
        press(&mut app, KeyCode::Enter);
        assert!(dir.join(name).is_file(), "{name}: {:?}", app.app.status_message);
        assert!(!app.app.dirty);
        let again = test_app(&dir, Some(&dir.join(name)));
        assert_eq!(again.app.project().tracks()[0].notes().len(), 1);
    }
    // Ctrl+O lists folders and music files from data_dir.
    fs::create_dir_all(dir.join("sub")).unwrap();
    app.handle_key(ctrl('o'));
    let dialog = app.app.file_dialog.as_ref().expect("open dialog");
    let names: Vec<&str> = dialog.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"sub") && names.contains(&"song.json") && names.contains(&"song.oxm"), "{names:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn missing_mid_path_starts_blank_and_saves_there() {
    let dir = temp_dir("missing");
    let target = dir.join("new tune.mid");
    let mut app = test_app(&dir, Some(&target));
    assert_eq!(app.app.project().track_count(), 1);
    assert_eq!(app.file_label(), "new tune.mid");
    chr(&mut app, 'n');
    // Esc on a dirty project asks; S saves straight to the target file.
    assert_eq!(press(&mut app, KeyCode::Esc), MidiResult::Continue);
    assert!(app.unsaved.is_some());
    assert_eq!(chr(&mut app, 's'), MidiResult::Close);
    assert!(target.is_file());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn close_paths_go_through_unsaved_prompt() {
    let dir = temp_dir("close");
    let mut app = test_app(&dir, None);
    // Clean: Esc closes immediately.
    assert_eq!(press(&mut app, KeyCode::Esc), MidiResult::Close);

    let mut app = test_app(&dir, None);
    chr(&mut app, 'a'); // add track -> dirty
    assert_eq!(press(&mut app, KeyCode::Esc), MidiResult::Continue);
    assert!(app.unsaved.is_some());
    assert_eq!(press(&mut app, KeyCode::Esc), MidiResult::Continue); // cancel
    assert!(app.unsaved.is_none());
    assert_eq!(chr(&mut app, 'q'), MidiResult::Continue); // q asks too
    assert_eq!(chr(&mut app, 'd'), MidiResult::Close); // discard

    // Esc leaves Insert mode first; Ctrl+C asks.
    let mut app = test_app(&dir, None);
    chr(&mut app, 'i');
    assert_eq!(app.app.edit_mode, EditMode::Insert);
    assert_eq!(press(&mut app, KeyCode::Esc), MidiResult::Continue);
    assert_eq!(app.app.edit_mode, EditMode::Normal);
    chr(&mut app, 'a');
    assert_eq!(app.handle_key(ctrl('c')), MidiResult::Continue);
    assert!(app.unsaved.is_some());

    // Save from the prompt with no file yet -> Save dialog, then closes.
    assert_eq!(chr(&mut app, 's'), MidiResult::Continue);
    assert!(app.app.save_dialog.open);
    assert_eq!(press(&mut app, KeyCode::Enter), MidiResult::Close);
    assert!(dir.join("New_Project.json").is_file());

    // File > Exit via F2 menu.
    let mut app = test_app(&dir, None);
    press(&mut app, KeyCode::F(2));
    assert_eq!(app.menu_open, Some(0));
    press(&mut app, KeyCode::End);
    assert_eq!(press(&mut app, KeyCode::Enter), MidiResult::Close);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn editing_keys_undo_redo_and_modes() {
    let dir = temp_dir("edit");
    let mut app = test_app(&dir, None);
    chr(&mut app, 'n');
    assert_eq!(app.app.selected_track().unwrap().notes().len(), 1);
    app.handle_key(ctrl('z'));
    assert_eq!(app.app.selected_track().unwrap().notes().len(), 0);
    app.handle_key(ctrl('y'));
    assert_eq!(app.app.selected_track().unwrap().notes().len(), 1);

    // Track management.
    chr(&mut app, 'a');
    assert_eq!(app.app.project().track_count(), 2);
    chr(&mut app, 'm');
    assert!(app.app.selected_track().unwrap().muted);
    chr(&mut app, 's');
    assert!(app.app.selected_track().unwrap().solo);
    chr(&mut app, '\'');
    assert_eq!(app.app.selected_track().unwrap().volume, 105);
    chr(&mut app, ')');
    assert_eq!(app.app.selected_track().unwrap().pan, 72);
    chr(&mut app, '>');
    assert_eq!(app.app.selected_track().unwrap().program, 1);
    chr(&mut app, 'r');
    assert!(app.app.renaming_track);
    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "Bass".chars() {
        chr(&mut app, c);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.app.selected_track().unwrap().name, "Bass");
    chr(&mut app, 'd');
    assert_eq!(app.app.project().track_count(), 1);

    // Tempo and time signature.
    chr(&mut app, ']');
    assert_eq!(app.app.project().tempo, 125);
    chr(&mut app, '}');
    assert_eq!(app.app.project().time_sig_numerator, 5);

    // Select mode: select the note under the cursor and move it.
    app.app.cursor_tick = 0;
    app.app.cursor_pitch = 60;
    chr(&mut app, 'v');
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.app.selected_notes.len(), 1);
    chr(&mut app, 'w');
    assert_eq!(app.app.selected_track().unwrap().notes()[0].pitch, 61);
    chr(&mut app, 'D');
    assert!(app.app.selected_track().unwrap().notes()[0].duration_ticks > 480);
    chr(&mut app, 'x');
    assert_eq!(app.app.selected_track().unwrap().notes().len(), 0);
    press(&mut app, KeyCode::Esc);

    // Insert mode: QWERTY keys add notes; key releases are honored.
    chr(&mut app, 'i');
    chr(&mut app, 'z');
    chr(&mut app, 'q');
    let pitches: Vec<u8> = app.app.selected_track().unwrap().notes().iter().map(|n| n.pitch).collect();
    assert!(pitches.contains(&48) && pitches.contains(&60), "{pitches:?}");
    app.handle_key(KeyEvent { kind: KeyEventKind::Release, ..key(KeyCode::Char('z')) });
    assert!(app.app.saw_key_release);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn playback_advances_from_tick() {
    let dir = temp_dir("play");
    let mut app = test_app(&dir, Some(Path::new(EXAMPLE_MID)));
    draw(&mut app, 120, 40);
    chr(&mut app, ' ');
    assert!(app.app.audio.is_playing());
    std::thread::sleep(std::time::Duration::from_millis(120));
    app.tick();
    assert!(app.app.cursor_tick > 0);
    assert!(!app.app.active_tracks.is_empty() || app.app.cursor_tick > 0);
    chr(&mut app, '.');
    assert!(!app.app.audio.is_playing());
    assert_eq!(app.app.cursor_tick, 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn mouse_menu_track_list_and_piano_roll() {
    let dir = temp_dir("mouse");
    let mut app = test_app(&dir, None);
    let area = Rect::new(0, 0, 120, 40);
    draw(&mut app, 120, 40);

    // Click "File F2" on the menu bar, then click outside to close it.
    let (file_rect, _) = app.layout.menu_bar[0];
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), file_rect.x + 1, file_rect.y), area);
    assert_eq!(app.menu_open, Some(0));
    draw(&mut app, 120, 40);
    assert!(!app.layout.dropdown.is_empty());
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 100, 30), area);
    assert_eq!(app.menu_open, None);
    draw(&mut app, 120, 40);

    // Track list: click the mute indicator of track 1.
    let tl = app.app.layout.track_list;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), tl.x + 1, tl.y + 1), area);
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), tl.x + 1, tl.y + 1), area);
    assert!(app.app.project().tracks()[0].muted);

    // Right-click toggles Insert mode; a click in the grid places a note.
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 60, 20), area);
    assert_eq!(app.app.edit_mode, EditMode::Insert);
    let grid = app.app.layout.piano_roll_grid;
    let (x, y) = (grid.x + 4, grid.y + 3);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area);
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x, y), area);
    let notes = app.app.selected_track().unwrap().notes().to_vec();
    assert_eq!(notes.len(), 1);
    let expected_pitch = (app.app.scroll_y as u16 + app.app.layout.visible_pitches as u16 - 1 - 2) as u8;
    assert_eq!(notes[0].pitch, expected_pitch);
    assert_eq!(notes[0].start_tick, 4 * app.app.zoom);

    // Scroll wheel over the grid scrolls pitches; horizontal scroll moves time.
    let before = app.app.scroll_y;
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, x, y), area);
    assert_eq!(app.app.scroll_y, before + 1);
    app.handle_mouse(mouse(MouseEventKind::ScrollRight, x, y), area);
    assert_eq!(app.app.scroll_x, app.app.zoom);

    // Shortcut chip click acts like its key (Esc = back to Normal).
    draw(&mut app, 120, 40);
    let (chip, _) = app.layout.buttons[0];
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), chip.x, chip.y), area);
    assert_eq!(app.app.edit_mode, EditMode::Normal);

    // Tiny screen: events don't panic.
    let tiny = Rect::new(0, 0, 1, 1);
    draw(&mut app, 1, 1);
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Drag(MouseButton::Left), MouseEventKind::Up(MouseButton::Left), MouseEventKind::ScrollDown] {
        app.handle_mouse(mouse(kind, 0, 0), tiny);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn autosave_lives_in_data_dir_and_new_is_blank() {
    let dir = temp_dir("autosave");
    let mut app = test_app(&dir, None);
    chr(&mut app, 'n');
    app.app.force_autosave();
    assert!(dir.join(".autosave.oxm").is_file());
    // A new blank app does not reload it…
    let mut blank = test_app(&dir, None);
    assert_eq!(blank.app.project().tracks()[0].notes().len(), 0);
    // …but File > Recover Autosave does.
    press(&mut blank, KeyCode::F(2));
    let idx = MENU_FILE.iter().position(|i| i.action == MenuAction::RecoverAutosave).unwrap();
    blank.activate_menu_item(0, idx);
    assert_eq!(blank.app.project().tracks()[0].notes().len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn midi_file_detection() {
    assert!(is_midi_file(Path::new("a.mid")));
    assert!(is_midi_file(Path::new("a.MIDI")));
    assert!(is_midi_file(Path::new("/x/y.Oxm")));
    assert!(!is_midi_file(Path::new("a.json")));
    assert!(!is_midi_file(Path::new("a.wav")));
    assert!(!is_midi_file(Path::new("mid")));
}

#[test]
fn soundfont_choice_is_saved_in_data_dir() {
    let sf = Path::new("/usr/share/sounds/sf2/TimGM6mb.sf2");
    if !sf.is_file() {
        return;
    }
    let dir = temp_dir("sfchoice");
    let mut app = test_app(&dir, None);
    app.handle_key(ctrl('l'));
    assert!(app.app.file_dialog.is_some());
    for c in sf.to_string_lossy().chars() {
        chr(&mut app, c);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.app.file_dialog.is_none());
    assert_eq!(app.app.soundfont_path.as_deref(), Some(sf));
    assert_eq!(settings::Settings::load(&dir).soundfont.as_deref(), Some(sf));
    let _ = fs::remove_dir_all(&dir);
}

/// Opening the real audio device (with the system SoundFont) and playing
/// must not write anything to stderr (ALSA diagnostics would corrupt the TUI).
#[cfg(unix)]
#[test]
fn audio_init_is_silent_on_stderr() {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn dup(fd: i32) -> i32;
        fn dup2(src: i32, dst: i32) -> i32;
        fn close(fd: i32) -> i32;
    }
    let dir = temp_dir("stderr");
    let capture_path = dir.join("stderr.txt");
    let capture = fs::File::create(&capture_path).unwrap();
    // SAFETY: plain fd juggling; fd 2 is restored before returning.
    let saved = unsafe { dup(2) };
    assert!(saved >= 0);
    unsafe { dup2(capture.as_raw_fd(), 2) };
    {
        let mut app = MidiApp::new(dir.clone());
        if app.app.file_dialog.is_some() {
            app.handle_key(key(KeyCode::Esc));
        }
        chr(&mut app, 'z');
        chr(&mut app, ' ');
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            app.tick();
        }
        chr(&mut app, '.');
        // Provoke a real libasound diagnostic ("ALSA lib pcm.c: ... Unknown
        // PCM"), which must be swallowed by the installed handler.
        #[cfg(target_os = "linux")]
        {
            use std::os::raw::{c_char, c_int, c_void};
            extern "C" {
                fn snd_pcm_open(pcm: *mut *mut c_void, name: *const c_char, stream: c_int, mode: c_int) -> c_int;
            }
            audio::engine::quiet::install();
            let mut pcm = std::ptr::null_mut();
            // SAFETY: opening a PCM by name; it fails (no such device) and
            // nothing is left to close.
            let rc = unsafe { snd_pcm_open(&mut pcm, c"mm_no_such_device".as_ptr(), 0, 0) };
            assert!(rc < 0);
        }
        // Close drops the output stream.
        assert_eq!(press(&mut app, KeyCode::Esc), MidiResult::Close);
    }
    unsafe {
        dup2(saved, 2);
        close(saved);
    }
    let mut text = String::new();
    fs::File::open(&capture_path).unwrap().read_to_string(&mut text).unwrap();
    assert!(text.is_empty(), "stderr output: {text:?}");
    let _ = fs::remove_dir_all(&dir);
}
