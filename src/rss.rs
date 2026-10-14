//! RSS box: a built-in news reader — feed list, article list and article
//! content panels, vim-style keys (j/k, h/l, s sync, o open in browser,
//! r/u read/unread, m/v mark/unmark, 1/2/3 filters, / search, z zen mode,
//! : command line, ? help), plus full mouse support. Feeds and articles are
//! stored in a JSON file; OPML import/export moves feeds to and from other
//! feed readers.
//!
//! Feed content is untrusted: every string is stripped of control and
//! bidi-override characters before it is drawn (no terminal escape
//! injection), downloads are size- and time-limited, only http(s) URLs are
//! fetched, and links open with `xdg-open <url>` without a shell.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::Frame;
use serde::{Deserialize, Serialize};

use crate::boxes::{expand_tilde, wrap_text};
use crate::dmui::{
    confirm_click, contains, dm_box, fit_width, is_left_click, key, put_line, render_confirm, screen_chrome,
    shortcut_click, FieldForm, FormField, FormResult, Hits, Palette,
};

const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARTICLES_PER_FEED: usize = 300;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const SYNC_WORKERS: usize = 4;
/// Opening the reader syncs automatically when the last sync is older.
const AUTO_SYNC_MINUTES: i64 = 30;

// ---------------------------------------------------------------- storage

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Article {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub published: Option<DateTime<Utc>>,
    /// Plain text (HTML already converted).
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub marked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    pub url: String,
    /// Title chosen by the user (blank = the feed's own title).
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub feed_title: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub last_sync: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub articles: Vec<Article>,
}

impl Feed {
    pub fn name(&self) -> String {
        for t in [&self.title, &self.feed_title] {
            if !t.trim().is_empty() {
                return t.trim().to_string();
            }
        }
        self.url.clone()
    }

    pub fn unread(&self) -> usize {
        self.articles.iter().filter(|a| !a.read).count()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub feeds: Vec<Feed>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else { return Self::default() };
        match serde_json::from_str(&text) {
            Ok(store) => store,
            Err(_) => {
                // Keep the unreadable file so the next save cannot lose it.
                let _ = fs::copy(path, path.with_extension("json.bad"));
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        fs::rename(&tmp, path)
    }

    fn sort(&mut self) {
        self.feeds
            .sort_by_key(|f| (f.category.to_lowercase(), f.name().to_lowercase()));
    }

    /// Merge freshly fetched articles, keeping read/marked flags.
    /// Returns how many articles are new.
    fn merge(&mut self, url: &str, fetched: Fetched) -> usize {
        let Some(feed) = self.feeds.iter_mut().find(|f| f.url == url) else {
            return 0;
        };
        feed.feed_title = fetched.title;
        feed.error = None;
        feed.last_sync = Some(Utc::now());
        let known: HashSet<String> = feed.articles.iter().map(|a| a.id.clone()).collect();
        let mut new = 0;
        let mut by_id: HashMap<String, usize> =
            feed.articles.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
        for art in fetched.articles {
            match by_id.get(&art.id) {
                Some(&i) => {
                    let old = &mut feed.articles[i];
                    old.title = art.title;
                    old.link = art.link;
                    old.author = art.author;
                    old.text = art.text;
                    if art.published.is_some() {
                        old.published = art.published;
                    }
                }
                None => {
                    if !known.contains(&art.id) {
                        new += 1;
                    }
                    by_id.insert(art.id.clone(), feed.articles.len());
                    feed.articles.push(art);
                }
            }
        }
        feed.articles.sort_by(|a, b| b.published.cmp(&a.published));
        if feed.articles.len() > MAX_ARTICLES_PER_FEED {
            // Keep every marked article; drop the oldest of the rest.
            let mut kept = 0;
            feed.articles.retain(|a| {
                kept += 1;
                a.marked || kept <= MAX_ARTICLES_PER_FEED
            });
        }
        new
    }
}

/// Feed names with unread counts, for the desktop box.
pub fn preview_lines(path: &Path) -> Vec<(String, Option<(usize, usize)>)> {
    Store::load(path)
        .feeds
        .iter()
        .map(|f| {
            let unread = f.unread();
            let name = sanitize(&f.name());
            let text = if f.error.is_some() {
                format!("{name} (!)")
            } else if unread > 0 {
                format!("{name} ({unread})")
            } else {
                name
            };
            (text, None)
        })
        .collect()
}

/// One article as an RSS box shows it on the desktop.
#[derive(Clone, Debug, PartialEq)]
pub struct Headline {
    pub feed: usize,
    pub article: usize,
    pub title: String,
    /// The first line of the article's text.
    pub snippet: String,
    pub read: bool,
}

/// The newest `count` articles across all feeds, newest first.
pub fn headlines(path: &Path, count: usize) -> Vec<Headline> {
    let store = Store::load(path);
    let mut list: Vec<(usize, usize)> = store
        .feeds
        .iter()
        .enumerate()
        .flat_map(|(fi, feed)| (0..feed.articles.len()).map(move |ai| (fi, ai)))
        .collect();
    list.sort_by(|x, y| {
        let a = &store.feeds[x.0].articles[x.1];
        let b = &store.feeds[y.0].articles[y.1];
        b.published.cmp(&a.published)
    });
    list.into_iter()
        .take(count)
        .map(|(fi, ai)| {
            let a = &store.feeds[fi].articles[ai];
            Headline {
                feed: fi,
                article: ai,
                title: sanitize(a.title.trim()),
                snippet: first_line(&a.text),
                read: a.read,
            }
        })
        .collect()
}

/// The first non-blank line of an article's text, made safe to show.
pub fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(sanitize)
        .unwrap_or_default()
}

// ---------------------------------------------------------------- text safety

/// Remove control characters and bidi overrides so feed text can never
/// move the cursor, change colors or reorder what is shown.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c })
        .filter(|c| {
            !c.is_control()
                && !matches!(*c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
        })
        .collect()
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..semi];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "rsquo" | "lsquo" => Some('\''),
            "rdquo" | "ldquo" => Some('"'),
            _ => {
                let num = name.strip_prefix('#');
                num.and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32)
            }
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Very small HTML-to-text: drops tags (and script/style contents), turns
/// block tags into paragraph breaks and list items into bullets.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut chars = html.char_indices().peekable();
    let lower = html.to_ascii_lowercase();
    while let Some((i, c)) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let Some(end) = html[i..].find('>') else {
            out.push_str(&html[i..]);
            break;
        };
        let tag = &lower[i + 1..i + end];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let closing = tag.starts_with('/');
        let mut skip_to = i + end + 1;
        if !closing && (name == "script" || name == "style") {
            let close = format!("</{name}");
            skip_to = lower[skip_to..]
                .find(&close)
                .and_then(|p| lower[skip_to + p..].find('>').map(|q| skip_to + p + q + 1))
                .unwrap_or(html.len());
        }
        match name.as_str() {
            "br" => out.push('\n'),
            "li" if !closing => out.push_str(if out.ends_with('\n') { "• " } else { "\n• " }),
            "li" => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            "p" | "div" | "ul" | "ol" | "tr" | "blockquote" | "pre" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" | "table" | "section" | "article" | "hr" => out.push_str("\n\n"),
            "img" => {
                if let Some(alt) = attr(&html[i..i + end], "alt").filter(|a| !a.trim().is_empty()) {
                    out.push_str(&format!("[image: {alt}]"));
                }
            }
            _ => {}
        }
        while chars.peek().is_some_and(|(j, _)| *j < skip_to) {
            chars.next();
        }
    }
    let text = decode_entities(&out);
    // Collapse whitespace inside lines and runs of blank lines.
    let mut result = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !result.is_empty() {
            result.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        result.push_str(&line);
    }
    result
}

