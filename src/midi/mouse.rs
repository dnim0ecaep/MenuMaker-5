//! Mouse handling: host chrome first (shortcut chips, dialog buttons, file
//! rows, menu bar and dropdowns), then miditui's `main.rs` mouse dispatch
//! (click, double-click, drag, right-click mode toggle, middle-drag pan,
//! vertical/horizontal wheel, Ctrl+wheel zoom) unchanged.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::app::{DragState, EditMode};
use super::filedialog::DialogResult;
use super::ui::hit;
use super::{MidiApp, MidiResult};

impl MidiApp {
    /// All mouse events; `area` = the full-screen Rect last passed to render.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> MidiResult {
        let (x, y) = (mouse.column, mouse.row);
        let dragging = self.app.drag_state != DragState::None;
        if !hit(area, x, y) && !dragging && !matches!(mouse.kind, MouseEventKind::Up(_)) {
            return MidiResult::Continue;
        }
        let down = matches!(mouse.kind, MouseEventKind::Down(_));

        // Shortcut chips and dialog buttons act like their keys.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let found = self.layout.buttons.iter().rev().find(|(r, _)| hit(*r, x, y)).map(|(_, k)| *k);
            if let Some(key) = found {
                return self.dispatch(key);
            }
        }

        if self.info.is_some() {
            if down {
                self.info = None;
            }
            return MidiResult::Continue;
        }
        if self.show_help {
            match mouse.kind {
                // Click anywhere to close help
                MouseEventKind::Down(MouseButton::Left) => {
                    self.show_help = false;
                    self.app.help_scroll = 0;
                }
                // Mouse scroll to navigate help content
                MouseEventKind::ScrollUp => self.app.help_scroll = self.app.help_scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => self.app.help_scroll = self.app.help_scroll.saturating_add(3),
                _ => {}
            }
            return MidiResult::Continue;
        }
        if self.app.file_dialog.is_some() {
            self.file_dialog_mouse(mouse);
            return MidiResult::Continue;
        }
        if self.unsaved.is_some() || self.app.save_dialog.open || self.app.new_project_dialog.open {
            return MidiResult::Continue;
        }

        if down {
            if let Some(&(_, idx)) = self.layout.menu_bar.iter().find(|(r, _)| hit(*r, x, y)) {
                if self.menu_open == Some(idx) {
                    self.close_menu();
                } else {
                    self.open_menu(idx);
                }
                return MidiResult::Continue;
            }
        }
        if let Some(menu_idx) = self.menu_open {
            if down {
                if let Some(&(_, item)) = self.layout.dropdown.iter().find(|(r, _)| hit(*r, x, y)) {
                    return self.activate_menu_item(menu_idx, item);
                }
                self.close_menu();
            }
            return MidiResult::Continue;
        }

        self.app_mouse(mouse);
        MidiResult::Continue
    }

    fn file_dialog_mouse(&mut self, mouse: MouseEvent) {
        let (x, y) = (mouse.column, mouse.row);
        let row = self.layout.file_rows.iter().find(|(r, _)| hit(*r, x, y)).map(|(_, i)| *i);
        let Some(dialog) = &mut self.app.file_dialog else { return };
        let result = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match row {
                Some(idx) => dialog.click(idx),
                None => DialogResult::Continue,
            },
            MouseEventKind::ScrollUp => {
                dialog.move_selection(-3);
                DialogResult::Continue
            }
            MouseEventKind::ScrollDown => {
                dialog.move_selection(3);
                DialogResult::Continue
            }
            _ => DialogResult::Continue,
        };
        match result {
            DialogResult::Continue => {}
            DialogResult::Cancel => self.app.file_dialog_cancel(),
            DialogResult::Open(path) => {
                self.app.file_dialog_picked(path);
            }
        }
    }

    /// miditui's `handle_mouse`.
    fn app_mouse(&mut self, mouse: MouseEvent) {
        let app = &mut self.app;
        let x = mouse.column;
        let y = mouse.row;
        let shift_held = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl_held = mouse.modifiers.contains(KeyModifiers::CONTROL) || mouse.modifiers.contains(KeyModifiers::SUPER);

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.last_mouse_pos = Some((x, y));
                // Check for double-click
                if self.click_tracker.record_click(x, y) {
                    app.handle_double_click(x, y);
                } else {
                    app.handle_drag_start(x, y, shift_held);
                    app.handle_mouse_click(x, y, shift_held);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.handle_drag_end();
                if let Some((lx, ly)) = self.last_mouse_pos.take() {
                    app.handle_piano_key_release(lx, ly);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => app.handle_drag_move(x, y),
            MouseEventKind::Down(MouseButton::Right) => {
                app.edit_mode = match app.edit_mode {
                    EditMode::Normal => {
                        app.set_status("Insert mode - click to place notes");
                        EditMode::Insert
                    }
                    EditMode::Insert | EditMode::Select => {
                        app.stop_insert_recording();
                        app.set_status("Normal mode");
                        EditMode::Normal
                    }
                };
            }
            // Middle-click for panning (start scroll drag)
            MouseEventKind::Down(MouseButton::Middle) => app.handle_drag_start(x, y, false),
            MouseEventKind::Up(MouseButton::Middle) => app.handle_drag_end(),
            MouseEventKind::Drag(MouseButton::Middle) => app.handle_drag_move(x, y),
            MouseEventKind::ScrollUp => app.handle_mouse_scroll(x, y, 0, 1, ctrl_held),
            MouseEventKind::ScrollDown => app.handle_mouse_scroll(x, y, 0, -1, ctrl_held),
            MouseEventKind::ScrollLeft => app.handle_mouse_scroll(x, y, -1, 0, ctrl_held),
            MouseEventKind::ScrollRight => app.handle_mouse_scroll(x, y, 1, 0, ctrl_held),
            _ => {}
        }
    }
}
