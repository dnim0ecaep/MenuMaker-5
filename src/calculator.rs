//! Pocket Calculator: a DeskMate-style four-function calculator drawn right
//! inside its desktop box: a blue display with the number on the right, and
//! green buttons that cast black shadows, on a gray face.
//!
//! With the box selected: digits and `.`, `+ - * / %`, Enter or `=`,
//! Backspace (`<-`), `c` / Delete (clear), `_` (change sign). Buttons can
//! also be clicked with the mouse.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// The buttons, row by row, as in the DeskMate calculator.
pub const BUTTONS: [[&str; 4]; 5] = [
    ["C", "<-", "%", "+-"],
    ["7", "8", "9", "/"],
    ["4", "5", "6", "*"],
    ["1", "2", "3", "-"],
    ["0", ".", "=", "+"],
];

/// Rows the calculator needs inside its box: a blank row, the display, a
/// blank row, then each button row followed by its shadow row.
pub const BODY_ROWS: usize = 3 + BUTTONS.len() * 2;

/// The calculator face colors (the box's default colors).
pub const FACE_HEX: &str = "#AAAAAA";
pub const INK_HEX: &str = "#000000";

const BLUE: Color = Color::Rgb(0, 0, 170);
const GREEN: Color = Color::Rgb(0, 170, 0);
const BLACK: Color = Color::Rgb(0, 0, 0);
const WHITE: Color = Color::Rgb(255, 255, 255);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
pub struct Calculator {
    /// Digits being typed; empty when showing `value`.
    entry: String,
    /// The number on the display when not typing.
    value: f64,
    /// Left-hand side and operator waiting for the right-hand side.
    pending: Option<(f64, Op)>,
    error: bool,
}

impl Calculator {
    /// Start showing `start` (the box's last result).
    pub fn new(start: &str) -> Self {
        Self {
            entry: String::new(),
            value: start.trim().parse().unwrap_or(0.0),
            pending: None,
            error: false,
        }
    }

    /// The display text: the digits being typed, else the value with six
    /// decimals (as DeskMate shows it).
    pub fn display(&self) -> String {
        if self.error {
            return "Error".into();
        }
        if !self.entry.is_empty() {
            return self.entry.clone();
        }
        format_value(self.value)
    }

    fn current(&self) -> f64 {
        if self.entry.is_empty() {
            self.value
        } else {
            self.entry.parse().unwrap_or(0.0)
        }
    }

    /// Fold the typed number into the pending operation.
    fn settle(&mut self) {
        let right = self.current();
        self.entry.clear();
        self.value = match self.pending.take() {
            Some((left, op)) => match apply(left, op, right) {
                Some(v) => v,
                None => {
                    self.error = true;
                    0.0
                }
            },
            None => right,
        };
    }

    /// Press one button by its label.
    pub fn press(&mut self, label: &str) {
        if self.error && label != "C" {
            // Any key after an error starts over.
            self.clear();
        }
        match label {
            "C" => self.clear(),
            "<-" => {
                self.entry.pop();
                if self.entry == "-" {
                    self.entry.clear();
                }
            }
            "." => {
                if !self.entry.contains('.') {
                    if self.entry.is_empty() || self.entry == "-" {
                        self.entry.push('0');
                    }
                    self.entry.push('.');
                }
            }
            "+-" => {
                if self.entry.is_empty() {
                    self.value = -self.value;
                } else if let Some(rest) = self.entry.strip_prefix('-') {
                    self.entry = rest.to_string();
                } else {
                    self.entry.insert(0, '-');
                }
            }
            "%" => {
                let x = self.current();
                let percent = match self.pending {
                    // 200 + 10% = 220; 200 * 10% = 20.
                    Some((left, Op::Add | Op::Sub)) => left * x / 100.0,
                    _ => x / 100.0,
                };
                self.entry.clear();
                self.value = percent;
            }
            "=" => self.settle(),
            "+" | "-" | "*" | "/" => {
                let op = match label {
                    "+" => Op::Add,
                    "-" => Op::Sub,
                    "*" => Op::Mul,
                    _ => Op::Div,
                };
                if self.entry.is_empty() && self.pending.is_some() {
                    // Changing your mind about the operator.
                    if let Some((left, _)) = self.pending {
                        self.pending = Some((left, op));
                    }
                } else {
                    self.settle();
                    if !self.error {
                        self.pending = Some((self.value, op));
                    }
                }
            }
            digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                if self.entry.len() < 15 {
                    if self.entry == "0" {
                        self.entry.clear();
                    } else if self.entry == "-0" {
                        self.entry.truncate(1);
                    }
                    self.entry.push_str(digit);
                }
            }
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.entry.clear();
        self.value = 0.0;
        self.pending = None;
        self.error = false;
    }
}