/// Value of attribute `name` inside one tag's text (`<a href="..">`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let start = from + pos;
        from = start + name.len();
        let before_ok = start == 0 || lower.as_bytes()[start - 1].is_ascii_whitespace();
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value_start = tag.len() - after.len() + 1;
        let value = tag[value_start..].trim_start();
        let (quote, body) = match value.chars().next() {
            Some(q @ ('"' | '\'')) => (Some(q), &value[1..]),
            _ => (None, value),
        };
        let end = match quote {
            Some(q) => body.find(q).unwrap_or(body.len()),
            None => body.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(body.len()),
        };
        return Some(decode_entities(&body[..end]));
    }
    None
}

// ---------------------------------------------------------------- fetching

pub struct Fetched {
    pub title: String,
    pub articles: Vec<Article>,
}

fn valid_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && !url.trim().contains(char::is_whitespace)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    if !valid_url(url) {
        return Err("only http:// and https:// feeds are supported".into());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(5)
        .user_agent(concat!("menu-maker/", env!("CARGO_PKG_VERSION"), " (RSS box)"))
        .build();
    let response = agent.get(url.trim()).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.to_string(),
    })?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > MAX_BODY_BYTES {
        return Err("feed is larger than 8 MB".into());
    }
    Ok(body)
}

/// Parse feed bytes (RSS 0.9x/1.0/2.0, Atom, JSON Feed).
pub fn parse_feed(body: &[u8]) -> Result<Fetched, String> {
    let feed = feed_rs::parser::parse(body).map_err(|e| format!("not a feed: {e}"))?;
    let title = sanitize(&feed.title.map(|t| html_to_text(&t.content)).unwrap_or_default());
    let articles = feed
        .entries
        .into_iter()
        .map(|e| {
            let link = e
                .links
                .iter()
                .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                .or(e.links.first())
                .map(|l| l.href.trim().to_string())
                .unwrap_or_default();
            let body = e
                .content
                .and_then(|c| c.body)
                .or_else(|| e.summary.map(|s| s.content))
                .unwrap_or_default();
            let title = sanitize(&e.title.map(|t| html_to_text(&t.content)).unwrap_or_default());
            let id = if !e.id.trim().is_empty() {
                e.id
            } else if !link.is_empty() {
                link.clone()
            } else {
                title.clone()
            };
            Article {
                id,
                title: if title.is_empty() { "(untitled)".into() } else { title },
                author: sanitize(
                    &e.authors.iter().filter_map(|p| p.name.clone()).collect::<Vec<_>>().join(", "),
                ),
                published: e.published.or(e.updated),
                text: html_to_text(&body),
                link,
                read: false,
                marked: false,
            }
        })
        .collect();
    Ok(Fetched { title, articles })
}

/// Find `<link rel="alternate" type="application/rss+xml" href=...>` in a web page.
fn discover_feed(page: &str, base: &str) -> Option<String> {
    let lower = page.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<link") {
        let start = from + pos;
        let end = lower[start..].find('>').map(|e| start + e)?;
        from = end;
        let tag = &page[start..end];
        let kind = attr(tag, "type").unwrap_or_default().to_ascii_lowercase();
        let rel = attr(tag, "rel").unwrap_or_default().to_ascii_lowercase();
        if rel.contains("alternate") && (kind.contains("rss") || kind.contains("atom") || kind.contains("feed+json")) {
            let href = attr(tag, "href")?;
            return Some(absolute_url(base, &href));
        }
    }
    None
}

fn absolute_url(base: &str, href: &str) -> String {
    if valid_url(href) {
        return href.to_string();
    }
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
    if let Some(rest) = href.strip_prefix("//") {
        return format!("{}{rest}", &base[..scheme_end]);
    }
    if href.starts_with('/') {
        return format!("{}{href}", &base[..host_end]);
    }
    let dir_end = base.rfind('/').filter(|i| *i >= host_end).unwrap_or(host_end);
    format!("{}/{href}", &base[..dir_end])
}

/// Download and parse one feed; a web page is searched for its feed link.
pub fn fetch_feed(url: &str) -> Result<Fetched, String> {
    let body = download(url)?;
    match parse_feed(&body) {
        Ok(f) => Ok(f),
        Err(err) => {
            let page = String::from_utf8_lossy(&body);
            match discover_feed(&page, url) {
                Some(found) if found != url => parse_feed(&download(&found)?),
                _ => Err(err),
            }
        }
    }
}

enum SyncMsg {
    Feed(String, Result<Fetched, String>),
    Done,
}

/// Fetch `urls` on a few background threads; results arrive on the channel.
fn start_sync(urls: Vec<String>) -> Receiver<SyncMsg> {
    let (tx, rx) = mpsc::channel();
    let queue = Arc::new(Mutex::new(urls));
    let workers = SYNC_WORKERS;
    let done = Arc::new(Mutex::new(0usize));
    for _ in 0..workers {
        let (tx, queue, done) = (tx.clone(), queue.clone(), done.clone());
        std::thread::spawn(move || {
            loop {
                let next = queue.lock().ok().and_then(|mut q| q.pop());
                let Some(url) = next else { break };
                let result = fetch_feed(&url);
                if tx.send(SyncMsg::Feed(url, result)).is_err() {
                    return;
                }
            }
            if let Ok(mut d) = done.lock() {
                *d += 1;
                if *d == workers {
                    let _ = tx.send(SyncMsg::Done);
                }
            }
        });
    }
    rx
}

// ---------------------------------------------------------------- OPML

