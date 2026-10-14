//! Writing a canvas to disk. The format is picked by extension: `.txt`
//! (plain glyphs), `.html`/`.htm`, `.svg`, `.irc`/`.mirc` (mIRC color
//! codes), and `.ans` (truecolor ANSI) for anything else. Only `.ans` and
//! `.txt` can be read back in.

use std::fs;
use std::path::Path;

use super::canvas::Canvas;
use super::palette::{color_distance, RgbColor};

pub fn save(canvas: &Canvas, path: &Path) -> anyhow::Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let bytes = match ext.as_deref() {
        Some("txt") => to_txt(canvas)?.into_bytes(),
        Some("html") | Some("htm") => to_html(canvas)?.into_bytes(),
        Some("svg") => to_svg(canvas)?.into_bytes(),
        Some("irc") | Some("mirc") => to_mirc(canvas)?.into_bytes(),
        _ => to_ans(canvas)?,
    };
    fs::write(path, bytes)?;
    Ok(())
}

fn bounds(canvas: &Canvas) -> anyhow::Result<(u16, u16, u16, u16)> {
    match canvas.bounds() {
        Some(b) => Ok(b),
        None => anyhow::bail!("canvas is empty"),
    }
}

pub fn to_ans(canvas: &Canvas) -> anyhow::Result<Vec<u8>> {
    let (min_x, min_y, max_x, max_y) = bounds(canvas)?;
    let mut out = Vec::new();
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if let Some(cell) = canvas.get(x, y) {
                out.extend_from_slice(
                    format!(
                        "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m{}",
                        cell.fg.0, cell.fg.1, cell.fg.2, cell.bg.0, cell.bg.1, cell.bg.2, cell.ch
                    )
                    .as_bytes(),
                );
            } else {
                out.extend_from_slice(b"\x1b[0m ");
            }
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    Ok(out)
}

