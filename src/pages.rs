//! Pages: several named desktops, each with its own set of boxes.
//!
//! When Pages is on in Settings, a page bar sits right under the shortcut
//! bar. Each tab switches to that desktop; `+ New page` adds one. This module
//! holds the pure parts (bar layout, naming, applying the Settings edits) so
//! they can be tested without a terminal.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Name the first page gets when it was never named.
pub const DEFAULT_FIRST_PAGE: &str = "Main";
/// Longest page name shown on a tab (longer names are cut with "…").
const TAB_NAME_MAX: usize = 24;
const ADD_LABEL: &str = " + New page ";
const HINT: &str = "  [ ] switch · p rename";

/// What a click on the page bar does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageBarAction {
    Switch(usize),
    Add,
}

pub struct PageBarSegment {
    pub start: u16,
    pub end: u16,
    pub action: PageBarAction,
}

pub struct PageBarStyles {
    pub tab: Style,
    pub active: Style,
    pub add: Style,
    pub hint: Style,
}

/// The page bar's text plus the clickable ranges (columns from the bar's
/// left edge). Drawing and mouse hit-testing both use this, so they agree.
pub struct PageBar {
    pub line: Line<'static>,
    pub segments: Vec<PageBarSegment>,
}

fn width_of(text: &str) -> u16 {
    UnicodeWidthStr::width(text) as u16
}

