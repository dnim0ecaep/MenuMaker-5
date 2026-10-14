//! Spreadsheet box: a small grid of cells saved as CSV, with formulas.
//!
//! Cells hold plain text, numbers, or formulas starting with `=`. Formulas
//! support `+ - * / ^`, `&` (join text), comparisons (`= <> < > <= >=`),
//! cell references (`A1`, `AB12`), ranges (`A1:B5`) inside functions, and
//! the functions SUM, AVERAGE/AVG, MIN, MAX, COUNT, COUNTA, PRODUCT, ABS,
//! ROUND, SQRT, IF, AND, OR, NOT. Errors show as `#REF!`, `#DIV/0!`,
//! `#VALUE!`, `#NAME?`, `#CYCLE!` or `#ERR!` and never crash the app.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::dmui::{
    confirm_click, dm_box, fit_width, is_left_click, key, pad_width, put_line, render_confirm, screen_chrome,
    shortcut_click, Hits, Palette,
};

/// Hard limits so a typo like `A1:ZZZ99999` cannot freeze the desktop.
const MAX_ROWS: usize = 10_000;
const MAX_COLS: usize = 702; // A..ZZ
const MAX_RANGE_CELLS: usize = 100_000;
const MAX_DEPTH: usize = 256;

const COL_WIDTH: usize = 11;
const ROW_HEADER: usize = 5;

// ---------------------------------------------------------------- values

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Num(f64),
    Text(String),
    Bool(bool),
    Err(&'static str),
}

impl Value {
    pub fn display(&self) -> String {
        match self {
            Value::Empty => String::new(),
            Value::Num(n) => format_number(*n),
            Value::Text(t) => t.clone(),
            Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            Value::Err(e) => (*e).to_string(),
        }
    }

    fn is_numeric(&self) -> bool {
        matches!(self, Value::Num(_))
    }

    fn as_num(&self) -> Result<f64, &'static str> {
        match self {
            Value::Empty => Ok(0.0),
            Value::Num(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::Text(t) => t.trim().parse::<f64>().map_err(|_| "#VALUE!"),
            Value::Err(e) => Err(e),
        }
    }

    fn as_bool(&self) -> Result<bool, &'static str> {
        match self {
            Value::Bool(b) => Ok(*b),
            Value::Text(t) if t.eq_ignore_ascii_case("true") => Ok(true),
            Value::Text(t) if t.eq_ignore_ascii_case("false") => Ok(false),
            other => other.as_num().map(|n| n != 0.0),
        }
    }

    fn as_text(&self) -> Result<String, &'static str> {
        match self {
            Value::Err(e) => Err(e),
            other => Ok(other.display()),
        }
    }
}

fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "#NUM!".into();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let s = format!("{n:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

// ---------------------------------------------------------------- cell names

/// `0 -> A`, `25 -> Z`, `26 -> AA`.
pub fn col_name(mut col: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push((b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.iter().rev().collect()
}

/// Parse `B12` (case-insensitive, optional `$`) into `(row, col)`, 0-based.
pub fn parse_ref(name: &str) -> Option<(usize, usize)> {
    let name = name.replace('$', "");
    let letters: String = name.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &name[letters.len()..];
    if letters.is_empty() || letters.len() > 2 || digits.is_empty() {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut col = 0usize;
    for ch in letters.chars() {
        col = col * 26 + (ch.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    let row: usize = digits.parse().ok()?;
    if row == 0 || row > MAX_ROWS || col == 0 || col > MAX_COLS {
        return None;
    }
    Some((row - 1, col - 1))
}

// ---------------------------------------------------------------- parser

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Colon,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, &'static str> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            out.push(Tok::Num(text.parse().map_err(|_| "#VALUE!")?));
        } else if c.is_ascii_alphabetic() || c == '$' || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '$' || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect()));
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            loop {
                match chars.get(i) {
                    None => return Err("#ERR!"),
                    Some('"') if chars.get(i + 1) == Some(&'"') => {
                        s.push('"');
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(ch) => {
                        s.push(*ch);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(s));
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            let op2 = match two.as_str() {
                "<=" => Some("<="),
                ">=" => Some(">="),
                "<>" => Some("<>"),
                "!=" => Some("<>"),
                "==" => Some("="),
                _ => None,
            };
            if let Some(op) = op2 {
                out.push(Tok::Op(op));
                i += 2;
                continue;
            }
            out.push(match c {
                '+' => Tok::Op("+"),
                '-' => Tok::Op("-"),
                '*' => Tok::Op("*"),
                '/' => Tok::Op("/"),
                '^' => Tok::Op("^"),
                '&' => Tok::Op("&"),
                '=' => Tok::Op("="),
                '<' => Tok::Op("<"),
                '>' => Tok::Op(">"),
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                ',' | ';' => Tok::Comma,
                ':' => Tok::Colon,
                _ => return Err("#ERR!"),
            });
            i += 1;
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Ref(usize, usize),
    Range(usize, usize, usize, usize),
    Neg(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat_op(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        if let Some(Tok::Op(op)) = self.peek() {
            if let Some(found) = ops.iter().find(|o| *o == op) {
                self.pos += 1;
                return Some(found);
            }
        }
        None
    }

    fn enter(&mut self) -> Result<(), &'static str> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err("#ERR!")
        } else {
            Ok(())
        }
    }

    fn comparison(&mut self) -> Result<Expr, &'static str> {
        self.enter()?;
        let mut left = self.concat()?;
        while let Some(op) = self.eat_op(&["=", "<>", "<", ">", "<=", ">="]) {
            let right = self.concat()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        self.depth -= 1;
        Ok(left)
    }

    fn concat(&mut self) -> Result<Expr, &'static str> {
        let mut left = self.additive()?;
        while let Some(op) = self.eat_op(&["&"]) {
            let right = self.additive()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, &'static str> {
        let mut left = self.term()?;
        while let Some(op) = self.eat_op(&["+", "-"]) {
            let right = self.term()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Expr, &'static str> {
        let mut left = self.power()?;
        while let Some(op) = self.eat_op(&["*", "/"]) {
            let right = self.power()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn power(&mut self) -> Result<Expr, &'static str> {
        let base = self.unary()?;
        if self.eat_op(&["^"]).is_some() {
            let exp = self.power()?;
            return Ok(Expr::Bin("^", Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Expr, &'static str> {
        self.enter()?;
        let out = if self.eat_op(&["-"]).is_some() {
            Expr::Neg(Box::new(self.unary()?))
        } else if self.eat_op(&["+"]).is_some() {
            self.unary()?
        } else {
            self.primary()?
        };
        self.depth -= 1;
        Ok(out)
    }

    fn primary(&mut self) -> Result<Expr, &'static str> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::LParen) => {
                let e = self.comparison()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err("#ERR!"),
                }
            }
            Some(Tok::Ident(name)) => {
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::RParen) {
                        self.pos += 1;
                    } else {
                        loop {
                            args.push(self.comparison()?);
                            match self.next() {
                                Some(Tok::Comma) => continue,
                                Some(Tok::RParen) => break,
                                _ => return Err("#ERR!"),
                            }
                        }
                    }
                    return Ok(Expr::Call(name.to_ascii_uppercase(), args));
                }
                if name.eq_ignore_ascii_case("TRUE") {
                    return Ok(Expr::Bool(true));
                }
                if name.eq_ignore_ascii_case("FALSE") {
                    return Ok(Expr::Bool(false));
                }
                let (r1, c1) = parse_ref(&name).ok_or("#NAME?")?;
                if self.peek() == Some(&Tok::Colon) {
                    self.pos += 1;
                    let Some(Tok::Ident(end)) = self.next() else {
                        return Err("#REF!");
                    };
                    let (r2, c2) = parse_ref(&end).ok_or("#REF!")?;
                    return Ok(Expr::Range(r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)));
                }
                Ok(Expr::Ref(r1, c1))
            }
            _ => Err("#ERR!"),
        }
    }
}

fn parse_formula(src: &str) -> Result<Expr, &'static str> {
    let toks = tokenize(src)?;
    let mut parser = Parser { toks, pos: 0, depth: 0 };
    let expr = parser.comparison()?;
    if parser.pos != parser.toks.len() {
        return Err("#ERR!");
    }
    Ok(expr)
}

// ---------------------------------------------------------------- grid + evaluation

/// Raw cell text, row-major. Rows may have different lengths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    pub cells: Vec<Vec<String>>,
}