/// Plain glyphs only, no color.
pub fn to_txt(canvas: &Canvas) -> anyhow::Result<String> {
    let (min_x, min_y, max_x, max_y) = bounds(canvas)?;
    let mut out = String::new();
    for y in min_y..=max_y {
        let mut line = String::new();
        for x in min_x..=max_x {
            line.push(canvas.get(x, y).map(|c| c.ch).unwrap_or(' '));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    Ok(out)
}

/// A `<pre>` block with one `<span>` per run of same-styled cells.
pub fn to_html(canvas: &Canvas) -> anyhow::Result<String> {
    let (min_x, min_y, max_x, max_y) = bounds(canvas)?;
    let mut out = String::from(
        "<!doctype html>\n<pre style=\"font-family:monospace;background:#000;color:#ccc;line-height:1\">\n",
    );
    for row in canvas_rows(canvas, min_x, min_y, max_x, max_y) {
        for run in row {
            let text = html_escape(&run.text);
            match (run.fg, run.bg) {
                (Some(fg), Some(bg)) => out.push_str(&format!(
                    "<span style=\"color:#{:02x}{:02x}{:02x};background:#{:02x}{:02x}{:02x}\">{text}</span>",
                    fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
                )),
                _ => out.push_str(&text),
            }
        }
        out.push('\n');
    }
    out.push_str("</pre>\n");
    Ok(out)
}

/// One `<text>` per row, one `<tspan>` per same-style run, with a `<rect>`
/// behind each colored run for its background.
pub fn to_svg(canvas: &Canvas) -> anyhow::Result<String> {
    let (min_x, min_y, max_x, max_y) = bounds(canvas)?;
    const CELL_W: u32 = 8;
    const CELL_H: u32 = 16;
    let w = (max_x - min_x + 1) as u32;
    let h = (max_y - min_y + 1) as u32;
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" font-family=\"monospace\" font-size=\"{CELL_H}\">\n<rect width=\"100%\" height=\"100%\" fill=\"#000\"/>\n",
        w * CELL_W,
        h * CELL_H
    );
    for (row_i, row) in canvas_rows(canvas, min_x, min_y, max_x, max_y).into_iter().enumerate() {
        let row_y = row_i as u32 * CELL_H;
        let mut cx = 0u32;
        for run in &row {
            let len = run.text.chars().count() as u32;
            if let Some(bg) = run.bg {
                out.push_str(&format!(
                    "<rect x=\"{}\" y=\"{row_y}\" width=\"{}\" height=\"{CELL_H}\" fill=\"#{:02x}{:02x}{:02x}\"/>\n",
                    cx * CELL_W,
                    len * CELL_W,
                    bg.0,
                    bg.1,
                    bg.2
                ));
            }
            cx += len;
        }
        out.push_str(&format!("<text x=\"0\" y=\"{}\" xml:space=\"preserve\">", row_y + CELL_H - 4));
        for run in &row {
            let text = html_escape(&run.text);
            match run.fg {
                Some(fg) => out.push_str(&format!("<tspan fill=\"#{:02x}{:02x}{:02x}\">{text}</tspan>", fg.0, fg.1, fg.2)),
                None => out.push_str(&text),
            }
        }
        out.push_str("</text>\n");
    }
    out.push_str("</svg>\n");
    Ok(out)
}

/// mIRC control codes: `\x03FF,BB` before each same-color run, `\x0F` at
/// each line end.
pub fn to_mirc(canvas: &Canvas) -> anyhow::Result<String> {
    let (min_x, min_y, max_x, max_y) = bounds(canvas)?;
    let mut out = String::new();
    for row in canvas_rows(canvas, min_x, min_y, max_x, max_y) {
        for run in row {
            match (run.fg, run.bg) {
                (Some(fg), Some(bg)) => {
                    out.push_str(&format!("\x03{:02},{:02}", nearest_mirc_index(fg), nearest_mirc_index(bg)));
                    out.push_str(&run.text);
                }
                _ => out.push_str(&run.text),
            }
        }
        out.push_str("\x0f\r\n");
    }
    Ok(out)
}

/// One run of consecutive same-styled cells on a row; an unpainted cell has
/// `fg`/`bg` both `None`.
struct Run {
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    text: String,
}

fn canvas_rows(canvas: &Canvas, min_x: u16, min_y: u16, max_x: u16, max_y: u16) -> Vec<Vec<Run>> {
    let mut rows = Vec::new();
    for y in min_y..=max_y {
        let mut runs: Vec<Run> = Vec::new();
        for x in min_x..=max_x {
            let (ch, fg, bg) = match canvas.get(x, y) {
                Some(c) => (c.ch, Some(c.fg), Some(c.bg)),
                None => (' ', None, None),
            };
            match runs.last_mut() {
                Some(run) if run.fg == fg && run.bg == bg => run.text.push(ch),
                _ => runs.push(Run { fg, bg, text: ch.to_string() }),
            }
        }
        rows.push(runs);
    }
    rows
}

/// The standard 16-entry mIRC color table, in mIRC's own numbering.
const MIRC_PALETTE: [RgbColor; 16] = [
    RgbColor::new(255, 255, 255),
    RgbColor::new(0, 0, 0),
    RgbColor::new(0, 0, 127),
    RgbColor::new(0, 147, 0),
    RgbColor::new(255, 0, 0),
    RgbColor::new(127, 0, 0),
    RgbColor::new(156, 0, 156),
    RgbColor::new(252, 127, 0),
    RgbColor::new(255, 255, 0),
    RgbColor::new(0, 252, 0),
    RgbColor::new(0, 147, 147),
    RgbColor::new(0, 255, 255),
    RgbColor::new(0, 0, 252),
    RgbColor::new(255, 0, 255),
    RgbColor::new(127, 127, 127),
    RgbColor::new(210, 210, 210),
];

fn nearest_mirc_index(color: RgbColor) -> u8 {
    MIRC_PALETTE
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| color_distance(**c, color))
        .map(|(i, _)| i as u8)
        .unwrap_or(1)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::super::ansi::{parse_ans, PaintCellSer};
    use super::*;

    fn sample() -> Canvas {
        let mut c = Canvas::new(10, 5);
        let cell = |ch| PaintCellSer { ch, fg: RgbColor::new(255, 0, 0), bg: RgbColor::new(0, 0, 0) };
        c.set(2, 1, Some(cell('<')));
        c.set(3, 1, Some(cell('A')));
        c.set(4, 2, Some(cell('B')));
        c
    }

    #[test]
    fn empty_canvas_refuses_to_save() {
        assert!(to_ans(&Canvas::new(3, 3)).is_err());
        assert!(to_txt(&Canvas::new(3, 3)).is_err());
    }

    #[test]
    fn ans_round_trips_through_the_parser() {
        let canvas = sample();
        let cells = parse_ans(&to_ans(&canvas).unwrap());
        // Saved cropped to the painted bounds.
        assert_eq!(cells.get(&(0, 0)).map(|c| c.ch), Some('<'));
        assert_eq!(cells.get(&(1, 0)), canvas.get(3, 1).as_ref());
        assert_eq!(cells.get(&(2, 1)).map(|c| c.ch), Some('B'));
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn txt_html_svg_mirc_contents() {
        let canvas = sample();
        assert_eq!(to_txt(&canvas).unwrap(), "<A\n  B\n");
        let html = to_html(&canvas).unwrap();
        assert!(html.contains("&lt;A</span>"));
        assert!(html.contains("color:#ff0000;background:#000000"));
        let svg = to_svg(&canvas).unwrap();
        assert!(svg.starts_with("<svg") && svg.contains("<tspan fill=\"#ff0000\">&lt;A</tspan>"));
        let mirc = to_mirc(&canvas).unwrap();
        assert!(mirc.starts_with("\x0304,01<A \x0f\r\n"));
    }
}