fn cut(name: &str, max: usize) -> String {
    if UnicodeWidthStr::width(name) <= max {
        return name.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in name.chars() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w + 1 > max {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

fn tab_label(name: &str) -> String {
    format!(" {} ", cut(name, TAB_NAME_MAX))
}

/// Lay out the page bar for `width` columns. Tabs run left to right; when
/// they do not all fit, the bar scrolls so the current page's tab stays
/// visible, with ‹ / › showing there are more.
pub fn page_bar(names: &[String], current: usize, width: u16, styles: &PageBarStyles) -> PageBar {
    let labels: Vec<String> = names.iter().map(|n| tab_label(n)).collect();
    let current = current.min(labels.len().saturating_sub(1));
    let add_w = width_of(ADD_LABEL);
    // Width of tabs first..=last, with one space between tabs.
    let run = |first: usize, last: usize| -> u16 {
        labels[first..=last].iter().map(|l| width_of(l) + 1).sum::<u16>()
    };
    let mut first = 0;
    if !labels.is_empty() {
        // Room for "‹ " when scrolled and " ›" when cut on the right.
        while first < current && run(first, current) + add_w + 4 > width {
            first += 1;
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut segments = Vec::new();
    let mut cursor: u16 = 0;
    if first > 0 {
        spans.push(Span::styled("‹ ", styles.tab));
        cursor += 2;
    }
    let mut cut_right = false;
    for (idx, label) in labels.iter().enumerate().skip(first) {
        let w = width_of(label);
        let reserve = if idx + 1 < labels.len() { 2 } else { 0 };
        if idx > current && cursor + w + 1 + add_w + reserve > width {
            cut_right = true;
            break;
        }
        let style = if idx == current { styles.active } else { styles.tab };
        spans.push(Span::styled(label.clone(), style));
        segments.push(PageBarSegment {
            start: cursor,
            end: cursor + w,
            action: PageBarAction::Switch(idx),
        });
        cursor += w;
        spans.push(Span::styled(" ", styles.tab));
        cursor += 1;
    }
    if cut_right {
        spans.push(Span::styled("› ", styles.tab));
        cursor += 2;
    }
    spans.push(Span::styled(ADD_LABEL, styles.add));
    segments.push(PageBarSegment {
        start: cursor,
        end: cursor + add_w,
        action: PageBarAction::Add,
    });
    cursor += add_w;
    if cursor + width_of(HINT) <= width {
        spans.push(Span::styled(HINT, styles.hint));
    }
    PageBar {
        line: Line::from(spans),
        segments,
    }
}

/// The action under `column` (relative to the bar's left edge).
pub fn action_at(bar: &PageBar, column: u16) -> Option<PageBarAction> {
    bar.segments
        .iter()
        .find(|s| column >= s.start && column < s.end)
        .map(|s| s.action)
}

/// "Page N" with the lowest N not already used.
pub fn default_page_name(existing: &[String]) -> String {
    (existing.len() + 1..)
        .chain(1..)
        .map(|n| format!("Page {n}"))
        .find(|name| !existing.iter().any(|e| e.eq_ignore_ascii_case(name)))
        .unwrap_or_else(|| "Page".into())
}

/// `name`, or `name (2)`, `name (3)`… — the first one `taken` says is free.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} ({n})"))
        .find(|candidate| !taken(candidate))
        .unwrap()
}

/// The page list after the Settings form is saved.
#[derive(Debug, PartialEq, Eq)]
pub struct PagePlan {
    /// New page order: (index of the existing page, or None for a new
    /// empty page, and its name).
    pub pages: Vec<(Option<usize>, String)>,
    /// Existing pages that were removed (their boxes go to the first page).
    pub removed: Vec<usize>,
}

/// Turn the names typed in Settings into a plan. `existing` is how many
/// pages there were when the form opened; names past that are new pages.
/// A blank name removes that page (blank new ones are ignored). The first
/// page cannot be removed, and two pages cannot share a name.
pub fn plan_page_edits(existing: usize, names: &[String]) -> Result<PagePlan, String> {
    let mut pages = Vec::new();
    let mut removed = Vec::new();
    for (idx, raw) in names.iter().enumerate() {
        let name = raw.trim();
        if name.is_empty() {
            if idx == 0 {
                return Err("The first page needs a name".into());
            }
            if idx < existing {
                removed.push(idx);
            }
            continue;
        }
        if pages
            .iter()
            .any(|(_, other): &(Option<usize>, String)| other.eq_ignore_ascii_case(name))
        {
            return Err(format!("Two pages are named \"{name}\""));
        }
        pages.push(((idx < existing).then_some(idx), name.to_string()));
    }
    // Pages the form never listed (should not happen) are kept, not lost.
    for idx in names.len()..existing {
        if idx == 0 {
            pages.insert(0, (Some(0), DEFAULT_FIRST_PAGE.to_string()));
        } else {
            pages.push((Some(idx), unique_name(&format!("Page {}", idx + 1), |n| {
                pages.iter().any(|(_, o)| o.eq_ignore_ascii_case(n))
            })));
        }
    }
    if pages.is_empty() {
        pages.push((Some(0), DEFAULT_FIRST_PAGE.to_string()));
    }
    Ok(PagePlan { pages, removed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn styles() -> PageBarStyles {
        PageBarStyles {
            tab: Style::default(),
            active: Style::default(),
            add: Style::default(),
            hint: Style::default(),
        }
    }

    fn text(bar: &PageBar) -> String {
        bar.line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bar_lists_tabs_then_add_button_and_hits_match() {
        let bar = page_bar(&names(&["Main", "Work"]), 1, 80, &styles());
        assert!(text(&bar).starts_with(" Main   Work   + New page "));
        assert_eq!(action_at(&bar, 0), Some(PageBarAction::Switch(0)));
        assert_eq!(action_at(&bar, 5), Some(PageBarAction::Switch(0)));
        assert_eq!(action_at(&bar, 6), None); // the gap between tabs
        assert_eq!(action_at(&bar, 7), Some(PageBarAction::Switch(1)));
        assert_eq!(action_at(&bar, 15), Some(PageBarAction::Add));
        assert!(text(&bar).contains("switch"));
    }

    #[test]
    fn bar_scrolls_to_keep_current_tab_visible() {
        let many: Vec<String> = (1..=12).map(|n| format!("Desktop {n}")).collect();
        let bar = page_bar(&many, 11, 40, &styles());
        let shown = text(&bar);
        assert!(shown.starts_with("‹ "), "{shown}");
        assert!(shown.contains("Desktop 12"));
        assert!(UnicodeWidthStr::width(shown.as_str()) <= 40, "{shown}");
        assert!(bar.segments.iter().any(|s| s.action == PageBarAction::Switch(11)));
        assert!(bar.segments.iter().any(|s| s.action == PageBarAction::Add));

        let bar = page_bar(&many, 0, 40, &styles());
        let shown = text(&bar);
        assert!(shown.starts_with(" Desktop 1 "));
        assert!(shown.contains("› "), "{shown}");
        assert!(UnicodeWidthStr::width(shown.as_str()) <= 40, "{shown}");
    }

    #[test]
    fn long_names_are_cut() {
        let bar = page_bar(&names(&["A very very long page name indeed!!"]), 0, 80, &styles());
        assert!(text(&bar).contains('…'));
    }

    #[test]
    fn default_names_skip_used_ones() {
        assert_eq!(default_page_name(&names(&["Main"])), "Page 2");
        assert_eq!(default_page_name(&names(&["Main", "Page 3", "Page 2"])), "Page 4");
        assert_eq!(default_page_name(&names(&["Main", "page 2"])), "Page 3");
    }

    #[test]
    fn unique_name_adds_a_number() {
        let taken = names(&["Music", "Music (2)"]);
        assert_eq!(unique_name("Music", |n| taken.iter().any(|t| t == n)), "Music (3)");
        assert_eq!(unique_name("Paint", |n| taken.iter().any(|t| t == n)), "Paint");
    }

    #[test]
    fn plan_renames_adds_and_removes() {
        let plan = plan_page_edits(3, &names(&["Home", "", " Games ", "New", "  "])).unwrap();
        assert_eq!(
            plan.pages,
            vec![
                (Some(0), "Home".to_string()),
                (Some(2), "Games".to_string()),
                (None, "New".to_string()),
            ]
        );
        assert_eq!(plan.removed, vec![1]);
    }

    #[test]
    fn plan_rejects_blank_first_page_and_duplicates() {
        assert!(plan_page_edits(2, &names(&["", "Work"])).is_err());
        let err = plan_page_edits(2, &names(&["Work", "work"])).unwrap_err();
        assert!(err.contains("work"));
    }
}