impl Grid {
    pub fn get(&self, row: usize, col: usize) -> &str {
        self.cells
            .get(row)
            .and_then(|r| r.get(col))
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn set(&mut self, row: usize, col: usize, text: String) {
        if row >= MAX_ROWS || col >= MAX_COLS {
            return;
        }
        if text.is_empty() && self.get(row, col).is_empty() {
            return;
        }
        if self.cells.len() <= row {
            self.cells.resize(row + 1, Vec::new());
        }
        let line = &mut self.cells[row];
        if line.len() <= col {
            line.resize(col + 1, String::new());
        }
        line[col] = text;
        self.trim();
    }

    fn trim(&mut self) {
        for row in &mut self.cells {
            while row.last().is_some_and(|c| c.is_empty()) {
                row.pop();
            }
        }
        while self.cells.last().is_some_and(|r| r.is_empty()) {
            self.cells.pop();
        }
    }

    pub fn rows(&self) -> usize {
        self.cells.len()
    }

    pub fn cols(&self) -> usize {
        self.cells.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Evaluate every cell. Shared memo, per-cell cycle detection.
    pub fn evaluate(&self) -> Vec<Vec<Value>> {
        let mut ev = Evaluator { grid: self, memo: HashMap::new(), visiting: HashSet::new() };
        self.cells
            .iter()
            .enumerate()
            .map(|(r, line)| (0..line.len()).map(|c| ev.cell(r, c)).collect())
            .collect()
    }

    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path).map(|t| Self::from_csv(&t)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write to a temp file and rename, so a crash never leaves a
        // half-written sheet.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        fs::write(&tmp, self.to_csv())?;
        fs::rename(&tmp, path)
    }

    pub fn from_csv(text: &str) -> Self {
        let mut cells = Vec::new();
        let mut row = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if quoted {
                match c {
                    '"' if chars.peek() == Some(&'"') => {
                        field.push('"');
                        chars.next();
                    }
                    '"' => quoted = false,
                    _ => field.push(c),
                }
                continue;
            }
            match c {
                '"' if field.is_empty() => quoted = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\r' => {}
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    cells.push(std::mem::take(&mut row));
                }
                _ => field.push(c),
            }
        }
        if !field.is_empty() || !row.is_empty() {
            row.push(field);
            cells.push(row);
        }
        let mut grid = Grid { cells };
        grid.cells.truncate(MAX_ROWS);
        for r in &mut grid.cells {
            r.truncate(MAX_COLS);
        }
        grid.trim();
        grid
    }

    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        for row in &self.cells {
            let line: Vec<String> = row
                .iter()
                .map(|f| {
                    if f.contains([',', '"', '\n', '\r']) {
                        format!("\"{}\"", f.replace('"', "\"\""))
                    } else {
                        f.clone()
                    }
                })
                .collect();
            out.push_str(&line.join(","));
            out.push('\n');
        }
        out
    }
}

struct Evaluator<'a> {
    grid: &'a Grid,
    memo: HashMap<(usize, usize), Value>,
    visiting: HashSet<(usize, usize)>,
}