/// The button a key presses while the calculator box is selected, if any.
/// Letters other than `c`/`x` are left for the desktop's own shortcuts.
pub fn key_label(key: KeyEvent) -> Option<&'static str> {
    let label = match key.code {
        KeyCode::Enter | KeyCode::Char('=') => "=",
        KeyCode::Backspace => "<-",
        KeyCode::Delete | KeyCode::Char('c') | KeyCode::Char('C') => "C",
        KeyCode::Char('_') => "+-",
        KeyCode::Char('x') | KeyCode::Char('X') => "*",
        KeyCode::Char(',') => ".",
        KeyCode::Char(c) => {
            let typed = c.to_string();
            return BUTTONS.iter().flatten().find(|b| **b == typed).copied();
        }
        _ => return None,
    };
    Some(label)
}

/// Where the display and the buttons go inside the box body.
struct Layout {
    display: Rect,
    /// Width of one button face.
    button: u16,
    /// Distance from one button's left edge to the next.
    step: u16,
    left: u16,
}

/// Widest button slot (face + shadow + gap), as in the DeskMate picture.
const MAX_STEP: u16 = 8;

fn layout(area: Rect) -> Layout {
    let margin = if area.width >= 30 { 2 } else { 1 };
    let usable = area.width.saturating_sub(margin * 2);
    let step = (usable / 4).clamp(3, MAX_STEP);
    // Each slot: face, one shadow column, and a gap.
    let button = step.saturating_sub(2).max(2);
    // The four buttons (the last one has no gap after its shadow), centered;
    // the display lines up with them.
    let used = step * 3 + button + 1;
    let left = area.x + area.width.saturating_sub(used) / 2;
    Layout {
        display: Rect {
            x: left,
            y: area.y + 1,
            width: used.min(area.width),
            height: 1,
        },
        button,
        step,
        left,
    }
}

/// Where button (`row`, `col`) sits inside the box body `area`.
pub fn button_rect(area: Rect, row: usize, col: usize) -> Rect {
    let l = layout(area);
    Rect {
        x: l.left + col as u16 * l.step,
        y: area.y + 3 + row as u16 * 2,
        width: l.button,
        height: 1,
    }
}

/// The button under (`x`, `y`) in the box body `area`.
pub fn button_at(area: Rect, x: u16, y: u16) -> Option<&'static str> {
    for (row, labels) in BUTTONS.iter().enumerate() {
        for (col, label) in labels.iter().enumerate() {
            let r = button_rect(area, row, col);
            if x >= r.x && x < r.x + r.width && y == r.y {
                return Some(label);
            }
        }
    }
    None
}

/// Draw the calculator into the box body `area` (already filled with the
/// box's gray face).
pub fn draw(buf: &mut Buffer, area: Rect, display: &str, face: Color) {
    let put = |buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style| {
        if y < area.y + area.height && x < area.x + area.width {
            let room = (area.x + area.width - x) as usize;
            buf.set_stringn(x, y, text, room, style);
        }
    };
    let l = layout(area);
    if l.display.width > 1 {
        let room = l.display.width as usize - 1;
        let shown: String = if display.len() > room {
            display[display.len() - room..].to_string()
        } else {
            display.to_string()
        };
        put(
            buf,
            l.display.x,
            l.display.y,
            &format!("{shown:>room$} "),
            Style::default().bg(BLUE).fg(WHITE).add_modifier(Modifier::BOLD),
        );
    }
    let shadow = Style::default().fg(BLACK).bg(face);
    for (row, labels) in BUTTONS.iter().enumerate() {
        for (col, label) in labels.iter().enumerate() {
            let r = button_rect(area, row, col);
            // Black shadow down and to the right, half a row deep.
            put(buf, r.x + r.width, r.y, "▄", shadow);
            put(buf, r.x + 1, r.y + 1, &"▀".repeat(r.width as usize), shadow);
            let face_text = if r.width > 2 {
                format!(" {:<w$}", label, w = r.width as usize - 1)
            } else {
                format!("{:<w$}", label, w = r.width as usize)
            };
            put(
                buf,
                r.x,
                r.y,
                &face_text,
                Style::default().bg(GREEN).fg(BLACK).add_modifier(Modifier::BOLD),
            );
        }
    }
}

