//! Keyboard handling: the host's modal layers (help, dialogs, menus) and
//! miditui's `main.rs` key bindings (global keys, then Normal / Insert /
//! Select mode keys), unchanged except that quitting (`q`, `Ctrl+C`,
//! `Ctrl+Q`, and now `Esc` in Normal mode) goes through the unsaved-changes
//! prompt and closes the app instead of exiting the process.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::app::{EditMode, FocusedPanel};
use super::filedialog::DialogResult;
use super::model::TICKS_PER_BEAT;
use super::{menu_for_key, MidiApp, MidiResult};

impl MidiApp {
    pub fn handle_key(&mut self, key: KeyEvent) -> MidiResult {
        if key.kind == KeyEventKind::Release {
            // Handle note key releases for keyboard playing (only reported by
            // terminals with the keyboard-enhancement protocol).
            if let KeyCode::Char(c) = key.code {
                self.app.handle_note_key_release(c);
            }
            return MidiResult::Continue;
        }
        self.dispatch(key)
    }

    /// Routes a key press to the topmost layer.
    pub(super) fn dispatch(&mut self, key: KeyEvent) -> MidiResult {
        if self.info.is_some() {
            self.info = None;
            return MidiResult::Continue;
        }
        if self.show_help {
            self.handle_help_key(key);
            return MidiResult::Continue;
        }
        if let Some(dialog) = &mut self.app.file_dialog {
            match dialog.handle_key(key) {
                DialogResult::Continue => {}
                DialogResult::Cancel => self.app.file_dialog_cancel(),
                DialogResult::Open(path) => {
                    self.app.file_dialog_picked(path);
                }
            }
            return MidiResult::Continue;
        }
        if self.unsaved.is_some() {
            return self.resolve_unsaved_key(key);
        }
        if self.app.save_dialog.open {
            return self.handle_save_dialog_key(key);
        }
        if self.app.new_project_dialog.open {
            self.handle_new_project_key(key);
            return MidiResult::Continue;
        }
        if self.app.renaming_track {
            self.handle_rename_key(key);
            return MidiResult::Continue;
        }
        if self.menu_open.is_some() {
            return self.handle_menu_key(key);
        }
        if let Some(idx) = menu_for_key(key.code) {
            self.open_menu(idx);
            return MidiResult::Continue;
        }
        self.handle_app_key(key.code, key.modifiers)
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        let scroll = &mut self.app.help_scroll;
        match key.code {
            KeyCode::Char('?') | KeyCode::Esc | KeyCode::Enter | KeyCode::F(1) => {
                self.show_help = false;
                *scroll = 0; // Reset scroll on close
            }
            KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            KeyCode::PageDown => *scroll = scroll.saturating_add(10),
            KeyCode::Home => *scroll = 0,
            _ => {}
        }
    }