impl Evaluator<'_> {
    fn cell(&mut self, r: usize, c: usize) -> Value {
        if let Some(v) = self.memo.get(&(r, c)) {
            return v.clone();
        }
        if !self.visiting.insert((r, c)) {
            return Value::Err("#CYCLE!");
        }
        let raw = self.grid.get(r, c).to_string();
        let value = literal_or_formula(&raw, |expr| self.eval(expr));
        self.visiting.remove(&(r, c));
        self.memo.insert((r, c), value.clone());
        value
    }

    fn eval(&mut self, expr: &Expr) -> Value {
        match expr {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Text(s.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Ref(r, c) => match self.cell(*r, *c) {
                Value::Empty => Value::Num(0.0),
                v => v,
            },
            Expr::Range(..) => Value::Err("#VALUE!"),
            Expr::Neg(e) => match self.eval(e).as_num() {
                Ok(n) => Value::Num(-n),
                Err(e) => Value::Err(e),
            },
            Expr::Bin(op, a, b) => {
                let (a, b) = (self.eval(a), self.eval(b));
                binary(op, &a, &b)
            }
            Expr::Call(name, args) => self.call(name, args),
        }
    }

    /// Flatten arguments (ranges expand to their cells) for aggregate functions.
    fn flatten(&mut self, args: &[Expr]) -> Result<Vec<Value>, &'static str> {
        let mut out = Vec::new();
        for arg in args {
            if let Expr::Range(r1, c1, r2, c2) = arg {
                let count = (r2 - r1 + 1).saturating_mul(c2 - c1 + 1);
                if count > MAX_RANGE_CELLS {
                    return Err("#REF!");
                }
                for r in *r1..=*r2 {
                    for c in *c1..=*c2 {
                        let v = self.cell(r, c);
                        if let Value::Err(e) = v {
                            return Err(e);
                        }
                        out.push(v);
                    }
                }
            } else {
                match self.eval(arg) {
                    Value::Err(e) => return Err(e),
                    v => out.push(v),
                }
            }
        }
        Ok(out)
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> Value {
        let numbers = |vals: &[Value]| -> Vec<f64> {
            vals.iter().filter_map(|v| if v.is_numeric() { v.as_num().ok() } else { None }).collect()
        };
        let result: Result<Value, &'static str> = (|| match name {
            "SUM" => Ok(Value::Num(numbers(&self.flatten(args)?).iter().sum())),
            "PRODUCT" => Ok(Value::Num(numbers(&self.flatten(args)?).iter().product())),
            "AVERAGE" | "AVG" | "MEAN" => {
                let n = numbers(&self.flatten(args)?);
                if n.is_empty() {
                    Err("#DIV/0!")
                } else {
                    Ok(Value::Num(n.iter().sum::<f64>() / n.len() as f64))
                }
            }
            "MIN" => Ok(Value::Num(numbers(&self.flatten(args)?).into_iter().reduce(f64::min).unwrap_or(0.0))),
            "MAX" => Ok(Value::Num(numbers(&self.flatten(args)?).into_iter().reduce(f64::max).unwrap_or(0.0))),
            "COUNT" => Ok(Value::Num(numbers(&self.flatten(args)?).len() as f64)),
            "COUNTA" => Ok(Value::Num(
                self.flatten(args)?.iter().filter(|v| **v != Value::Empty).count() as f64,
            )),
            "ABS" | "SQRT" | "ROUND" | "INT" => {
                let x = self.arg(args, 0)?.as_num()?;
                Ok(Value::Num(match name {
                    "ABS" => x.abs(),
                    "INT" => x.floor(),
                    "SQRT" if x < 0.0 => return Err("#NUM!"),
                    "SQRT" => x.sqrt(),
                    _ => {
                        let digits = match args.get(1) {
                            Some(_) => self.arg(args, 1)?.as_num()?.clamp(-15.0, 15.0) as i32,
                            None => 0,
                        };
                        let f = 10f64.powi(digits);
                        (x * f).round() / f
                    }
                }))
            }
            "IF" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err("#VALUE!");
                }
                if self.arg(args, 0)?.as_bool()? {
                    self.arg(args, 1)
                } else if args.len() == 3 {
                    self.arg(args, 2)
                } else {
                    Ok(Value::Bool(false))
                }
            }
            "AND" => {
                let vals = self.flatten(args)?;
                vals.iter().try_fold(true, |acc, v| Ok(acc && v.as_bool()?)).map(Value::Bool)
            }
            "OR" => {
                let vals = self.flatten(args)?;
                vals.iter().try_fold(false, |acc, v| Ok(acc || v.as_bool()?)).map(Value::Bool)
            }
            "NOT" => Ok(Value::Bool(!self.arg(args, 0)?.as_bool()?)),
            _ => Err("#NAME?"),
        })();
        result.unwrap_or_else(Value::Err)
    }

    fn arg(&mut self, args: &[Expr], i: usize) -> Result<Value, &'static str> {
        let expr = args.get(i).ok_or("#VALUE!")?;
        match self.eval(expr) {
            Value::Err(e) => Err(e),
            v => Ok(v),
        }
    }
}

fn literal_or_formula(raw: &str, eval: impl FnOnce(&Expr) -> Value) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Empty;
    }
    if let Some(formula) = trimmed.strip_prefix('=') {
        return match parse_formula(formula) {
            Ok(expr) => eval(&expr),
            Err(e) => Value::Err(e),
        };
    }
    match trimmed.parse::<f64>() {
        Ok(n) if n.is_finite() => Value::Num(n),
        _ => Value::Text(raw.to_string()),
    }
}

fn binary(op: &str, a: &Value, b: &Value) -> Value {
    if let Value::Err(e) = a {
        return Value::Err(e);
    }
    if let Value::Err(e) = b {
        return Value::Err(e);
    }
    if op == "&" {
        return match (a.as_text(), b.as_text()) {
            (Ok(x), Ok(y)) => Value::Text(x + &y),
            (Err(e), _) | (_, Err(e)) => Value::Err(e),
        };
    }
    if matches!(op, "=" | "<>" | "<" | ">" | "<=" | ">=") {
        let ord = match (a.as_num(), b.as_num()) {
            (Ok(x), Ok(y)) if !matches!(a, Value::Text(_)) || !matches!(b, Value::Text(_)) => {
                x.partial_cmp(&y)
            }
            _ => Some(a.display().to_lowercase().cmp(&b.display().to_lowercase())),
        };
        let Some(ord) = ord else { return Value::Err("#NUM!") };
        use std::cmp::Ordering::*;
        return Value::Bool(match op {
            "=" => ord == Equal,
            "<>" => ord != Equal,
            "<" => ord == Less,
            ">" => ord == Greater,
            "<=" => ord != Greater,
            _ => ord != Less,
        });
    }
    let (x, y) = match (a.as_num(), b.as_num()) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => return Value::Err(e),
    };
    let n = match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" if y == 0.0 => return Value::Err("#DIV/0!"),
        "/" => x / y,
        "^" => x.powf(y),
        _ => return Value::Err("#ERR!"),
    };
    if n.is_finite() {
        Value::Num(n)
    } else {
        Value::Err("#NUM!")
    }
}

// ---------------------------------------------------------------- box preview

