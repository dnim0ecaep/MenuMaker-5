//! Help overlay content: keyboard shortcuts and commands (miditui's help
//! screen; drawn as a scrollable DeskMate dialog in `dialogs.rs`).

use crate::dmui::Palette;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Key binding entry for the help display.
struct KeyBinding {
    key: &'static str,
    description: &'static str,
}

const GENERAL_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "?",
        description: "Toggle this help",
    },
    KeyBinding {
        key: "q / Esc",
        description: "Close Music (asks to save changes)",
    },
    KeyBinding {
        key: "Ctrl+C / Ctrl+Q",
        description: "Close Music (asks to save changes)",
    },
    KeyBinding {
        key: "F2-F6, F10",
        description: "Menus: File Edit View Track Play (F10 = File)",
    },
    KeyBinding {
        key: "F1",
        description: "Help menu",
    },
    KeyBinding {
        key: "Tab",
        description: "Cycle focus between panels",
    },
    KeyBinding {
        key: "Space",
        description: "Play / Pause",
    },
    KeyBinding {
        key: "Shift+Space",
        description: "Restart playback from beginning",
    },
    KeyBinding {
        key: ".",
        description: "Stop (reset to start)",
    },
];

const MODE_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "i",
        description: "Enter INSERT mode",
    },
    KeyBinding {
        key: "v",
        description: "Enter SELECT mode",
    },
    KeyBinding {
        key: "Esc",
        description: "Return to NORMAL mode (in NORMAL: close)",
    },
];

const NAVIGATION_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "h / Left",
        description: "Move cursor left",
    },
    KeyBinding {
        key: "l / Right",
        description: "Move cursor right",
    },
    KeyBinding {
        key: "k / Up",
        description: "Move cursor up (higher pitch)",
    },
    KeyBinding {
        key: "j / Down",
        description: "Move cursor down (lower pitch)",
    },
    KeyBinding {
        key: "H",
        description: "Jump left by measure",
    },
    KeyBinding {
        key: "L",
        description: "Jump right by measure",
    },
    KeyBinding {
        key: "0",
        description: "Go to start",
    },
    KeyBinding {
        key: "$",
        description: "Go to end",
    },
];

const EDIT_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "Enter / n",
        description: "Place note at cursor",
    },
    KeyBinding {
        key: "Delete",
        description: "Delete note at cursor",
    },
    KeyBinding {
        key: "W/A/S/D",
        description: "Move selected notes (up/left/down/right)",
    },
    KeyBinding {
        key: "Shift+A/D",
        description: "Shrink/expand note duration",
    },
];

const TRACK_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "a",
        description: "Add new track",
    },
    KeyBinding {
        key: "x / d",
        description: "Delete selected track",
    },
    KeyBinding {
        key: "r",
        description: "Rename selected track",
    },
    KeyBinding {
        key: "m",
        description: "Toggle mute on selected track",
    },
    KeyBinding {
        key: "s",
        description: "Toggle solo on selected track",
    },
    KeyBinding {
        key: "J / K",
        description: "Select next/previous track",
    },
    KeyBinding {
        key: "< / >",
        description: "Change instrument (GM)",
    },
    KeyBinding {
        key: "; / '",
        description: "Decrease/increase volume",
    },
    KeyBinding {
        key: "( / )",
        description: "Pan left/right",
    },
];

const KEYBOARD_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "Z-M / Q-I",
        description: "Play notes (piano layout)",
    },
    KeyBinding {
        key: ",",
        description: "Octave down",
    },
    KeyBinding {
        key: "/",
        description: "Octave up",
    },
];

const VIEW_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "g",
        description: "Cycle views (Combined/Piano/Timeline)",
    },
    KeyBinding {
        key: "t",
        description: "Toggle track list view (compact/expanded)",
    },
    KeyBinding {
        key: "W",
        description: "Toggle active track highlighting",
    },
    KeyBinding {
        key: "= / -",
        description: "Zoom in/out",
    },
    KeyBinding {
        key: "[ / ]",
        description: "Decrease/increase tempo (BPM)",
    },
    KeyBinding {
        key: "{ / }",
        description: "Decrease/increase time sig numerator",
    },
    KeyBinding {
        key: "|",
        description: "Cycle time sig denominator (2/4/8/16)",
    },
];

const FILE_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "Ctrl+n",
        description: "New project (with confirmation)",
    },
    KeyBinding {
        key: "Ctrl+s",
        description: "Save project (JSON / OXM / MIDI)",
    },
    KeyBinding {
        key: "Ctrl+o",
        description: "Open project",
    },
    KeyBinding {
        key: "Ctrl+l",
        description: "Load SoundFont (.sf2)",
    },
    KeyBinding {
        key: "e / Ctrl+e",
        description: "Export to WAV (music folder/output)",
    },
    KeyBinding {
        key: "Ctrl+m",
        description: "Export to MIDI (music folder/output)",
    },
];

const MOUSE_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "Click",
        description: "Focus panel / select item",
    },
    KeyBinding {
        key: "Double-click",
        description: "Toggle note / rename track",
    },
    KeyBinding {
        key: "Right-click",
        description: "Toggle Insert mode",
    },
    KeyBinding {
        key: "Drag",
        description: "Pan/scroll the view",
    },
    KeyBinding {
        key: "Shift+Click",
        description: "Multi-select notes",
    },
    KeyBinding {
        key: "Scroll",
        description: "Navigate pitch (vert) or time (horiz)",
    },
    KeyBinding {
        key: "Ctrl+Scroll",
        description: "Zoom in/out",
    },
    KeyBinding {
        key: "Piano keys",
        description: "Click to play notes",
    },
];

/// All help lines, sectioned like miditui's help overlay.
pub fn help_lines(pal: &Palette) -> Vec<Line<'static>> {
    let section_style = Style::default().fg(pal.accent).bg(pal.surface).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let key_style = pal.label();
    let desc_style = pal.fill();
    let sections: [(&'static str, &[KeyBinding]); 9] = [
        ("General", GENERAL_BINDINGS),
        ("Modes", MODE_BINDINGS),
        ("Navigation", NAVIGATION_BINDINGS),
        ("Editing", EDIT_BINDINGS),
        ("Tracks", TRACK_BINDINGS),
        ("Keyboard", KEYBOARD_BINDINGS),
        ("View", VIEW_BINDINGS),
        ("File & Export", FILE_BINDINGS),
        ("Mouse Controls", MOUSE_BINDINGS),
    ];
    let mut lines = Vec::new();
    for (title, bindings) in sections {
        lines.push(Line::from(Span::styled(title, section_style)));
        for binding in bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("{:17}", binding.key), key_style),
                Span::styled(binding.description, desc_style),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}