    fn handle_save_dialog_key(&mut self, key: KeyEvent) -> MidiResult {
        if self.app.save_dialog.overwrite.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let saved = self.app.save_dialog_confirm();
                    return self.finish_save_then(saved);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    // Back to the Save dialog to pick another name.
                    self.app.save_dialog.overwrite = None;
                }
                _ => {}
            }
            return MidiResult::Continue;
        }
        match key.code {
            KeyCode::Enter => {
                let saved = self.app.save_dialog_confirm();
                if saved || !self.app.save_dialog.open {
                    return self.finish_save_then(saved);
                }
            }
            KeyCode::Esc => {
                self.save_then = None;
                self.app.save_dialog_cancel();
            }
            KeyCode::Tab => self.app.save_dialog_toggle_format(),
            KeyCode::Backspace => self.app.save_dialog_backspace(),
            // Only accept valid filename characters
            KeyCode::Char(c)
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && (c.is_alphanumeric() || c == '_' || c == '-') =>
            {
                self.app.save_dialog_input(c);
            }
            _ => {}
        }
        MidiResult::Continue
    }

    fn handle_new_project_key(&mut self, key: KeyEvent) {
        let app = &mut self.app;
        match key.code {
            KeyCode::Enter => {
                app.new_project_dialog_confirm();
            }
            KeyCode::Esc => app.new_project_dialog_cancel(),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('y') => app.new_project_dialog_left(), // Select "Yes"
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') => app.new_project_dialog_right(), // Select "No"
            // The dialog's buttons: choose and confirm in one go.
            KeyCode::Char('Y') => {
                app.new_project_dialog_left();
                app.new_project_dialog_confirm();
            }
            KeyCode::Char('N') => {
                app.new_project_dialog_right();
                app.new_project_dialog_confirm();
            }
            KeyCode::Tab => {
                if app.new_project_dialog.selected == 0 {
                    app.new_project_dialog_right();
                } else {
                    app.new_project_dialog_left();
                }
            }
            _ => {}
        }
    }

    fn handle_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.app.confirm_rename_track(),
            KeyCode::Esc => self.app.cancel_rename_track(),
            KeyCode::Backspace => self.app.rename_track_backspace(),
            KeyCode::Char(c) if !c.is_control() && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.app.rename_track_input(c)
            }
            _ => {}
        }
    }

    /// Esc in Insert/Select mode: back to Normal mode.
    pub(super) fn leave_mode(&mut self) {
        // Stop Insert Mode recording if active
        self.app.stop_insert_recording();
        self.app.edit_mode = EditMode::Normal;
        self.app.release_all_notes();
        self.app.set_status("Normal mode");
    }

    /// Runs a Normal-mode single-key command (used by the menus).
    pub(super) fn normal_mode_command(&mut self, c: char) -> MidiResult {
        self.handle_normal_mode(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// miditui's `handle_key`: global bindings, then the mode's bindings.
    fn handle_app_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> MidiResult {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            // Quit (now: close the app, asking to save first)
            KeyCode::Char('c') | KeyCode::Char('q') if ctrl => return self.request_close(),
            KeyCode::Char('q') if self.app.edit_mode == EditMode::Normal => return self.request_close(),

            // Undo/Redo (Ctrl+Z / Ctrl+Y)
            KeyCode::Char('z') if ctrl => {
                self.app.undo();
                return MidiResult::Continue;
            }
            KeyCode::Char('y') if ctrl => {
                self.app.redo();
                return MidiResult::Continue;
            }

            // Help toggle
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                return MidiResult::Continue;
            }

            // Escape - return to normal mode, or close from Normal mode
            KeyCode::Esc => {
                if self.app.edit_mode != EditMode::Normal {
                    self.leave_mode();
                    return MidiResult::Continue;
                }
                return self.request_close();
            }

            // Tab - cycle focus
            KeyCode::Tab => {
                self.app.focused_panel = match self.app.focused_panel {
                    FocusedPanel::TrackList => FocusedPanel::Timeline,
                    FocusedPanel::Timeline => FocusedPanel::PianoRoll,
                    FocusedPanel::PianoRoll => FocusedPanel::Keyboard,
                    FocusedPanel::Keyboard => FocusedPanel::TrackList,
                };
                return MidiResult::Continue;
            }

            // Playback controls
            // Shift+Space: restart playback from beginning
            KeyCode::Char(' ') if modifiers.contains(KeyModifiers::SHIFT) => {
                self.app.restart_playback();
                return MidiResult::Continue;
            }
            KeyCode::Char(' ') => {
                self.app.toggle_playback();
                return MidiResult::Continue;
            }
            KeyCode::Char('.') if self.app.edit_mode == EditMode::Normal => {
                self.app.stop_playback();
                return MidiResult::Continue;
            }

            // Export WAV (Ctrl+E)
            KeyCode::Char('e') if ctrl => {
                self.app.export_project();
                return MidiResult::Continue;
            }
            // Export MIDI (Ctrl+M)
            KeyCode::Char('m') if ctrl => {
                self.app.export_midi();
                return MidiResult::Continue;
            }
            // Save project (Ctrl+S) - opens save dialog
            KeyCode::Char('s') if ctrl => {
                self.app.open_save_dialog();
                return MidiResult::Continue;
            }
            // Load project (Ctrl+O) - opens file browser (after the unsaved prompt)
            KeyCode::Char('o') if ctrl => return self.request(super::UnsavedAction::Open),
            // New project (Ctrl+N) - opens confirmation dialog
            KeyCode::Char('n') if ctrl => {
                self.app.open_new_project_dialog();
                return MidiResult::Continue;
            }
            // Load SoundFont (Ctrl+L) - opens SoundFont browser
            KeyCode::Char('l') if ctrl => {
                self.app.open_soundfont_dialog(false);
                return MidiResult::Continue;
            }
            _ => {}
        }

        // Other control/alt chords are not note keys.
        if matches!(code, KeyCode::Char(_)) && modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return MidiResult::Continue;
        }

        // Mode-specific key bindings
        match self.app.edit_mode {
            EditMode::Normal => self.handle_normal_mode(code, modifiers),
            EditMode::Insert => self.handle_insert_mode(code),
            EditMode::Select => self.handle_select_mode(code),
        }
    }

    /// Toggles mute or solo on the selected track.
    fn toggle_mute_solo(&mut self, solo: bool) {
        let app = &mut self.app;
        if app.selected_track().is_some() {
            app.save_state(if solo { "Toggle solo" } else { "Toggle mute" });
        }
        let status_msg = app.selected_track_mut().map(|track| {
            let flag = if solo { &mut track.solo } else { &mut track.muted };
            *flag = !*flag;
            let status = match (solo, *flag) {
                (false, true) => "Muted",
                (false, false) => "Unmuted",
                (true, true) => "Solo on",
                (true, false) => "Solo off",
            };
            format!("{} {}", status, track.name)
        });
        // Silence all notes - the sequencer will restart appropriate ones
        app.audio.all_notes_off(true);
        if let Some(msg) = status_msg {
            app.set_status(msg);
            app.mark_modified();
        }
    }

    fn adjust_tempo(&mut self, delta: i32) {
        let app = &mut self.app;
        app.save_state("Adjust tempo");
        let tempo = (app.project().tempo as i32 + delta).clamp(20, 300) as u32;
        app.project_mut().tempo = tempo;
        app.audio.set_tempo(tempo);
        app.set_status(format!("Tempo: {} BPM", tempo));
        app.mark_modified();
    }

    /// Handles keys in normal mode.
    fn handle_normal_mode(&mut self, code: KeyCode, modifiers: KeyModifiers) -> MidiResult {
        let app = &mut self.app;
        match code {
            // Mode changes
            KeyCode::Char('i') => {
                app.edit_mode = EditMode::Insert;
                app.set_status("Insert mode - keys play & insert notes");
            }
            KeyCode::Char('v') => {
                app.edit_mode = EditMode::Select;
                app.set_status("Select mode");
            }

            // Navigation
            KeyCode::Char('h') | KeyCode::Left => app.move_cursor_horizontal(-(app.zoom as i32)),
            KeyCode::Char('l') | KeyCode::Right => app.move_cursor_horizontal(app.zoom as i32),
            KeyCode::Char('k') | KeyCode::Up => app.move_cursor_vertical(1),
            KeyCode::Char('j') | KeyCode::Down => app.move_cursor_vertical(-1),
            KeyCode::Char('H') => app.move_cursor_horizontal(-(TICKS_PER_BEAT as i32 * 4)), // Jump left by measure
            KeyCode::Char('L') => app.move_cursor_horizontal(TICKS_PER_BEAT as i32 * 4),    // Jump right by measure
            KeyCode::Char('0') => {
                app.cursor_tick = 0;
                app.scroll_x = 0;
            }
            KeyCode::Char('$') => app.cursor_tick = app.project().duration_ticks(),

            // Track selection
            KeyCode::Char('J') => {
                if app.selected_track_index < app.project().track_count().saturating_sub(1) {
                    app.selected_track_index += 1;
                }
            }
            KeyCode::Char('K') => {
                if app.selected_track_index > 0 {
                    app.selected_track_index -= 1;
                }
            }

            // Track management
            KeyCode::Char('a') => app.add_track(),
            KeyCode::Char('d') | KeyCode::Char('x') => app.delete_selected_track(),
            KeyCode::Char('r') => app.start_rename_track(),
            KeyCode::Char('g') => app.toggle_view_mode(),
            KeyCode::Char('t') => app.toggle_expanded_tracks(),
            KeyCode::Char('m') => self.toggle_mute_solo(false),
            KeyCode::Char('s') => self.toggle_mute_solo(true),

            // Note editing
            KeyCode::Enter | KeyCode::Char('n') => app.place_note(),
            KeyCode::Delete => app.delete_note_at_cursor(),

            // Zoom
            KeyCode::Char('=') | KeyCode::Char('+') => {
                app.zoom(0.5);
                app.set_status(format!("Zoom: {} ticks/col", app.zoom));
            }
            KeyCode::Char('-') if !modifiers.contains(KeyModifiers::CONTROL) => {
                app.zoom(2.0);
                app.set_status(format!("Zoom: {} ticks/col", app.zoom));
            }

            // Octave
            KeyCode::Char(',') => app.change_octave(-1),
            KeyCode::Char('/') => app.change_octave(1),

            // Tempo adjustment
            KeyCode::Char('[') => self.adjust_tempo(-5),
            KeyCode::Char(']') => self.adjust_tempo(5),

            // Time signature adjustment
            KeyCode::Char('{') => app.adjust_time_sig_numerator(-1),
            KeyCode::Char('}') => app.adjust_time_sig_numerator(1),
            KeyCode::Char('|') => app.cycle_time_sig_denominator(),

            // Instrument cycling (< and >)
            KeyCode::Char('<') => app.cycle_instrument(-1),
            KeyCode::Char('>') => app.cycle_instrument(1),

            // Volume control (; and ')
            KeyCode::Char(';') => app.adjust_track_volume(-5),
            KeyCode::Char('\'') => app.adjust_track_volume(5),

            // Pan control (( and ))
            KeyCode::Char('(') => app.adjust_track_pan(-8),
            KeyCode::Char(')') => app.adjust_track_pan(8),

            // Export to WAV directly
            KeyCode::Char('e') => app.export_project(),

            // Cycle highlight mode for active notes during playback
            KeyCode::Char('W') => app.cycle_highlight_mode(),

            // Keyboard note playing (still works in normal mode)
            KeyCode::Char(c) => {
                app.handle_note_key(c);
            }

            _ => {}
        }
        MidiResult::Continue
    }

    /// Handles keys in insert mode.
    fn handle_insert_mode(&mut self, code: KeyCode) -> MidiResult {
        let app = &mut self.app;
        match code {
            // Navigation still works
            KeyCode::Left => app.move_cursor_horizontal(-(app.zoom as i32)),
            KeyCode::Right => app.move_cursor_horizontal(app.zoom as i32),
            KeyCode::Up => app.move_cursor_vertical(1),
            KeyCode::Down => app.move_cursor_vertical(-1),

            // Octave
            KeyCode::Char(',') => app.change_octave(-1),
            KeyCode::Char('/') => app.change_octave(1),

            // Instrument cycling (silences playing notes before switching)
            KeyCode::Char('<') => app.cycle_instrument(-1),
            KeyCode::Char('>') => app.cycle_instrument(1),

            // In insert mode, keyboard keys insert and play notes
            KeyCode::Char(c) => {
                app.handle_note_key(c);
            }

            _ => {}
        }
        MidiResult::Continue
    }

    /// Deletes all selected notes (Select mode `x` / Delete).
    pub(super) fn delete_selected_notes(&mut self) {
        let app = &mut self.app;
        if app.selected_notes.is_empty() {
            return;
        }
        app.save_state("Delete selected notes");
        let ids_to_delete: Vec<_> = app.selected_notes.drain().collect();
        let count = ids_to_delete.len();
        if let Some(track) = app.selected_track_mut() {
            for id in ids_to_delete {
                track.remove_note(id);
            }
        }
        app.set_status(format!("Deleted {} notes", count));
        app.mark_modified();
    }

    /// Handles keys in select mode.
    fn handle_select_mode(&mut self, code: KeyCode) -> MidiResult {
        let app = &mut self.app;
        let has_selection = !app.selected_notes.is_empty();
        match code {
            // Shift+A: shrink note duration / Shift+D: expand note duration
            KeyCode::Char('A') => {
                if has_selection {
                    app.adjust_selected_notes_duration(-(app.zoom as i32));
                    app.set_status("Reduced note duration");
                }
            }
            KeyCode::Char('D') => {
                if has_selection {
                    app.adjust_selected_notes_duration(app.zoom as i32);
                    app.set_status("Expanded note duration");
                }
            }

            // WASD: move selected notes (if notes selected) or navigate cursor
            KeyCode::Char('w') => {
                if has_selection {
                    app.transpose_selected_notes(1);
                    app.set_status("Moved notes up");
                } else {
                    app.move_cursor_vertical(1);
                }
            }
            KeyCode::Char('s') => {
                if has_selection {
                    app.transpose_selected_notes(-1);
                    app.set_status("Moved notes down");
                } else {
                    app.move_cursor_vertical(-1);
                }
            }
            KeyCode::Char('a') => {
                if has_selection {
                    app.move_selected_notes_horizontal(-(app.zoom as i32));
                    app.set_status("Moved notes left");
                } else {
                    app.move_cursor_horizontal(-(app.zoom as i32));
                }
            }
            KeyCode::Char('d') => {
                if has_selection {
                    app.move_selected_notes_horizontal(app.zoom as i32);
                    app.set_status("Moved notes right");
                } else {
                    app.move_cursor_horizontal(app.zoom as i32);
                }
            }

            // Navigation with hjkl and arrow keys
            KeyCode::Char('h') | KeyCode::Left => app.move_cursor_horizontal(-(app.zoom as i32)),
            KeyCode::Char('l') | KeyCode::Right => app.move_cursor_horizontal(app.zoom as i32),
            KeyCode::Char('k') | KeyCode::Up => app.move_cursor_vertical(1),
            KeyCode::Char('j') | KeyCode::Down => app.move_cursor_vertical(-1),

            // Select note under cursor
            KeyCode::Enter | KeyCode::Char(' ') => {
                let note_id = app.selected_track().and_then(|track| {
                    track
                        .notes()
                        .iter()
                        .find(|n| n.pitch == app.cursor_pitch && n.is_active_at(app.cursor_tick))
                        .map(|n| n.id)
                });
                if let Some(id) = note_id {
                    if app.selected_notes.contains(&id) {
                        app.selected_notes.remove(&id);
                        app.set_status("Deselected note");
                    } else {
                        app.selected_notes.insert(id);
                        app.set_status("Selected note");
                    }
                }
            }

            // Delete selected notes
            KeyCode::Char('x') | KeyCode::Delete => self.delete_selected_notes(),

            // Clear selection
            KeyCode::Char('c') => {
                app.selected_notes.clear();
                app.set_status("Selection cleared");
            }

            _ => {}
        }
        MidiResult::Continue
    }
}