/// A few rows of the sheet for the desktop box (values, not formulas).
pub fn preview_lines(path: &Path) -> Vec<(String, Option<(usize, usize)>)> {
    let grid = Grid::load(path);
    if grid.rows() == 0 {
        return Vec::new();
    }
    let values = grid.evaluate();
    let cols = grid.cols().min(4);
    values
        .iter()
        .take(12)
        .map(|row| {
            let cells: Vec<String> = (0..cols)
                .map(|c| pad_width(&row.get(c).map(Value::display).unwrap_or_default(), 8))
                .collect();
            (cells.join("│").trim_end().to_string(), None)
        })
        .collect()
}

// ---------------------------------------------------------------- full-screen app

pub enum AppResult {
    Continue,
    Close,
}

const SAVE_LABEL: &str = "[ Save ]";
const CLOSE_PROMPT: &str = "Save changes before closing?";

#[derive(Clone, Copy)]
enum SheetHit {
    Cell(usize, usize),
    ColHeader(usize),
    RowHeader(usize),
    FormulaBar,
    Save,
}

pub struct SpreadsheetApp {
    pub box_name: String,
    path: PathBuf,
    grid: Grid,
    values: Vec<Vec<Value>>,
    row: usize,
    col: usize,
    /// In-cell editor: text and cursor (in chars).
    edit: Option<(String, usize)>,
    clipboard: Option<String>,
    status: String,
    top: Cell<usize>,
    left: Cell<usize>,
    /// Unsaved edits since the last save.
    dirty: bool,
    /// The "save before closing?" question is showing.
    confirm_close: bool,
    hits: Hits<SheetHit>,
}

impl SpreadsheetApp {
    pub fn open(box_name: &str, path: PathBuf) -> Self {
        let grid = Grid::load(&path);
        let values = grid.evaluate();
        Self {
            box_name: box_name.to_string(),
            status: format!("Sheet file: {}", path.display()),
            path,
            grid,
            values,
            row: 0,
            col: 0,
            edit: None,
            clipboard: None,
            top: Cell::new(0),
            left: Cell::new(0),
            dirty: false,
            confirm_close: false,
            hits: Hits::default(),
        }
    }

    /// Write the sheet to its CSV file (the Save button / Ctrl+S).
    pub fn save(&mut self) -> bool {
        match self.grid.save(&self.path) {
            Ok(()) => {
                self.dirty = false;
                self.status = format!("Saved {}", self.path.display());
                true
            }
            Err(err) => {
                self.status = format!("Could not save {}: {err}", self.path.display());
                false
            }
        }
    }

    /// Commit any open cell edit (used before saving or clicking away).
    fn finish_edit(&mut self) {
        if let Some((text, _)) = self.edit.take() {
            self.commit(text);
        }
    }