fn apply(left: f64, op: Op, right: f64) -> Option<f64> {
    let v = match op {
        Op::Add => left + right,
        Op::Sub => left - right,
        Op::Mul => left * right,
        Op::Div if right == 0.0 => return None,
        Op::Div => left / right,
    };
    v.is_finite().then_some(v)
}

/// A value as the calculator shows it: six decimals.
pub fn format_value(v: f64) -> String {
    let text = format!("{v:.6}");
    if text == "-0.000000" {
        "0.000000".into()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn run(keys: &[&str]) -> String {
        let mut calc = Calculator::new("");
        for key in keys {
            calc.press(key);
        }
        calc.display()
    }

    #[test]
    fn basic_arithmetic() {
        assert_eq!(run(&["4", "/", "5", "="]), "0.800000");
        assert_eq!(run(&["1", "2", "+", "3", "*", "2", "="]), "30.000000");
        assert_eq!(run(&["7", "-", "9", "="]), "-2.000000");
    }

    #[test]
    fn entry_editing_and_sign() {
        assert_eq!(run(&["1", "2", "3", "<-"]), "12");
        assert_eq!(run(&[".", "5"]), "0.5");
        assert_eq!(run(&["5", "+-"]), "-5");
        assert_eq!(run(&["5", "+-", "+-"]), "5");
        assert_eq!(run(&["1", ".", ".", "2"]), "1.2");
    }

    #[test]
    fn percent_clear_and_errors() {
        assert_eq!(run(&["2", "0", "0", "+", "1", "0", "%", "="]), "220.000000");
        assert_eq!(run(&["5", "0", "%"]), "0.500000");
        assert_eq!(run(&["1", "/", "0", "="]), "Error");
        assert_eq!(run(&["1", "/", "0", "=", "3"]), "3");
        assert_eq!(run(&["9", "+", "C"]), "0.000000");
        // Changing the operator before typing the next number.
        assert_eq!(run(&["6", "+", "*", "2", "="]), "12.000000");
    }

    #[test]
    fn starts_from_remembered_value() {
        assert_eq!(Calculator::new("0.800000").display(), "0.800000");
    }

    #[test]
    fn keys_map_to_buttons() {
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        assert_eq!(key_label(k(KeyCode::Char('7'))), Some("7"));
        assert_eq!(key_label(k(KeyCode::Char('/'))), Some("/"));
        assert_eq!(key_label(k(KeyCode::Enter)), Some("="));
        assert_eq!(key_label(k(KeyCode::Backspace)), Some("<-"));
        // Desktop shortcuts are left alone.
        assert_eq!(key_label(k(KeyCode::Char('e'))), None);
        assert_eq!(key_label(k(KeyCode::Char('n'))), None);
    }

    #[test]
    fn buttons_fit_inside_the_box_body() {
        for width in [20u16, 30, 36, 38, 60] {
            let area = Rect::new(10, 5, width, BODY_ROWS as u16);
            let last = button_rect(area, 4, 3);
            // Face and shadow stay inside the body, rows included.
            assert!(last.x + last.width < area.x + area.width, "width {width}");
            assert!(last.y + 1 < area.y + area.height);
            assert_eq!(button_at(area, last.x, last.y), Some("+"));
            let first = button_rect(area, 0, 0);
            assert!(first.x >= area.x);
            // Buttons stay DeskMate-sized however wide the box is.
            assert!(first.width <= MAX_STEP - 2);
            assert_eq!(button_at(area, first.x, first.y), Some("C"));
            // Shadow cells are not buttons.
            assert_eq!(button_at(area, first.x + first.width, first.y), None);
        }
    }
}