pub fn parse_opml(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut categories: Vec<String> = Vec::new();
    let mut from = 0;
    let lower = text.to_ascii_lowercase();
    while let Some(pos) = lower[from..].find('<') {
        let start = from + pos;
        let Some(end) = lower[start..].find('>').map(|e| start + e) else { break };
        from = end + 1;
        let tag = &text[start..=end];
        let ltag = &lower[start..=end];
        if ltag.starts_with("</outline") {
            categories.pop();
            continue;
        }
        if !ltag.starts_with("<outline") {
            continue;
        }
        let self_closing = ltag.trim_end_matches('>').trim_end().ends_with('/');
        let name = attr(tag, "title").or_else(|| attr(tag, "text")).unwrap_or_default();
        match attr(tag, "xmlurl") {
            Some(url) if valid_url(&url) => {
                out.push((url, sanitize(&name), categories.last().cloned().unwrap_or_default()));
                if !self_closing {
                    categories.push(categories.last().cloned().unwrap_or_default());
                }
            }
            _ if !self_closing => categories.push(sanitize(&name)),
            _ => {}
        }
    }
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn to_opml(store: &Store) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head><title>MenuMaker feeds</title></head>\n  <body>\n",
    );
    let mut cats: Vec<&str> = store.feeds.iter().map(|f| f.category.as_str()).collect();
    cats.dedup();
    cats.sort();
    cats.dedup();
    for cat in cats {
        let indent = if cat.is_empty() { "    " } else { "      " };
        if !cat.is_empty() {
            out.push_str(&format!("    <outline text=\"{0}\" title=\"{0}\">\n", xml_escape(cat)));
        }
        for f in store.feeds.iter().filter(|f| f.category == cat) {
            out.push_str(&format!(
                "{indent}<outline type=\"rss\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\"/>\n",
                xml_escape(&f.name()),
                xml_escape(&f.url)
            ));
        }
        if !cat.is_empty() {
            out.push_str("    </outline>\n");
        }
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

// ---------------------------------------------------------------- the app

pub enum AppResult {
    Continue,
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Feeds,
    Articles,
    Content,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Unread,
    Marked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    All,
    Marked,
    Feed(usize),
}

enum FormPurpose {
    Add,
    Edit(usize),
}

enum Popup {
    Form(FieldForm, FormPurpose),
    ConfirmDelete(usize),
    Help,
}

#[derive(Clone, Copy)]
enum RssHit {
    FeedsBox,
    Feed(usize),
    ArticlesBox,
    Article(usize),
    ContentBox,
    Link,
    HelpBox,
}

pub struct RssApp {
    pub box_name: String,
    path: PathBuf,
    store: Store,
    focus: Focus,
    filter: Filter,
    feed_sel: usize,
    article_sel: usize,
    content_scroll: usize,
    search: Option<String>,
    search_typing: bool,
    zen: bool,
    command: Option<String>,
    popup: Option<Popup>,
    sync: Option<Receiver<SyncMsg>>,
    sync_total: usize,
    sync_done: usize,
    sync_new: usize,
    sync_errors: usize,
    status: String,
    hits: Hits<RssHit>,
    feed_top: std::cell::Cell<usize>,
    article_top: std::cell::Cell<usize>,
}

const HELP: &[(&str, &str)] = &[
    ("j / k, ↑ / ↓", "Move down / up (wheel too)"),
    ("h / l, ← / →, Tab", "Move between panels"),
    ("g / G", "First / last"),
    ("Enter", "Read the selected article (marks it read)"),
    ("o", "Open article in browser, mark read, jump to next unread"),
    ("r / u", "Mark read / unread"),
    ("m / v", "Mark (star) / unmark article"),
    ("A", "Mark every shown article as read"),
    ("s / S", "Sync all feeds / the selected feed"),
    ("1 / 2 / 3", "Show all / only unread / only marked"),
    ("/", "Search articles (Esc clears)"),
    ("z", "Zen mode: just the article"),
    ("Ctrl-j / Ctrl-k", "Next / previous feed from any panel"),
    ("a / e / d", "Add / edit / delete feed"),
    (":", "Command: importopml FILE, exportopml FILE, add URL, sync, read"),
    ("?", "This help"),
    ("q / Esc", "Back to the desktop"),
    ("Mouse", "Click to select; click again to read / open; ◄► chips work"),
];

impl RssApp {
    pub fn open(box_name: &str, path: PathBuf, select: Option<usize>) -> Self {
        let store = Store::load(&path);
        let mut app = Self {
            box_name: box_name.to_string(),
            status: format!("Feeds file: {}  —  press ? for keys", path.display()),
            path,
            store,
            focus: Focus::Feeds,
            filter: Filter::All,
            feed_sel: 0,
            article_sel: 0,
            content_scroll: 0,
            search: None,
            search_typing: false,
            zen: false,
            command: None,
            popup: None,
            sync: None,
            sync_total: 0,
            sync_done: 0,
            sync_new: 0,
            sync_errors: 0,
            hits: Hits::default(),
            feed_top: Default::default(),
            article_top: Default::default(),
        };
        if let Some(row) = select {
            app.feed_sel = (row + 2).min(app.sources().len().saturating_sub(1));
            app.focus = Focus::Articles;
        }
        if app.store.feeds.is_empty() {
            app.status = "No feeds yet - press a to add one, or :importopml FILE".into();
        } else {
            let stale = app.store.feeds.iter().any(|f| {
                f.last_sync
                    .is_none_or(|t| (Utc::now() - t).num_minutes() >= AUTO_SYNC_MINUTES)
            });
            if stale {
                app.sync_all();
            }
        }
        app
    }

    /// Open the reader showing one article (picked from the desktop box).
    pub fn open_article(box_name: &str, path: PathBuf, feed: usize, article: usize) -> Self {
        let mut app = Self::open(box_name, path, None);
        if let Some(pos) = app.articles().iter().position(|at| *at == (feed, article)) {
            app.article_sel = pos;
            app.focus = Focus::Content;
        }
        app
    }

    fn save(&mut self) {
        if let Err(err) = self.store.save(&self.path) {
            self.status = format!("Could not save {}: {err}", self.path.display());
        }
    }

    // ----- views

    fn sources(&self) -> Vec<Source> {
        let mut s = vec![Source::All, Source::Marked];
        s.extend((0..self.store.feeds.len()).map(Source::Feed));
        s
    }

    fn source(&self) -> Source {
        self.sources().get(self.feed_sel).copied().unwrap_or(Source::All)
    }

    /// Articles shown for the selected source, filter and search, newest first.
    fn articles(&self) -> Vec<(usize, usize)> {
        let source = self.source();
        let query = self.search.as_ref().map(|q| q.to_lowercase()).filter(|q| !q.is_empty());
        let mut list: Vec<(usize, usize)> = Vec::new();
        for (fi, feed) in self.store.feeds.iter().enumerate() {
            if let Source::Feed(sel) = source {
                if sel != fi {
                    continue;
                }
            }
            for (ai, a) in feed.articles.iter().enumerate() {
                let keep = match (source, self.filter) {
                    (Source::Marked, _) => a.marked,
                    (_, Filter::All) => true,
                    (_, Filter::Unread) => !a.read,
                    (_, Filter::Marked) => a.marked,
                };
                let matches = query.as_ref().is_none_or(|q| {
                    a.title.to_lowercase().contains(q) || a.text.to_lowercase().contains(q) || a.author.to_lowercase().contains(q)
                });
                if keep && matches {
                    list.push((fi, ai));
                }
            }
        }
        list.sort_by(|x, y| {
            let a = &self.store.feeds[x.0].articles[x.1];
            let b = &self.store.feeds[y.0].articles[y.1];
            b.published.cmp(&a.published)
        });
        list
    }

    fn current(&self) -> Option<(usize, usize)> {
        self.articles().get(self.article_sel).copied()
    }

    fn article(&self, at: (usize, usize)) -> &Article {
        &self.store.feeds[at.0].articles[at.1]
    }

    fn article_mut(&mut self, at: (usize, usize)) -> &mut Article {
        &mut self.store.feeds[at.0].articles[at.1]
    }

    fn select_feed(&mut self, idx: usize) {
        self.feed_sel = idx.min(self.sources().len().saturating_sub(1));
        self.article_sel = 0;
        self.content_scroll = 0;
    }

    fn select_article(&mut self, idx: usize) {
        let len = self.articles().len();
        self.article_sel = idx.min(len.saturating_sub(1));
        self.content_scroll = 0;
    }

    fn set_read(&mut self, read: bool) {
        if let Some(at) = self.current() {
            // Under the Unread filter the article drops out of the list;
            // keep the cursor where it is so the next one moves up.
            self.article_mut(at).read = read;
            self.save();
        }
    }

    fn set_marked(&mut self, marked: bool) {
        if let Some(at) = self.current() {
            self.article_mut(at).marked = marked;
            self.status = if marked { "Article marked".into() } else { "Article unmarked".into() };
            self.save();
        }
    }

    fn read_current(&mut self) {
        if let Some(at) = self.current() {
            if !self.article(at).read {
                self.article_mut(at).read = true;
                self.save();
                // Unread filter: re-find it so the view does not jump away.
                if self.filter == Filter::Unread {
                    return;
                }
            }
            self.focus = Focus::Content;
        }
    }

    fn open_in_browser(&mut self) {
        let Some(at) = self.current() else { return };
        let link = self.article(at).link.clone();
        if !valid_url(&link) {
            self.status = "This article has no web link".into();
            return;
        }
        let opened = std::process::Command::new("xdg-open")
            .arg(&link)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        self.status = match opened {
            Ok(_) => format!("Opened {}", sanitize(&link)),
            Err(err) => format!("Could not run xdg-open: {err}"),
        };
        let was_unread = !self.article(at).read;
        self.article_mut(at).read = true;
        self.save();
        // Jump to the next unread article.
        let list = self.articles();
        let start = list.iter().position(|x| *x == at).map(|p| p + 1).unwrap_or(self.article_sel);
        if let Some(next) = (start..list.len()).find(|&i| !self.article(list[i]).read) {
            self.select_article(next);
        } else if was_unread && self.filter == Filter::Unread {
            self.select_article(self.article_sel);
        }
    }

    fn mark_all_read(&mut self) {
        let list = self.articles();
        for at in &list {
            self.article_mut(*at).read = true;
        }
        self.status = format!("Marked {} articles as read", list.len());
        self.save();
    }

    // ----- sync

    fn sync_urls(&mut self, urls: Vec<String>) {
        if self.sync.is_some() {
            self.status = "Already syncing...".into();
            return;
        }
        if urls.is_empty() {
            self.status = "No feeds to sync - press a to add one".into();
            return;
        }
        self.sync_total = urls.len();
        self.sync_done = 0;
        self.sync_new = 0;
        self.sync_errors = 0;
        self.status = format!("Syncing {} feeds...", urls.len());
        self.sync = Some(start_sync(urls));
    }

    fn sync_all(&mut self) {
        let urls = self.store.feeds.iter().map(|f| f.url.clone()).collect();
        self.sync_urls(urls);
    }

    fn sync_selected(&mut self) {
        match self.source() {
            Source::Feed(i) => {
                let url = self.store.feeds[i].url.clone();
                self.sync_urls(vec![url]);
            }
            _ => self.sync_all(),
        }
    }

    /// Collect finished downloads; call this every event-loop pass.
    pub fn poll(&mut self) {
        let Some(rx) = self.sync.take() else { return };
        let mut finished = false;
        let mut changed = false;
        // Keep the selected article selected across the merge.
        let selected_id = self.current().map(|at| (self.store.feeds[at.0].url.clone(), self.article(at).id.clone()));
        let selected_feed = match self.source() {
            Source::Feed(i) => Some(self.store.feeds[i].url.clone()),
            _ => None,
        };
        while let Ok(msg) = rx.try_recv() {
            match msg {
                SyncMsg::Feed(url, Ok(fetched)) => {
                    self.sync_new += self.store.merge(&url, fetched);
                    self.sync_done += 1;
                    changed = true;
                }
                SyncMsg::Feed(url, Err(err)) => {
                    if let Some(feed) = self.store.feeds.iter_mut().find(|f| f.url == url) {
                        feed.error = Some(sanitize(&err));
                    }
                    self.sync_errors += 1;
                    self.sync_done += 1;
                    changed = true;
                }
                SyncMsg::Done => finished = true,
            }
        }
        if changed {
            self.store.sort();
            if let Some(url) = selected_feed {
                if let Some(i) = self.store.feeds.iter().position(|f| f.url == url) {
                    self.feed_sel = i + 2;
                }
            }
            if let Some((url, id)) = selected_id {
                let list = self.articles();
                if let Some(pos) = list
                    .iter()
                    .position(|at| self.store.feeds[at.0].url == url && self.article(*at).id == id)
                {
                    self.article_sel = pos;
                }
            }
            self.save();
        }
        if finished {
            let errors = if self.sync_errors > 0 { format!(", {} failed (marked !)", self.sync_errors) } else { String::new() };
            self.status = format!("Synced {} feeds: {} new articles{errors}", self.sync_total, self.sync_new);
        } else {
            self.status = format!("Syncing... {}/{} feeds, {} new", self.sync_done, self.sync_total, self.sync_new);
            self.sync = Some(rx);
        }
    }

    pub fn syncing(&self) -> bool {
        self.sync.is_some()
    }

    // ----- feeds

    fn feed_form(&self, purpose: FormPurpose) -> Popup {
        let (title, url, name, cat) = match purpose {
            FormPurpose::Add => ("Add Feed", String::new(), String::new(), String::new()),
            FormPurpose::Edit(i) => {
                let f = &self.store.feeds[i];
                ("Edit Feed", f.url.clone(), f.title.clone(), f.category.clone())
            }
        };
        Popup::Form(
            FieldForm::new(
                title,
                vec![
                    FormField::text("Feed or site URL", &url),
                    FormField::text("Title (blank = feed's)", &name),
                    FormField::text("Category", &cat),
                ],
            ),
            purpose,
        )
    }

    fn add_feed(&mut self, url: &str, title: &str, category: &str) -> Result<(), String> {
        let url = url.trim().to_string();
        if !valid_url(&url) {
            return Err("Enter an http:// or https:// URL".into());
        }
        if self.store.feeds.iter().any(|f| f.url == url) {
            return Err("That feed is already in the list".into());
        }
        self.store.feeds.push(Feed {
            url: url.clone(),
            title: sanitize(title.trim()),
            category: sanitize(category.trim()),
            ..Feed::default()
        });
        self.store.sort();
        if let Some(i) = self.store.feeds.iter().position(|f| f.url == url) {
            self.select_feed(i + 2);
        }
        self.save();
        self.sync_urls(vec![url]);
        Ok(())
    }

    fn submit_form(&mut self, form: &FieldForm, purpose: &FormPurpose) -> Result<(), String> {
        let (url, title, cat) = (form.value(0).to_string(), form.value(1).to_string(), form.value(2).to_string());
        match purpose {
            FormPurpose::Add => self.add_feed(&url, &title, &cat),
            FormPurpose::Edit(i) => {
                if !valid_url(&url) {
                    return Err("Enter an http:// or https:// URL".into());
                }
                if self.store.feeds.iter().enumerate().any(|(j, f)| j != *i && f.url == url.trim()) {
                    return Err("That feed is already in the list".into());
                }
                let feed = &mut self.store.feeds[*i];
                let url_changed = feed.url != url.trim();
                feed.url = url.trim().to_string();
                feed.title = sanitize(&title);
                feed.category = sanitize(&cat);
                if url_changed {
                    feed.articles.clear();
                    feed.last_sync = None;
                }
                let url = feed.url.clone();
                self.store.sort();
                if let Some(i) = self.store.feeds.iter().position(|f| f.url == url) {
                    self.feed_sel = i + 2;
                }
                self.save();
                self.status = "Feed updated".into();
                if url_changed {
                    self.sync_urls(vec![url]);
                }
                Ok(())
            }
        }
    }

    fn run_command(&mut self, line: &str) {
        let line = line.trim();
        let (cmd, arg) = line.split_once(char::is_whitespace).map(|(c, a)| (c, a.trim())).unwrap_or((line, ""));
        match cmd.to_ascii_lowercase().as_str() {
            "" => {}
            "importopml" | "import" => {
                let path = expand_tilde(arg);
                match fs::read_to_string(&path) {
                    Ok(text) => {
                        let mut added = Vec::new();
                        for (url, title, cat) in parse_opml(&text) {
                            if !self.store.feeds.iter().any(|f| f.url == url) {
                                self.store.feeds.push(Feed { url: url.clone(), title, category: cat, ..Feed::default() });
                                added.push(url);
                            }
                        }
                        self.store.sort();
                        self.save();
                        self.status = format!("Imported {} feeds from {}", added.len(), path.display());
                        if !added.is_empty() {
                            self.sync_urls(added);
                        }
                    }
                    Err(err) => self.status = format!("Could not read {}: {err}", path.display()),
                }
            }
            "exportopml" | "export" => {
                let path = expand_tilde(if arg.is_empty() { "~/feeds.opml" } else { arg });
                self.status = match fs::write(&path, to_opml(&self.store)) {
                    Ok(()) => format!("Exported {} feeds to {}", self.store.feeds.len(), path.display()),
                    Err(err) => format!("Could not write {}: {err}", path.display()),
                };
            }
            "add" | "addfeed" => {
                if let Err(err) = self.add_feed(arg, "", "") {
                    self.status = err;
                }
            }
            "sync" => self.sync_all(),
            "read" => self.mark_all_read(),
            "q" | "quit" => {}
            other => self.status = format!("Unknown command: {other} (try importopml, exportopml, add, sync, read)"),
        }
    }

    // ----- keys

    pub fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(popup) = self.popup.take() {
            match popup {
                Popup::Help => {}
                Popup::ConfirmDelete(i) => {
                    if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter) && i < self.store.feeds.len() {
                        let feed = self.store.feeds.remove(i);
                        self.status = format!("Deleted feed {}", feed.name());
                        self.select_feed(self.feed_sel.min(self.store.feeds.len() + 1));
                        self.save();
                    }
                }
                Popup::Form(mut form, purpose) => match form.handle_key(key) {
                    FormResult::Continue => self.popup = Some(Popup::Form(form, purpose)),
                    FormResult::Cancel => {}
                    FormResult::Submit => {
                        if let Err(err) = self.submit_form(&form, &purpose) {
                            form.error = Some(err);
                            self.popup = Some(Popup::Form(form, purpose));
                        }
                    }
                },
            }
            return AppResult::Continue;
        }
        if let Some(mut line) = self.command.take() {
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.run_command(&line),
                KeyCode::Backspace => {
                    line.pop();
                    self.command = Some(line);
                }
                KeyCode::Char(c) if !ctrl => {
                    line.push(c);
                    self.command = Some(line);
                }
                _ => self.command = Some(line),
            }
            return AppResult::Continue;
        }
        if self.search_typing {
            let query = self.search.get_or_insert_with(String::new);
            match key.code {
                KeyCode::Esc => {
                    self.search = None;
                    self.search_typing = false;
                }
                KeyCode::Enter => {
                    self.search_typing = false;
                    self.focus = Focus::Articles;
                }
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c) if !ctrl => query.push(c),
                _ => {}
            }
            self.article_sel = 0;
            return AppResult::Continue;
        }

        match key.code {
            KeyCode::Esc if self.zen => self.zen = false,
            KeyCode::Esc if self.search.is_some() => {
                self.search = None;
                self.article_sel = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return AppResult::Close,
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('s') => self.sync_all(),
            KeyCode::Char('S') => self.sync_selected(),
            KeyCode::Char('1') => {
                self.filter = Filter::All;
                self.select_article(0);
                self.status = "Showing all articles".into();
            }
            KeyCode::Char('2') => {
                self.filter = Filter::Unread;
                self.select_article(0);
                self.status = "Showing unread articles".into();
            }
            KeyCode::Char('3') => {
                self.filter = Filter::Marked;
                self.select_article(0);
                self.status = "Showing marked articles".into();
            }
            KeyCode::Char('/') => {
                self.search = Some(String::new());
                self.search_typing = true;
            }
            KeyCode::Char('z') => {
                if self.current().is_some() {
                    self.zen = !self.zen;
                    self.read_current();
                }
            }
            KeyCode::Char(':') => self.command = Some(String::new()),
            KeyCode::Char('a') => self.popup = Some(self.feed_form(FormPurpose::Add)),
            KeyCode::Char('A') => self.mark_all_read(),
            KeyCode::Char('j') if ctrl => self.select_feed(self.feed_sel + 1),
            KeyCode::Char('k') if ctrl => self.select_feed(self.feed_sel.saturating_sub(1)),
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right if !self.zen => {
                self.focus = match self.focus {
                    Focus::Feeds => Focus::Articles,
                    Focus::Articles => Focus::Content,
                    Focus::Content => Focus::Feeds,
                };
            }
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left if !self.zen => {
                self.focus = match self.focus {
                    Focus::Feeds => Focus::Content,
                    Focus::Articles => Focus::Feeds,
                    Focus::Content => Focus::Articles,
                };
            }
            _ => {
                let focus = if self.zen { Focus::Content } else { self.focus };
                match focus {
                    Focus::Feeds => self.feeds_key(key),
                    Focus::Articles => self.articles_key(key),
                    Focus::Content => self.content_key(key),
                }
            }
        }
        AppResult::Continue
    }

    fn feeds_key(&mut self, key: KeyEvent) {
        let len = self.sources().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select_feed(self.feed_sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.select_feed(self.feed_sel + 1),
            KeyCode::Home | KeyCode::Char('g') => self.select_feed(0),
            KeyCode::End | KeyCode::Char('G') => self.select_feed(len),
            KeyCode::PageUp => self.select_feed(self.feed_sel.saturating_sub(10)),
            KeyCode::PageDown => self.select_feed(self.feed_sel + 10),
            KeyCode::Enter => self.focus = Focus::Articles,
            KeyCode::Char('e') => match self.source() {
                Source::Feed(i) => self.popup = Some(self.feed_form(FormPurpose::Edit(i))),
                _ => self.status = "Select a feed to edit".into(),
            },
            KeyCode::Char('d') | KeyCode::Delete => match self.source() {
                Source::Feed(i) => self.popup = Some(Popup::ConfirmDelete(i)),
                _ => self.status = "Select a feed to delete".into(),
            },
            _ => {}
        }
    }

    fn articles_key(&mut self, key: KeyEvent) {
        let len = self.articles().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select_article(self.article_sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.select_article(self.article_sel + 1),
            KeyCode::Home | KeyCode::Char('g') => self.select_article(0),
            KeyCode::End | KeyCode::Char('G') => self.select_article(len),
            KeyCode::PageUp => self.select_article(self.article_sel.saturating_sub(10)),
            KeyCode::PageDown => self.select_article(self.article_sel + 10),
            KeyCode::Enter => self.read_current(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('r') => self.set_read(true),
            KeyCode::Char('u') => self.set_read(false),
            KeyCode::Char('m') => self.set_marked(true),
            KeyCode::Char('v') => self.set_marked(false),
            _ => {}
        }
    }

    fn content_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.content_scroll = self.content_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.content_scroll += 1,
            KeyCode::PageUp => self.content_scroll = self.content_scroll.saturating_sub(10),
            KeyCode::PageDown | KeyCode::Char(' ') => self.content_scroll += 10,
            KeyCode::Home | KeyCode::Char('g') => self.content_scroll = 0,
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('r') => self.set_read(true),
            KeyCode::Char('u') => self.set_read(false),
            KeyCode::Char('m') => self.set_marked(true),
            KeyCode::Char('v') => self.set_marked(false),
            KeyCode::Enter => self.focus = Focus::Articles,
            _ => {}
        }
    }

    // ----- mouse

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> AppResult {
        match &mut self.popup {
            Some(Popup::Form(form, _)) => {
                return match form.handle_mouse(mouse, area) {
                    FormResult::Submit => self.handle_key(key(KeyCode::Enter)),
                    FormResult::Cancel => self.handle_key(key(KeyCode::Esc)),
                    FormResult::Continue => AppResult::Continue,
                };
            }
            Some(Popup::ConfirmDelete(i)) => {
                let i = *i;
                let message = self.confirm_message(i);
                if let Some(k) = confirm_click(area, &message, mouse) {
                    return self.handle_key(k);
                }
                return AppResult::Continue;
            }
            Some(Popup::Help) => {
                if is_left_click(mouse) {
                    self.popup = None;
                }
                return AppResult::Continue;
            }
            None => {}
        }
        if let Some(k) = shortcut_click(area, self.shortcuts(), mouse) {
            return self.handle_key(k);
        }
        let hit = self.hits.at(mouse.column, mouse.row);
        let (up, down) = (key(KeyCode::Up), key(KeyCode::Down));
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let k = if mouse.kind == MouseEventKind::ScrollUp { up } else { down };
                match hit {
                    Some(RssHit::FeedsBox | RssHit::Feed(_)) => self.feeds_key(k),
                    Some(RssHit::ArticlesBox | RssHit::Article(_)) => self.articles_key(k),
                    Some(RssHit::ContentBox | RssHit::Link) => {
                        for _ in 0..3 {
                            self.content_key(k);
                        }
                    }
                    _ => {}
                }
                return AppResult::Continue;
            }
            _ if !is_left_click(mouse) => return AppResult::Continue,
            _ => {}
        }
        self.command = None;
        self.search_typing = false;
        match hit {
            Some(RssHit::FeedsBox) => self.focus = Focus::Feeds,
            Some(RssHit::Feed(i)) => {
                if self.focus == Focus::Feeds && i == self.feed_sel {
                    self.focus = Focus::Articles;
                } else {
                    self.focus = Focus::Feeds;
                    self.select_feed(i);
                }
            }
            Some(RssHit::ArticlesBox) => self.focus = Focus::Articles,
            Some(RssHit::Article(pos)) => {
                if self.focus == Focus::Articles && pos == self.article_sel {
                    self.open_in_browser();
                } else {
                    self.focus = Focus::Articles;
                    self.select_article(pos);
                    // Under the Unread filter, marking it read would make it
                    // vanish from the list under the pointer; r / Enter do it.
                    if self.filter != Filter::Unread {
                        if let Some(at) = self.current() {
                            if !self.article(at).read {
                                self.article_mut(at).read = true;
                                self.save();
                            }
                        }
                    }
                }
            }
            Some(RssHit::ContentBox) => self.focus = Focus::Content,
            Some(RssHit::Link) => self.open_in_browser(),
            Some(RssHit::HelpBox) | None => {}
        }
        AppResult::Continue
    }

    // ----- drawing

    fn confirm_message(&self, i: usize) -> String {
        let name = self.store.feeds.get(i).map(|f| sanitize(&f.name())).unwrap_or_default();
        format!("Delete feed \"{name}\" and its articles?")
    }

    fn shortcuts(&self) -> &'static [(&'static str, &'static str)] {
        if self.zen {
            return &[("Esc", "Leave Zen"), ("↑/↓", "Scroll"), ("o", "Browser"), ("m", "Mark"), ("?", "Help")];
        }
        match self.focus {
            Focus::Feeds => &[
                ("Esc", "Back"),
                ("↑/↓", "Feed"),
                ("Tab", "Panel"),
                ("s", "Sync"),
                ("a", "Add"),
                ("e", "Edit"),
                ("d", "Delete"),
                ("/", "Search"),
                ("?", "Help"),
            ],
            Focus::Articles => &[
                ("Esc", "Back"),
                ("↑/↓", "Article"),
                ("Enter", "Read"),
                ("o", "Browser"),
                ("r/u", "Read/Unread"),
                ("m/v", "Mark/Unmark"),
                ("1/2/3", "All/Unread/Marked"),
                ("z", "Zen"),
                ("s", "Sync"),
                ("?", "Help"),
            ],
            Focus::Content => &[
                ("Esc", "Back"),
                ("↑/↓", "Scroll"),
                ("Tab", "Panel"),
                ("o", "Browser"),
                ("m/v", "Mark/Unmark"),
                ("z", "Zen"),
                ("?", "Help"),
            ],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, title: &str, pal: &Palette) {
        self.hits.clear();
        let status = match (&self.command, self.search_typing) {
            (Some(line), _) => format!(":{line}_"),
            (None, true) => format!("Search: {}_   (Enter keep, Esc clear)", self.search.as_deref().unwrap_or("")),
            _ => self.status.clone(),
        };
        let desk = screen_chrome(frame, area, &format!("{title} - {} (RSS)", self.box_name), self.shortcuts(), &status, pal);
        if desk.width < 30 || desk.height < 8 {
            return;
        }
        if self.zen {
            self.render_content(frame, desk, pal);
        } else {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(28), Constraint::Length(1), Constraint::Min(20)])
                .split(desk);
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(40), Constraint::Length(1), Constraint::Min(5)])
                .split(cols[2]);
            self.render_feeds(frame, cols[0], pal);
            self.render_articles(frame, right[0], pal);
            self.render_content(frame, right[2], pal);
        }
        match &self.popup {
            Some(Popup::Form(form, _)) => form.render(frame, area, pal),
            Some(Popup::ConfirmDelete(i)) => render_confirm(frame, area, &self.confirm_message(*i), pal),
            Some(Popup::Help) => self.render_help(frame, area, pal),
            None => {}
        }
    }

    fn scroll_top(cell: &std::cell::Cell<usize>, selected: usize, height: usize) -> usize {
        let mut top = cell.get();
        if selected < top {
            top = selected;
        } else if height > 0 && selected >= top + height {
            top = selected + 1 - height;
        }
        cell.set(top);
        top
    }

    fn render_feeds(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let focused = self.focus == Focus::Feeds;
        let syncing = if self.syncing() { " ⟳" } else { "" };
        let inner = dm_box(frame, area, &format!("FEEDS{syncing}"), pal, pal.fill(), focused);
        self.hits.push(area, RssHit::FeedsBox);
        let total_unread: usize = self.store.feeds.iter().map(Feed::unread).sum();
        let marked: usize = self.store.feeds.iter().flat_map(|f| &f.articles).filter(|a| a.marked).count();
        let height = inner.height as usize;
        let sources = self.sources();
        let top = Self::scroll_top(&self.feed_top, self.feed_sel, height);
        let mut last_cat: Option<&str> = None;
        for (row, (idx, source)) in sources.iter().enumerate().skip(top).take(height).enumerate() {
            let (text, err) = match source {
                Source::All => (format!(" ≡ All articles ({total_unread})"), false),
                Source::Marked => (format!(" ★ Marked ({marked})"), false),
                Source::Feed(i) => {
                    let f = &self.store.feeds[*i];
                    let cat = if f.category.is_empty() || last_cat == Some(f.category.as_str()) {
                        String::new()
                    } else {
                        format!("{} › ", sanitize(&f.category))
                    };
                    last_cat = Some(f.category.as_str());
                    let unread = f.unread();
                    let count = if unread > 0 { format!(" ({unread})") } else { String::new() };
                    let mark = if f.error.is_some() { " !" } else { "" };
                    (format!("   {cat}{}{count}{mark}", sanitize(&f.name())), f.error.is_some())
                }
            };
            let mut style = if idx == self.feed_sel { pal.selected() } else { pal.fill() };
            if err {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if matches!(source, Source::Feed(i) if self.store.feeds[*i].unread() > 0) || matches!(source, Source::All) {
                style = style.add_modifier(Modifier::BOLD);
            }
            let rect = Rect { x: inner.x, y: inner.y + row as u16, width: inner.width, height: 1 };
            self.hits.push(rect, RssHit::Feed(idx));
            put_line(frame, inner, row as u16, &fit_width(&text, inner.width as usize), style);
        }
        if self.store.feeds.is_empty() && height > 4 {
            put_line(frame, inner, 3, " (no feeds - press a to add,", pal.fill().add_modifier(Modifier::ITALIC));
            put_line(frame, inner, 4, "  or :importopml FILE)", pal.fill().add_modifier(Modifier::ITALIC));
        }
    }

    fn render_articles(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let focused = self.focus == Focus::Articles;
        let list = self.articles();
        let filter = match self.filter {
            Filter::All => "",
            Filter::Unread => " · unread",
            Filter::Marked => " · marked",
        };
        let search = self.search.as_ref().map(|q| format!(" · /{}", sanitize(q))).unwrap_or_default();
        let source = match self.source() {
            Source::All => "ALL".to_string(),
            Source::Marked => "MARKED".to_string(),
            Source::Feed(i) => sanitize(&self.store.feeds[i].name()).to_uppercase(),
        };
        let inner = dm_box(frame, area, &format!("{source} ({}){filter}{search}", list.len()), pal, pal.fill(), focused);
        self.hits.push(area, RssHit::ArticlesBox);
        if let Source::Feed(i) = self.source() {
            if let Some(err) = &self.store.feeds[i].error {
                put_line(frame, inner, 0, &format!(" ! {err}"), pal.fill().add_modifier(Modifier::ITALIC));
                if list.is_empty() {
                    return;
                }
            }
        }
        if list.is_empty() {
            let hint = if self.store.feeds.is_empty() { " (add a feed with a)" } else { " (no articles - s syncs)" };
            put_line(frame, inner, 0, hint, pal.fill().add_modifier(Modifier::ITALIC));
            return;
        }
        let height = inner.height as usize;
        let top = Self::scroll_top(&self.article_top, self.article_sel, height);
        let show_feed = !matches!(self.source(), Source::Feed(_));
        for (row, (pos, at)) in list.iter().enumerate().skip(top).take(height).enumerate() {
            let a = self.article(*at);
            let date = a
                .published
                .map(|d| d.with_timezone(&Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "           ".into());
            let flag = match (a.marked, a.read) {
                (true, _) => "★",
                (false, false) => "●",
                _ => " ",
            };
            let feed = if show_feed { format!("{} · ", sanitize(&self.store.feeds[at.0].name())) } else { String::new() };
            let text = format!(" {flag} {date}  {feed}{}", a.title);
            let mut style = if pos == self.article_sel { pal.selected() } else { pal.fill() };
            if !a.read {
                style = style.add_modifier(Modifier::BOLD);
            }
            self.hits.push(Rect { x: inner.x, y: inner.y + row as u16, width: inner.width, height: 1 }, RssHit::Article(pos));
            put_line(frame, inner, row as u16, &fit_width(&text, inner.width as usize), style);
        }
    }

    fn render_content(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let focused = self.focus == Focus::Content || self.zen;
        let title = if self.zen { "ZEN" } else { "ARTICLE" };
        let inner = dm_box(frame, area, title, pal, pal.fill(), focused);
        self.hits.push(area, RssHit::ContentBox);
        let Some(at) = self.current() else {
            put_line(frame, inner, 0, " (select an article)", pal.fill().add_modifier(Modifier::ITALIC));
            return;
        };
        let a = self.article(at);
        let width = inner.width.saturating_sub(2) as usize;
        let mut lines: Vec<(String, ratatui::style::Style, bool)> = Vec::new();
        for l in wrap_text(&a.title, width) {
            lines.push((l, pal.label(), false));
        }
        let mut meta = sanitize(&self.store.feeds[at.0].name());
        if !a.author.is_empty() {
            meta.push_str(&format!(" · {}", a.author));
        }
        if let Some(d) = a.published {
            meta.push_str(&format!(" · {}", d.with_timezone(&Local).format("%a %-d %b %Y %H:%M")));
        }
        if a.marked {
            meta.push_str(" · ★");
        }
        lines.push((meta, pal.fill().add_modifier(Modifier::ITALIC), false));
        if !a.link.is_empty() {
            lines.push((format!("↗ {}", sanitize(&a.link)), pal.fill().add_modifier(Modifier::UNDERLINED), true));
        }
        lines.push((String::new(), pal.fill(), false));
        let body = if a.text.trim().is_empty() { "(no text in the feed - press o to open it in the browser)" } else { &a.text };
        for para in body.split('\n') {
            for l in wrap_text(&sanitize(para), width) {
                lines.push((l, pal.fill(), false));
            }
            if para.trim().is_empty() {
                lines.push((String::new(), pal.fill(), false));
            }
        }
        let height = inner.height as usize;
        let max_scroll = lines.len().saturating_sub(height);
        let scroll = self.content_scroll.min(max_scroll);
        for (row, (text, style, link)) in lines.iter().skip(scroll).take(height).enumerate() {
            let rect = Rect { x: inner.x, y: inner.y + row as u16, width: inner.width, height: 1 };
            if *link {
                self.hits.push(rect, RssHit::Link);
            }
            put_line(frame, inner, row as u16, &format!(" {}", fit_width(text, width)), *style);
        }
    }

    fn render_help(&self, frame: &mut Frame, screen: Rect, pal: &Palette) {
        let width = 100.min(screen.width.saturating_sub(4));
        let height = (HELP.len() as u16 + 4).min(screen.height);
        let area = Rect {
            x: screen.x + screen.width.saturating_sub(width) / 2,
            y: screen.y + screen.height.saturating_sub(height) / 2,
            width,
            height,
        };
        let inner = dm_box(frame, area, "RSS KEYS", pal, pal.fill(), true);
        self.hits.push(area, RssHit::HelpBox);
        for (row, (k, what)) in HELP.iter().enumerate() {
            put_line(frame, inner, row as u16 + 1, &format!(" {k:<20} {what}"), pal.fill());
        }
        let _ = contains;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Test &amp; Co</title>
<item><title>First</title><link>https://example.com/1</link><guid>a1</guid>
<pubDate>Tue, 06 Oct 2026 10:00:00 GMT</pubDate><description>&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;&lt;p&gt;Two&lt;/p&gt;</description></item>
<item><title>Evil &#x1b;[31m title</title><link>https://example.com/2</link><guid>a2</guid>
<pubDate>Wed, 07 Oct 2026 10:00:00 GMT</pubDate><description>x</description></item>
</channel></rss>"#;

    #[test]
    fn parses_rss_and_strips_escapes() {
        let f = parse_feed(RSS.as_bytes()).unwrap();
        assert_eq!(f.title, "Test & Co");
        assert_eq!(f.articles.len(), 2);
        assert_eq!(f.articles[0].text, "Hello world\n\nTwo");
        assert!(!f.articles[1].title.contains('\x1b'));
        assert_eq!(f.articles[0].link, "https://example.com/1");
    }

    #[test]
    fn merge_keeps_flags_and_counts_new() {
        let mut store = Store { feeds: vec![Feed { url: "https://x/feed".into(), ..Feed::default() }] };
        assert_eq!(store.merge("https://x/feed", parse_feed(RSS.as_bytes()).unwrap()), 2);
        store.feeds[0].articles.iter_mut().for_each(|a| a.read = true);
        store.feeds[0].articles[0].marked = true;
        assert_eq!(store.merge("https://x/feed", parse_feed(RSS.as_bytes()).unwrap()), 0);
        assert!(store.feeds[0].articles.iter().all(|a| a.read));
        assert!(store.feeds[0].articles[0].marked);
        // Newest first.
        assert_eq!(store.feeds[0].articles[0].id, "a2");
        assert_eq!(store.feeds[0].feed_title, "Test & Co");
    }

    #[test]
    fn html_and_entities() {
        assert_eq!(html_to_text("<script>bad()</script><ul><li>a</li><li>b &lt;3</li></ul>"), "• a\n• b <3");
        assert_eq!(decode_entities("&#65;&#x42;&amp;&bogus;"), "AB&&bogus;");
        assert_eq!(sanitize("a\x1b[2Jb\u{202E}c\x07"), "a[2Jbc");
    }

    #[test]
    fn opml_round_trip_and_discovery() {
        let store = Store {
            feeds: vec![
                Feed { url: "https://a.example/rss".into(), title: "A & B".into(), category: "News".into(), ..Feed::default() },
                Feed { url: "https://c.example/atom".into(), title: "C".into(), ..Feed::default() },
            ],
        };
        let parsed = parse_opml(&to_opml(&store));
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&("https://a.example/rss".into(), "A & B".into(), "News".into())));
        assert!(parsed.contains(&("https://c.example/atom".into(), "C".into(), String::new())));
        let page = r#"<html><head><link rel="alternate" type="application/rss+xml" href="/feed.xml"></head></html>"#;
        assert_eq!(discover_feed(page, "https://site.example/blog/post"), Some("https://site.example/feed.xml".into()));
        assert_eq!(absolute_url("https://s.example/a/b", "c.xml"), "https://s.example/a/c.xml");
        assert!(!valid_url("file:///etc/passwd"));
        assert!(download("file:///etc/passwd").is_err());
    }

    #[test]
    fn app_keys_without_network() {
        let dir = std::env::temp_dir().join(format!("mm-rss-test-{}", std::process::id()));
        let path = dir.join("rss.json");
        let mut store = Store { feeds: vec![Feed { url: "https://x/feed".into(), ..Feed::default() }] };
        store.merge("https://x/feed", parse_feed(RSS.as_bytes()).unwrap());
        store.save(&path).unwrap();
        let mut app = RssApp::open("News", path.clone(), Some(0));
        // A fresh last_sync means no auto sync was started.
        assert!(!app.syncing());
        assert_eq!(app.articles().len(), 2);
        app.handle_key(key(KeyCode::Enter)); // read first
        app.handle_key(key(KeyCode::Char('2'))); // unread only
        assert_eq!(app.articles().len(), 1);
        app.handle_key(key(KeyCode::Char('1')));
        app.handle_key(key(KeyCode::Char('m')));
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.articles().len(), 1);
        app.handle_key(key(KeyCode::Char('1')));
        for c in "/evil".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.articles().len(), 1);
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.handle_key(key(KeyCode::Esc)), AppResult::Close));
        let saved = Store::load(&path);
        assert!(saved.feeds[0].articles.iter().any(|a| a.marked));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn headlines_are_newest_first_with_first_line() {
        let dir = std::env::temp_dir().join(format!("mm-rss-head-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rss.json");
        let json = r#"{"feeds":[
          {"url":"a","last_sync":"2099-01-01T00:00:00Z","articles":[
            {"id":"1","title":"Old","published":"2026-01-01T00:00:00Z","text":"\n\n  old first\nmore"},
            {"id":"2","title":"New\u001b[31m","published":"2026-03-01T00:00:00Z","text":"new first line\nsecond"}]},
          {"url":"b","last_sync":"2099-01-01T00:00:00Z","articles":[
            {"id":"3","title":"Middle","published":"2026-02-01T00:00:00Z","text":""}]}]}"#;
        std::fs::write(&path, json).unwrap();
        let list = headlines(&path, 2);
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].feed, list[0].article), (0, 1));
        assert!(!list[0].title.contains('\u{1b}'));
        assert_eq!(list[0].snippet, "new first line");
        assert_eq!(list[1].title, "Middle");
        assert_eq!(list[1].snippet, "");
        assert_eq!(headlines(&path, 10)[2].snippet, "old first");
        let app = RssApp::open_article("RSS", path.clone(), 0, 0);
        assert_eq!(app.current(), Some((0, 0)));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