    /// Mouse: click a cell to select it (click it again to edit), click a
    /// column / row header to jump there, click the formula bar to edit,
    /// click [ Save ] to save; the wheel scrolls (Shift+wheel sideways);
    /// the shortcut bar and the close question are clickable.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> AppResult {
        if self.confirm_close {
            if let Some(k) = confirm_click(area, CLOSE_PROMPT, mouse) {
                return self.handle_key(k);
            }
            return AppResult::Continue;
        }
        if let Some(k) = shortcut_click(area, self.shortcuts(), mouse) {
            return self.handle_key(k);
        }
        let sideways = mouse.modifiers.contains(KeyModifiers::SHIFT);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.finish_edit();
                let step: isize = if mouse.kind == MouseEventKind::ScrollUp { -3 } else { 3 };
                if sideways {
                    self.move_by(0, step.signum());
                } else {
                    self.move_by(step, 0);
                }
                return AppResult::Continue;
            }
            _ if !is_left_click(mouse) => return AppResult::Continue,
            _ => {}
        }
        match self.hits.at(mouse.column, mouse.row) {
            Some(SheetHit::Save) => {
                self.finish_edit();
                self.save();
            }
            Some(SheetHit::FormulaBar) => {
                if self.edit.is_none() {
                    return self.handle_key(key(KeyCode::F(2)));
                }
            }
            Some(SheetHit::Cell(r, c)) => {
                if self.edit.is_none() && (r, c) == (self.row, self.col) {
                    return self.handle_key(key(KeyCode::F(2)));
                }
                self.finish_edit();
                self.row = r;
                self.col = c;
            }
            Some(SheetHit::ColHeader(c)) => {
                self.finish_edit();
                self.col = c;
            }
            Some(SheetHit::RowHeader(r)) => {
                self.finish_edit();
                self.row = r;
            }
            None => {}
        }
        AppResult::Continue
    }

    fn shortcuts(&self) -> &'static [(&'static str, &'static str)] {
        if self.confirm_close {
            &[("y", "Save & Close"), ("n", "Discard"), ("Esc", "Keep Editing")]
        } else if self.edit.is_some() {
            &[("Enter", "Save Cell"), ("Tab", "Next Cell"), ("Esc", "Cancel"), ("←/→", "Cursor"), ("^s", "Save File")]
        } else {
            &[
                ("Esc", "Back"),
                ("Enter", "Edit"),
                ("Del", "Clear"),
                ("^c", "Copy"),
                ("^x", "Cut"),
                ("^v", "Paste"),
                ("^s", "Save"),
                ("=", "Formula"),
            ]
        }
    }

    fn cell_name(&self) -> String {
        format!("{}{}", col_name(self.col), self.row + 1)
    }

    fn value(&self, r: usize, c: usize) -> Value {
        self.values.get(r).and_then(|line| line.get(c)).cloned().unwrap_or(Value::Empty)
    }

    fn commit(&mut self, text: String) {
        if self.grid.get(self.row, self.col) != text {
            self.dirty = true;
        }
        self.grid.set(self.row, self.col, text);
        self.values = self.grid.evaluate();
        let unsaved = if self.dirty { "   (not saved - ^s or [ Save ])" } else { "" };
        self.status = format!("{} = {}{unsaved}", self.cell_name(), self.value(self.row, self.col).display());
    }

    fn move_by(&mut self, dr: isize, dc: isize) {
        self.row = (self.row as isize + dr).clamp(0, MAX_ROWS as isize - 1) as usize;
        self.col = (self.col as isize + dc).clamp(0, MAX_COLS as isize - 1) as usize;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.confirm_close {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_close = false;
                    if self.save() {
                        return AppResult::Close;
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => return AppResult::Close,
                KeyCode::Esc => {
                    self.confirm_close = false;
                    self.status = "Still editing - ^s saves".into();
                }
                _ => {}
            }
            return AppResult::Continue;
        }
        if ctrl && key.code == KeyCode::Char('s') {
            self.finish_edit();
            self.save();
            return AppResult::Continue;
        }
        if let Some((mut text, mut cur)) = self.edit.take() {
            let len = text.chars().count();
            let byte = |t: &str, i: usize| t.char_indices().nth(i).map(|(b, _)| b).unwrap_or(t.len());
            match key.code {
                KeyCode::Esc => {
                    self.status = "Edit cancelled".into();
                    return AppResult::Continue;
                }
                KeyCode::Enter | KeyCode::Down => {
                    self.commit(text);
                    self.move_by(1, 0);
                    return AppResult::Continue;
                }
                KeyCode::Up => {
                    self.commit(text);
                    self.move_by(-1, 0);
                    return AppResult::Continue;
                }
                KeyCode::Tab => {
                    self.commit(text);
                    self.move_by(0, 1);
                    return AppResult::Continue;
                }
                KeyCode::BackTab => {
                    self.commit(text);
                    self.move_by(0, -1);
                    return AppResult::Continue;
                }
                KeyCode::Left => cur = cur.saturating_sub(1),
                KeyCode::Right => cur = (cur + 1).min(len),
                KeyCode::Home => cur = 0,
                KeyCode::End => cur = len,
                KeyCode::Backspace if cur > 0 => {
                    let b = byte(&text, cur - 1);
                    text.remove(b);
                    cur -= 1;
                }
                KeyCode::Delete if cur < len => {
                    let b = byte(&text, cur);
                    text.remove(b);
                }
                KeyCode::Char(c) if !ctrl => {
                    let b = byte(&text, cur);
                    text.insert(b, c);
                    cur += 1;
                }
                _ => {}
            }
            self.edit = Some((text, cur));
            return AppResult::Continue;
        }

        match key.code {
            KeyCode::Esc if self.dirty => {
                self.confirm_close = true;
            }
            KeyCode::Esc => return AppResult::Close,
            KeyCode::Up => self.move_by(-1, 0),
            KeyCode::Down => self.move_by(1, 0),
            KeyCode::Left | KeyCode::BackTab => self.move_by(0, -1),
            KeyCode::Right | KeyCode::Tab => self.move_by(0, 1),
            KeyCode::PageUp => self.move_by(-10, 0),
            KeyCode::PageDown => self.move_by(10, 0),
            KeyCode::Home if ctrl => {
                self.row = 0;
                self.col = 0;
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.grid.cells.get(self.row).map_or(0, |r| r.len().saturating_sub(1)),
            KeyCode::Enter | KeyCode::F(2) => {
                let raw = self.grid.get(self.row, self.col).to_string();
                let len = raw.chars().count();
                self.edit = Some((raw, len));
                self.status = "Editing - Enter saves, Esc cancels".into();
            }
            KeyCode::Delete | KeyCode::Backspace => {
                self.commit(String::new());
            }
            KeyCode::Char('c') if ctrl => {
                self.clipboard = Some(self.grid.get(self.row, self.col).to_string());
                self.status = format!("Copied {}", self.cell_name());
            }
            KeyCode::Char('x') if ctrl => {
                self.clipboard = Some(self.grid.get(self.row, self.col).to_string());
                self.commit(String::new());
                self.status = format!("Cut {}", self.cell_name());
            }
            KeyCode::Char('v') if ctrl => {
                if let Some(text) = self.clipboard.clone() {
                    self.commit(text);
                }
            }
            KeyCode::Char(c) if !ctrl => {
                self.edit = Some((c.to_string(), 1));
                self.status = "Editing - Enter saves, Esc cancels".into();
            }
            _ => {}
        }
        AppResult::Continue
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, title: &str, pal: &Palette) {
        self.hits.clear();
        let modified = if self.dirty { " *modified*" } else { "" };
        let desk = screen_chrome(
            frame,
            area,
            &format!("{title} - {}{modified}", self.box_name),
            self.shortcuts(),
            &self.status,
            pal,
        );
        if desk.width < 24 || desk.height < 6 {
            return;
        }
        let file = self.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        let inner = dm_box(frame, desk, &format!("SHEET  {file}{modified}"), pal, pal.fill(), true);
        if inner.height < 3 {
            return;
        }
        // The Save button sits at the right end of the formula bar.
        let save_w = SAVE_LABEL.len() as u16;
        let save_rect = Rect { x: inner.x + inner.width.saturating_sub(save_w), y: inner.y, width: save_w.min(inner.width), height: 1 };
        let bar_rect = Rect { width: inner.width.saturating_sub(save_w + 1), height: 1, ..inner };

        // Formula bar.
        let (content, cursor) = match &self.edit {
            Some((text, cur)) => (text.clone(), Some(*cur)),
            None => (self.grid.get(self.row, self.col).to_string(), None),
        };
        let label = format!(" {:<6}│ ", self.cell_name());
        let mut bar = vec![Span::styled(label.clone(), pal.label())];
        match cursor {
            Some(cur) => {
                let before: String = content.chars().take(cur).collect();
                let at: String = content.chars().nth(cur).map(String::from).unwrap_or_else(|| " ".into());
                let after: String = content.chars().skip(cur + 1).collect();
                bar.push(Span::styled(before, pal.fill()));
                bar.push(Span::styled(at, pal.selected()));
                bar.push(Span::styled(after, pal.fill()));
            }
            None => bar.push(Span::styled(content, pal.fill())),
        }
        frame.render_widget(Paragraph::new(Line::from(bar)).style(pal.fill()), bar_rect);
        let save_style = if self.dirty { pal.selected() } else { pal.label().add_modifier(Modifier::REVERSED) };
        frame.render_widget(Paragraph::new(SAVE_LABEL).style(save_style), save_rect);
        self.hits.push(bar_rect, SheetHit::FormulaBar);
        self.hits.push(save_rect, SheetHit::Save);
        put_line(frame, inner, 1, &"─".repeat(inner.width as usize), pal.fill());

        // Grid area.
        let grid_area = Rect { y: inner.y + 2, height: inner.height - 2, ..inner };
        let vis_cols = ((grid_area.width as usize).saturating_sub(ROW_HEADER) / COL_WIDTH).max(1);
        let vis_rows = (grid_area.height as usize).saturating_sub(1).max(1);
        let (mut top, mut left) = (self.top.get(), self.left.get());
        if self.row < top {
            top = self.row;
        } else if self.row >= top + vis_rows {
            top = self.row + 1 - vis_rows;
        }
        if self.col < left {
            left = self.col;
        } else if self.col >= left + vis_cols {
            left = self.col + 1 - vis_cols;
        }
        self.top.set(top);
        self.left.set(left);

        let header_style = pal.label().add_modifier(Modifier::BOLD);
        let mut header = vec![Span::styled(" ".repeat(ROW_HEADER), header_style)];
        for c in left..left + vis_cols {
            let name = col_name(c);
            let style = if c == self.col { pal.selected() } else { header_style };
            header.push(Span::styled(format!("{:^w$}", name, w = COL_WIDTH), style));
            let x = grid_area.x + (ROW_HEADER + (c - left) * COL_WIDTH) as u16;
            self.hits.push(Rect { x, y: grid_area.y, width: COL_WIDTH as u16, height: 1 }, SheetHit::ColHeader(c));
        }
        frame.render_widget(Paragraph::new(Line::from(header)), Rect { height: 1, ..grid_area });

        for i in 0..vis_rows {
            let r = top + i;
            let y = grid_area.y + 1 + i as u16;
            if y >= grid_area.y + grid_area.height {
                break;
            }
            let rstyle = if r == self.row { pal.selected() } else { header_style };
            self.hits.push(Rect { x: grid_area.x, y, width: ROW_HEADER as u16, height: 1 }, SheetHit::RowHeader(r));
            let mut spans = vec![Span::styled(format!("{:>4} ", r + 1), rstyle)];
            for c in left..left + vis_cols {
                let v = self.value(r, c);
                let text = fit_width(&v.display(), COL_WIDTH - 1);
                let cell = if matches!(v, Value::Num(_)) {
                    format!("{:>w$} ", text, w = COL_WIDTH - 1)
                } else {
                    pad_width(&format!(" {text}"), COL_WIDTH)
                };
                let mut style = if (r, c) == (self.row, self.col) { pal.selected() } else { pal.fill() };
                if matches!(v, Value::Err(_)) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                spans.push(Span::styled(cell, style));
                let x = grid_area.x + (ROW_HEADER + (c - left) * COL_WIDTH) as u16;
                self.hits.push(Rect { x, y, width: COL_WIDTH as u16, height: 1 }, SheetHit::Cell(r, c));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default()),
                Rect { y, height: 1, ..grid_area },
            );
        }
        if self.confirm_close {
            render_confirm(frame, area, CLOSE_PROMPT, pal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&[&str]]) -> Grid {
        let mut g = Grid::default();
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                g.set(r, c, cell.to_string());
            }
        }
        g
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(parse_ref("b12"), Some((11, 1)));
        assert_eq!(parse_ref("$AA$3"), Some((2, 26)));
        assert_eq!(parse_ref("A0"), None);
        assert_eq!(parse_ref("SUM"), None);
    }

    #[test]
    fn arithmetic_and_functions() {
        let g = grid(&[&["10", "20", "=A1+B1*2"], &["5", "=SUM(A1:B2)", "=AVERAGE(A1:A2)"]]);
        let v = g.evaluate();
        assert_eq!(v[0][2], Value::Num(50.0));
        // B2 sums A1,B1,A2 and itself? No: B2 is inside its own range -> cycle.
        assert_eq!(v[1][1], Value::Err("#CYCLE!"));
        assert_eq!(v[1][2], Value::Num(7.5));
        let g = grid(&[&["1", "2", "3"], &["=SUM(A1:C1)", "=MAX(A1:C1)-MIN(A1:C1)", "=ROUND(10/3, 2)"]]);
        let v = g.evaluate();
        assert_eq!(v[1][0], Value::Num(6.0));
        assert_eq!(v[1][1], Value::Num(2.0));
        assert_eq!(v[1][2], Value::Num(3.33));
    }

    #[test]
    fn errors_do_not_panic() {
        let g = grid(&[&["=1/0", "=A1", "=FOO(1)", "=(1+", "=\"a\"*2", "=A1:B2", "=C1"]]);
        let v = g.evaluate();
        assert_eq!(v[0][0], Value::Err("#DIV/0!"));
        assert_eq!(v[0][1], Value::Err("#DIV/0!"));
        assert_eq!(v[0][2], Value::Err("#NAME?"));
        assert_eq!(v[0][3], Value::Err("#ERR!"));
        assert_eq!(v[0][4], Value::Err("#VALUE!"));
        assert_eq!(v[0][5], Value::Err("#VALUE!"));
        assert_eq!(v[0][6], Value::Err("#NAME?"));
        let g = grid(&[&["=B1", "=A1"]]);
        assert_eq!(g.evaluate()[0][0], Value::Err("#CYCLE!"));
        // Huge range and deep nesting are refused, not hung.
        let g = grid(&[&["=SUM(A2:ZZ9999)"], &[&format!("={}1{}", "(".repeat(5000), ")".repeat(5000))]]);
        let v = g.evaluate();
        assert_eq!(v[0][0], Value::Err("#REF!"));
        assert_eq!(v[1][0], Value::Err("#ERR!"));
    }

    #[test]
    fn text_logic_and_if() {
        let g = grid(&[&["Ann", "=\"Hi \"&A1", "=IF(C2>2,\"big\",\"small\")"], &["", "", "3"]]);
        let v = g.evaluate();
        assert_eq!(v[0][1], Value::Text("Hi Ann".into()));
        assert_eq!(v[0][2], Value::Text("big".into()));
    }

    #[test]
    fn csv_round_trip() {
        let g = grid(&[&["a,b", "say \"hi\"", "=SUM(1,2)"], &["", "x"]]);
        let back = Grid::from_csv(&g.to_csv());
        assert_eq!(back, g);
    }
}
