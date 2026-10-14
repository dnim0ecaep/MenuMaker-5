//! Weather box, modeled on `outside` (https://github.com/BaconIsAVeg/outside,
//! featured on Terminal Trove): a header with the place and conditions, a
//! "Current" panel (temperature, humidity, pressure, wind with gusts and
//! compass direction, UV index, precipitation, sunrise/sunset and a data-age
//! bar), a daily / 24-hour "Forecast" panel, and a "Bookmarks" list of saved
//! places on the right whose first entry, Automatic, finds you by IP address.
//!
//! Data comes from Open-Meteo (https://open-meteo.com, no API key) and
//! ip-api.com. Every download runs on a background thread so the screen never
//! freezes; `poll` picks up the results. Weather is cached for 10 minutes and
//! the IP location for 4 hours, both in the box's JSON data file, which also
//! keeps the bookmarks, the selected place and the units. The desktop box shows
//! the last cached report, `outside -o simple` style, without any network use.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::Frame;
use serde::{Deserialize, Serialize};

use crate::boxes::expand_tilde;
use crate::dmui::{
    confirm_click, dm_box, fit_width, is_left_click, put_line, render_confirm, screen_chrome,
    shortcut_click, wheel_key, FieldForm, FormField, FormResult, Hits, Palette,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// Weather older than this is fetched again (and the data-age bar is full).
const WEATHER_MAX_AGE: i64 = 10 * 60;
/// How long an IP-detected location is trusted.
const IP_MAX_AGE: i64 = 4 * 60 * 60;
/// Width of the Bookmarks panel, borders included.
const BOOKMARKS_WIDTH: u16 = 26;
/// Reports kept in the data file (one per place and units).
const MAX_CACHED_REPORTS: usize = 12;
const AUTOMATIC: &str = "Automatic";

const VIEW_SHORTCUTS: &[(&str, &str)] = &[
    ("Esc", "Back"),
    ("Enter", "Select"),
    ("a", "Add"),
    ("d", "Delete"),
    ("u", "Units"),
    ("f", "Forecast"),
    ("r", "Refresh"),
];
const PICK_SHORTCUTS: &[(&str, &str)] = &[("Esc", "Back"), ("↑/↓", "Move"), ("Enter", "Add")];

// ------------------------------------------------------------------ data

/// A geocoded place (a bookmark, or where the IP lookup put you).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub name: String,
    #[serde(default)]
    pub admin1: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub country_code: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl Location {
    /// Long form, for picking among search matches: `Paris, Île-de-France, France`.
    fn display(&self) -> String {
        let mut parts = vec![self.name.clone()];
        for part in [&self.admin1, &self.country] {
            if !part.is_empty() {
                parts.push(part.clone());
            }
        }
        parts.join(", ")
    }

    /// Short form, as `outside` writes it: `Edmonton, CA`.
    fn short(&self) -> String {
        let country = if self.country_code.is_empty() {
            &self.country
        } else {
            &self.country_code
        };
        if country.is_empty() {
            self.name.clone()
        } else {
            format!("{}, {country}", self.name)
        }
    }

    fn same_place(&self, other: &Location) -> bool {
        (self.latitude - other.latitude).abs() < 0.01 && (self.longitude - other.longitude).abs() < 0.01
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    #[default]
    Metric,
    Imperial,
}

impl Units {
    fn temp(self) -> &'static str {
        match self {
            Units::Metric => "°C",
            Units::Imperial => "°F",
        }
    }
    fn speed(self) -> &'static str {
        match self {
            Units::Metric => "km/h",
            Units::Imperial => "mph",
        }
    }
    fn rain(self) -> &'static str {
        match self {
            Units::Metric => "mm",
            Units::Imperial => "in",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Units::Metric => "metric",
            Units::Imperial => "imperial",
        }
    }
    fn toggled(self) -> Self {
        match self {
            Units::Metric => Units::Imperial,
            Units::Imperial => Units::Metric,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Current {
    pub temp: f64,
    pub feels: f64,
    pub code: i64,
    pub humidity: f64,
    pub pressure: f64,
    pub wind: f64,
    pub gusts: f64,
    pub wind_dir: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Day {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub code: i64,
    pub max: f64,
    pub min: f64,
    pub precip_sum: f64,
    pub precip_chance: i64,
    pub uv: f64,
    /// `HH:MM`, local time.
    pub sunrise: String,
    pub sunset: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hour {
    /// `HH:MM`, local time.
    pub time: String,
    pub temp: f64,
    pub precip: f64,
    pub chance: i64,
    pub code: i64,
}

/// Everything shown for one place, as fetched.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Which bookmark and units this is for; see [`report_key`].
    pub key: String,
    pub place: Location,
    pub units: Units,
    /// Unix time of the download.
    pub fetched_at: i64,
    pub current: Current,
    /// Today first.
    pub days: Vec<Day>,
    /// The next 24 hours, starting with the current one.
    pub hours: Vec<Hour>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CachedIp {
    place: Location,
    at: i64,
}

/// The box's data file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    units: Units,
    /// Saved places; the Automatic entry is not stored, it is always first.
    #[serde(default)]
    bookmarks: Vec<Location>,
    /// 0 = Automatic, n = `bookmarks[n - 1]`.
    #[serde(default)]
    selected: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ip: Option<CachedIp>,
    #[serde(default)]
    reports: Vec<Report>,
}

impl Store {
    /// Read the data file. A file from the first Weather box (just one saved
    /// Location) becomes a single bookmark, selected.
    fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        // The old file is a bare Location (it has a latitude at the top).
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if value.get("latitude").is_some() {
            return match serde_json::from_value::<Location>(value) {
                Ok(loc) => Store {
                    bookmarks: vec![loc],
                    selected: 1,
                    ..Store::default()
                },
                Err(_) => Self::default(),
            };
        }
        match serde_json::from_value::<Store>(value) {
            Ok(store) if store.selected <= store.bookmarks.len() => store,
            Ok(store) => Store { selected: 0, ..store },
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }

    /// The bookmark list as shown: Automatic, then the saved places.
    fn entries(&self) -> Vec<String> {
        std::iter::once(AUTOMATIC.to_string())
            .chain(self.bookmarks.iter().map(Location::short))
            .collect()
    }

    fn key_for(&self, entry: usize) -> String {
        report_key(entry.checked_sub(1).and_then(|i| self.bookmarks.get(i)), self.units)
    }

    fn report(&self, key: &str) -> Option<&Report> {
        self.reports.iter().find(|r| r.key == key)
    }

    fn put_report(&mut self, report: Report) {
        self.reports.retain(|r| r.key != report.key);
        self.reports.insert(0, report);
        self.reports.truncate(MAX_CACHED_REPORTS);
    }
}

/// Cache key: the place (or "auto") and the units.
fn report_key(place: Option<&Location>, units: Units) -> String {
    match place {
        None => format!("auto|{}", units.name()),
        Some(loc) => format!("{:.3},{:.3}|{}", loc.latitude, loc.longitude, units.name()),
    }
}

// ------------------------------------------------------------- formatting

/// WMO weather interpretation codes -> short text.
fn weather_text(code: i64) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 => "Light drizzle",
        53 => "Drizzle",
        55 => "Dense drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Light rain",
        63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Light snow",
        73 => "Snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80 => "Light showers",
        81 => "Showers",
        82 => "Violent showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm w/ hail",
        _ => "Unknown",
    }
}

/// A one-cell picture for a weather code (plain Unicode, no Nerd Font needed).
fn weather_icon(code: i64) -> &'static str {
    match code {
        0 | 1 => "☀",
        2 | 3 => "☁",
        45 | 48 => "≡",
        51..=67 | 80..=82 => "☂",
        71..=77 | 85 | 86 => "❄",
        95..=99 => "↯",
        _ => "?",
    }
}

/// Rain, drizzle, snow, showers or storms.
fn is_wet(code: i64) -> bool {
    matches!(code, 51..=67 | 71..=86 | 95..=99)
}

/// 16-point compass name for a wind direction in degrees.
fn compass(degrees: f64) -> &'static str {
    const POINTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW",
    ];
    let index = ((degrees.rem_euclid(360.0) / 22.5) + 0.5) as usize % 16;
    POINTS[index]
}

/// `HH:MM` (or an ISO date-time) -> `7:42am`.
fn twelve_hour(time: &str) -> String {
    let hm = time.rsplit('T').next().unwrap_or(time);
    let mut parts = hm.split(':');
    let (Some(Ok(h)), Some(m)) = (parts.next().map(str::parse::<u32>), parts.next()) else {
        return time.to_string();
    };
    let suffix = if h < 12 { "am" } else { "pm" };
    let h12 = match h % 12 {
        0 => 12,
        h => h,
    };
    format!("{h12}:{m}{suffix}")
}

/// `HH:MM` -> ` 9am` / `12pm`, for the hourly grid.
fn hour_label(time: &str) -> String {
    let h: u32 = time.split(':').next().and_then(|h| h.parse().ok()).unwrap_or(0);
    let suffix = if h < 12 { "am" } else { "pm" };
    let h12 = match h % 12 {
        0 => 12,
        h => h,
    };
    format!("{h12:>2}{suffix}")
}

/// Forecast row label: Today, Tomorrow, then `Wed 10/15`.
fn day_label(index: usize, date: &str) -> String {
    match index {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        _ => match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
            Ok(d) => format!("{} {:02}/{:02}", d.format("%a"), d.month(), d.day()),
            Err(_) => date.to_string(),
        },
    }
}

/// When it will rain (or stop) within the next 24 hours, from the hourly
/// forecast: an hour is wet with any precipitation or a 50%+ chance.
fn precipitation_note(hours: &[Hour]) -> Option<String> {
    let wet = |h: &Hour| h.precip > 0.0 || h.chance >= 50;
    let kind = |h: &Hour| if matches!(h.code, 71..=77 | 85 | 86) { "Snow" } else { "Rain" };
    let first = hours.first()?;
    if wet(first) {
        let lasts = hours.iter().take_while(|h| wet(h)).count();
        if lasts >= hours.len() {
            return Some(format!("{} for the next {} hours", kind(first), hours.len()));
        }
        let s = if lasts == 1 { "" } else { "s" };
        return Some(format!("{} stopping in about {lasts} hour{s}", kind(first)));
    }
    let start = hours.iter().position(wet)?;
    let s = if start == 1 { "" } else { "s" };
    Some(format!("{} likely in about {start} hour{s}", kind(&hours[start])))
}

fn round(value: f64) -> String {
    format!("{:.0}", value.round() + 0.0)
}

// ---------------------------------------------------------------- network

fn http_get(url: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(3)
        .user_agent(concat!("menu-maker/", env!("CARGO_PKG_VERSION"), " (Weather box)"))
        .build();
    let response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.to_string(),
    })?;
    response.into_string().map_err(|e| e.to_string())
}

/// Minimal percent-encoding for a query value.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Split `City, CC` into the name to search and an optional two-letter
/// country code to narrow the matches.
fn split_query(query: &str) -> (String, Option<String>) {
    let query = query.trim();
    if let Some((name, code)) = query.rsplit_once(',') {
        let code = code.trim();
        if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) {
            return (name.trim().to_string(), Some(code.to_ascii_uppercase()));
        }
    }
    (query.to_string(), None)
}

/// Search for places by name via the Open-Meteo geocoding API.
fn geocode(query: &str) -> Result<Vec<Location>, String> {
    let (name, code) = split_query(query);
    if name.is_empty() {
        return Err("Enter a city name, e.g. London, GB".into());
    }
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=10&language=en&format=json",
        urlencode(&name)
    );
    let json: serde_json::Value = serde_json::from_str(&http_get(&url)?).map_err(|e| e.to_string())?;
    let text = |r: &serde_json::Value, key: &str| r.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut out: Vec<Location> = json
        .get("results")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(Location {
                name: r.get("name")?.as_str()?.to_string(),
                admin1: text(r, "admin1"),
                country: text(r, "country"),
                country_code: text(r, "country_code"),
                latitude: r.get("latitude")?.as_f64()?,
                longitude: r.get("longitude")?.as_f64()?,
            })
        })
        .collect();
    if let Some(code) = &code {
        out.retain(|loc| loc.country_code.eq_ignore_ascii_case(code));
    }
    if out.is_empty() {
        return Err(format!("No match for \"{}\"", query.trim()));
    }
    Ok(out)
}

/// Where the IP address says you are (ip-api.com).
fn ip_location() -> Result<Location, String> {
    let body = http_get("http://ip-api.com/json/?fields=status,message,country,countryCode,regionName,city,lat,lon")?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let text = |key: &str| json.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if text("status") != "success" {
        return Err(format!("IP location failed: {}", text("message")));
    }
    Ok(Location {
        name: text("city"),
        admin1: text("regionName"),
        country: text("country"),
        country_code: text("countryCode"),
        latitude: json.get("lat").and_then(|v| v.as_f64()).ok_or("IP location: no latitude")?,
        longitude: json.get("lon").and_then(|v| v.as_f64()).ok_or("IP location: no longitude")?,
    })
}

fn forecast_url(loc: &Location, units: Units) -> String {
    let (temp, wind, rain) = match units {
        Units::Metric => ("celsius", "kmh", "mm"),
        Units::Imperial => ("fahrenheit", "mph", "inch"),
    };
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}\
&current=temperature_2m,apparent_temperature,relative_humidity_2m,pressure_msl,weather_code,\
wind_speed_10m,wind_direction_10m,wind_gusts_10m\
&hourly=temperature_2m,precipitation_probability,precipitation,weather_code\
&daily=weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset,uv_index_max,\
precipitation_sum,precipitation_probability_max\
&timezone=auto&forecast_days=7&temperature_unit={temp}&wind_speed_unit={wind}&precipitation_unit={rain}",
        loc.latitude, loc.longitude
    )
}

/// Turn an Open-Meteo forecast response into a [`Report`].
fn parse_forecast(body: &str, key: String, place: Location, units: Units, now: i64) -> Result<Report, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if let Some(reason) = json.get("reason").and_then(|v| v.as_str()) {
        return Err(reason.to_string());
    }
    let cur = json.get("current").ok_or("no current weather in the reply")?;
    let num = |v: &serde_json::Value, key: &str| v.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let current = Current {
        temp: num(cur, "temperature_2m"),
        feels: num(cur, "apparent_temperature"),
        code: cur.get("weather_code").and_then(|v| v.as_i64()).unwrap_or(-1),
        humidity: num(cur, "relative_humidity_2m"),
        pressure: num(cur, "pressure_msl"),
        wind: num(cur, "wind_speed_10m"),
        gusts: num(cur, "wind_gusts_10m"),
        wind_dir: num(cur, "wind_direction_10m"),
    };
    let now_local = cur.get("time").and_then(|v| v.as_str()).unwrap_or("");

    let column = |block: &serde_json::Value, key: &str| -> Vec<serde_json::Value> {
        block.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default()
    };
    let at_f = |col: &[serde_json::Value], i: usize| col.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let at_i = |col: &[serde_json::Value], i: usize| col.get(i).and_then(|v| v.as_i64()).unwrap_or(0);
    let at_s = |col: &[serde_json::Value], i: usize| {
        col.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let hhmm = |s: String| s.rsplit('T').next().unwrap_or("").to_string();

    let mut days = Vec::new();
    if let Some(daily) = json.get("daily") {
        let dates = column(daily, "time");
        let (codes, maxs, mins) = (
            column(daily, "weather_code"),
            column(daily, "temperature_2m_max"),
            column(daily, "temperature_2m_min"),
        );
        let (sums, chances, uvs) = (
            column(daily, "precipitation_sum"),
            column(daily, "precipitation_probability_max"),
            column(daily, "uv_index_max"),
        );
        let (rises, sets) = (column(daily, "sunrise"), column(daily, "sunset"));
        for i in 0..dates.len() {
            days.push(Day {
                date: at_s(&dates, i),
                code: at_i(&codes, i),
                max: at_f(&maxs, i),
                min: at_f(&mins, i),
                precip_sum: at_f(&sums, i),
                precip_chance: at_i(&chances, i),
                uv: at_f(&uvs, i),
                sunrise: hhmm(at_s(&rises, i)),
                sunset: hhmm(at_s(&sets, i)),
            });
        }
    }

    let mut hours = Vec::new();
    if let Some(hourly) = json.get("hourly") {
        let times = column(hourly, "time");
        let (temps, rain, chances, codes) = (
            column(hourly, "temperature_2m"),
            column(hourly, "precipitation"),
            column(hourly, "precipitation_probability"),
            column(hourly, "weather_code"),
        );
        // Start at the current local hour.
        let this_hour = now_local.get(..13).unwrap_or("");
        let start = times
            .iter()
            .position(|t| t.as_str().is_some_and(|t| !this_hour.is_empty() && t.starts_with(this_hour)))
            .unwrap_or(0);
        for i in start..(start + 24).min(times.len()) {
            hours.push(Hour {
                time: hhmm(at_s(&times, i)),
                temp: at_f(&temps, i),
                precip: at_f(&rain, i),
                chance: at_i(&chances, i),
                code: at_i(&codes, i),
            });
        }
    }

    Ok(Report {
        key,
        place,
        units,
        fetched_at: now,
        current,
        days,
        hours,
    })
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

// --------------------------------------------------------------- the app

pub enum AppResult {
    Continue,
    Close,
}

/// What a background download sends back.
enum Msg {
    /// (request id, report, the IP location if it was looked up).
    Weather(u64, Result<(Report, Option<Location>), String>),
    Search(Result<Vec<Location>, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForecastMode {
    Daily,
    Hourly,
}

enum Overlay {
    None,
    /// Typing a place to add.
    Add(FieldForm),
    /// Several places matched: pick one.
    Pick(Vec<Location>, usize),
    /// Delete this bookmark (entry index)?
    ConfirmDelete(usize, String),
}

#[derive(Clone, Copy)]
enum Hit {
    Bookmark(usize),
    Pick(usize),
}

pub struct WeatherApp {
    box_name: String,
    path: PathBuf,
    store: Store,
    /// Highlighted entry in the Bookmarks list (Enter switches to it).
    cursor: usize,
    mode: ForecastMode,
    overlay: Overlay,
    status: String,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Id of the weather download in flight, if any.
    loading: Option<u64>,
    next_request: u64,
    searching: bool,
    hits: Hits<Hit>,
}

impl WeatherApp {
    pub fn open(box_name: &str, path: PathBuf) -> Self {
        let path = expand_tilde(&path.to_string_lossy());
        let store = Store::load(&path);
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            box_name: box_name.to_string(),
            path,
            cursor: store.selected,
            store,
            mode: ForecastMode::Daily,
            overlay: Overlay::None,
            status: String::new(),
            tx,
            rx,
            loading: None,
            next_request: 0,
            searching: false,
            hits: Hits::default(),
        };
        app.refresh_if_stale(false);
        app
    }

    fn save(&mut self) {
        if let Err(err) = self.store.save(&self.path) {
            self.status = format!("Save failed: {err}");
        }
    }

    /// The report for the selected place and units, fresh or not.
    fn report(&self) -> Option<&Report> {
        self.store.report(&self.store.key_for(self.store.selected))
    }

    fn age(&self) -> Option<i64> {
        self.report().map(|r| (now_unix() - r.fetched_at).max(0))
    }

    /// Fetch the selected place unless its cached report is under 10 minutes
    /// old (`force` fetches anyway).
    fn refresh_if_stale(&mut self, force: bool) {
        let fresh = self.age().is_some_and(|age| age < WEATHER_MAX_AGE);
        if fresh && !force {
            // Drop any download still running for the place shown before.
            self.loading = None;
            self.status = self.updated_text();
            return;
        }
        self.start_fetch();
    }

    fn updated_text(&self) -> String {
        match (self.report(), self.age()) {
            (Some(report), Some(age)) => format!("{} - updated {} ago", report.place.short(), age_text(age)),
            _ => String::new(),
        }
    }

    /// Start a background download for the selected place.
    fn start_fetch(&mut self) {
        let entry = self.store.selected;
        let units = self.store.units;
        let key = self.store.key_for(entry);
        let bookmark = entry.checked_sub(1).and_then(|i| self.store.bookmarks.get(i)).cloned();
        let cached_ip = self
            .store
            .ip
            .as_ref()
            .filter(|ip| now_unix() - ip.at < IP_MAX_AGE)
            .map(|ip| ip.place.clone());
        self.next_request += 1;
        let id = self.next_request;
        self.loading = Some(id);
        if cfg!(test) {
            return; // tests never touch the network
        }
        self.status = match &bookmark {
            Some(loc) => format!("Loading weather for {}…", loc.short()),
            None => "Finding your location and loading the weather…".into(),
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let (place, looked_up) = match bookmark {
                    Some(loc) => (loc, None),
                    None => match cached_ip {
                        Some(loc) => (loc, None),
                        None => {
                            let loc = ip_location()?;
                            (loc.clone(), Some(loc))
                        }
                    },
                };
                let body = http_get(&forecast_url(&place, units))?;
                let report = parse_forecast(&body, key, place, units, now_unix())?;
                Ok((report, looked_up))
            })();
            let _ = tx.send(Msg::Weather(id, result));
        });
    }

    fn start_search(&mut self, query: String) {
        self.searching = true;
        self.status = format!("Searching for \"{}\"…", query.trim());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Search(geocode(&query)));
        });
    }

    /// Take finished downloads, and refetch when the data passes 10 minutes
    /// (the main loop calls this a few times a second).
    pub fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Weather(id, result) => {
                    if self.loading != Some(id) {
                        continue; // a newer request replaced it
                    }
                    self.loading = None;
                    match result {
                        Ok((report, looked_up)) => {
                            if let Some(place) = looked_up {
                                self.store.ip = Some(CachedIp { place, at: now_unix() });
                            }
                            self.store.put_report(report);
                            self.save();
                            self.status = self.updated_text();
                        }
                        Err(err) => {
                            self.status = match self.report() {
                                Some(_) => format!("Offline / update failed ({err}) - showing saved data"),
                                None => format!("Offline / fetch failed: {err}"),
                            };
                        }
                    }
                }
                Msg::Search(result) => {
                    self.searching = false;
                    match result {
                        Ok(found) if found.len() == 1 => {
                            self.overlay = Overlay::None;
                            self.add_bookmark(found.into_iter().next().unwrap());
                        }
                        Ok(found) => {
                            self.status = format!("{} matches - pick one", found.len());
                            self.overlay = Overlay::Pick(found, 0);
                        }
                        Err(err) => {
                            if let Overlay::Add(form) = &mut self.overlay {
                                form.error = Some(err.clone());
                            }
                            self.status = err;
                        }
                    }
                }
            }
        }
        let stale = self.age().is_some_and(|age| age >= WEATHER_MAX_AGE);
        if self.loading.is_none() && stale && matches!(self.overlay, Overlay::None) {
            self.start_fetch();
        }
    }

    /// Show the weather for an entry of the Bookmarks list.
    fn switch_to(&mut self, entry: usize) {
        if entry > self.store.bookmarks.len() {
            return;
        }
        self.cursor = entry;
        self.store.selected = entry;
        self.save();
        self.refresh_if_stale(false);
    }

    fn add_bookmark(&mut self, place: Location) {
        let entry = match self.store.bookmarks.iter().position(|b| b.same_place(&place)) {
            Some(i) => {
                self.status = format!("{} is already a bookmark", place.short());
                i + 1
            }
            None => {
                self.store.bookmarks.push(place);
                self.store.bookmarks.len()
            }
        };
        self.switch_to(entry);
    }

    fn delete_bookmark(&mut self, entry: usize) {
        if entry == 0 || entry > self.store.bookmarks.len() {
            return;
        }
        let gone = self.store.bookmarks.remove(entry - 1);
        let key = report_key(Some(&gone), Units::Metric);
        let key_imperial = report_key(Some(&gone), Units::Imperial);
        self.store.reports.retain(|r| r.key != key && r.key != key_imperial);
        if self.store.selected == entry {
            self.store.selected = 0;
        } else if self.store.selected > entry {
            self.store.selected -= 1;
        }
        self.cursor = self.cursor.min(self.store.bookmarks.len());
        self.save();
        self.status = format!("Deleted {}", gone.short());
        self.refresh_if_stale(false);
    }

    fn toggle_units(&mut self) {
        self.store.units = self.store.units.toggled();
        self.save();
        self.refresh_if_stale(false);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        match &mut self.overlay {
            Overlay::Add(form) => {
                if self.searching {
                    if key.code == KeyCode::Esc {
                        self.searching = false;
                        self.overlay = Overlay::None;
                    }
                    return AppResult::Continue;
                }
                match form.handle_key(key) {
                    FormResult::Continue => {}
                    FormResult::Cancel => self.overlay = Overlay::None,
                    FormResult::Submit => {
                        let query = form.value(0).to_string();
                        if split_query(&query).0.is_empty() {
                            form.error = Some("Enter a city, e.g. London, GB".into());
                        } else {
                            self.start_search(query);
                        }
                    }
                }
                return AppResult::Continue;
            }
            Overlay::Pick(found, sel) => {
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1).min(found.len().saturating_sub(1)),
                    KeyCode::Enter => {
                        let place = found[*sel].clone();
                        self.overlay = Overlay::None;
                        self.add_bookmark(place);
                    }
                    _ => {}
                }
                return AppResult::Continue;
            }
            Overlay::ConfirmDelete(entry, _) => {
                let entry = *entry;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.overlay = Overlay::None;
                        self.delete_bookmark(entry);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.overlay = Overlay::None,
                    _ => {}
                }
                return AppResult::Continue;
            }
            Overlay::None => {}
        }
        let count = self.store.bookmarks.len() + 1;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return AppResult::Close,
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(count - 1),
            KeyCode::Enter => self.switch_to(self.cursor),
            KeyCode::Char('a') => {
                self.overlay = Overlay::Add(FieldForm::new(
                    "Add Location (City, Country)",
                    vec![FormField::text("Place", "")],
                ));
            }
            KeyCode::Char('d') => {
                if self.cursor == 0 {
                    self.status = "Automatic can't be deleted".into();
                } else {
                    let name = self.store.entries()[self.cursor].clone();
                    self.overlay = Overlay::ConfirmDelete(self.cursor, format!("Delete '{name}'?"));
                }
            }
            KeyCode::Char('u') => self.toggle_units(),
            KeyCode::Char('f') => {
                self.mode = match self.mode {
                    ForecastMode::Daily => ForecastMode::Hourly,
                    ForecastMode::Hourly => ForecastMode::Daily,
                };
            }
            KeyCode::Char('r') => self.refresh_if_stale(true),
            _ => {}
        }
        AppResult::Continue
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> AppResult {
        match &mut self.overlay {
            Overlay::Add(form) => {
                if !self.searching {
                    match form.handle_mouse(mouse, area) {
                        FormResult::Continue => {}
                        FormResult::Cancel => self.overlay = Overlay::None,
                        FormResult::Submit => return self.handle_key(crate::dmui::key(KeyCode::Enter)),
                    }
                }
                return AppResult::Continue;
            }
            Overlay::ConfirmDelete(_, message) => {
                if let Some(key) = confirm_click(area, &message.clone(), mouse) {
                    return self.handle_key(key);
                }
                return AppResult::Continue;
            }
            Overlay::Pick(..) | Overlay::None => {}
        }
        let shortcuts = if matches!(self.overlay, Overlay::Pick(..)) { PICK_SHORTCUTS } else { VIEW_SHORTCUTS };
        if let Some(key) = shortcut_click(area, shortcuts, mouse) {
            return self.handle_key(key);
        }
        if let Some(key) = wheel_key(mouse) {
            return self.handle_key(key);
        }
        if is_left_click(mouse) {
            match self.hits.at(mouse.column, mouse.row) {
                Some(Hit::Bookmark(entry)) => self.switch_to(entry),
                Some(Hit::Pick(i)) => {
                    if let Overlay::Pick(found, _) = &self.overlay {
                        let place = found[i].clone();
                        self.overlay = Overlay::None;
                        self.add_bookmark(place);
                    }
                }
                None => {}
            }
        }
        AppResult::Continue
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _title: &str, pal: &Palette) {
        self.hits.clear();
        let title = format!("Weather — {}", self.box_name);
        let shortcuts = if matches!(self.overlay, Overlay::Pick(..)) { PICK_SHORTCUTS } else { VIEW_SHORTCUTS };
        let units = format!("{} | {}", self.status, self.store.units.name());
        let inner = screen_chrome(frame, area, &title, shortcuts, units.trim_start_matches(" | "), pal);
        if inner.width < 10 || inner.height < 6 {
            return;
        }

        // Bookmarks on the right when there is room, the weather on the left.
        let side = if inner.width >= 60 { BOOKMARKS_WIDTH } else { 0 };
        let left = Rect { width: inner.width - side - u16::from(side > 0), ..inner };
        if side > 0 {
            let right = Rect { x: inner.x + inner.width - side, width: side, ..inner };
            self.render_bookmarks(frame, right, pal);
        }

        let header = Rect { height: 4.min(left.height), ..left };
        let forecast_height = 10.min(left.height.saturating_sub(header.height + 3));
        let forecast = Rect {
            y: left.y + left.height - forecast_height,
            height: forecast_height,
            ..left
        };
        let current = Rect {
            y: header.y + header.height,
            height: left.height - header.height - forecast_height,
            ..left
        };
        self.render_header(frame, header, pal);
        self.render_current(frame, current, pal);
        if forecast_height >= 3 {
            self.render_forecast(frame, forecast, pal);
        }

        match &self.overlay {
            Overlay::None => {}
            Overlay::Add(form) => form.render(frame, area, pal),
            Overlay::Pick(found, sel) => self.render_pick(frame, inner, found, *sel, pal),
            Overlay::ConfirmDelete(_, message) => render_confirm(frame, area, message, pal),
        }
    }

    fn loading_text(&self) -> &'static str {
        if self.loading.is_some() {
            "Loading weather data…"
        } else {
            "No weather yet - press r to try again"
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let inner = dm_box(frame, area, "Weather", pal, pal.fill(), false);
        let Some(r) = self.report() else {
            put_centered(frame, inner, 0, self.loading_text(), pal.fill().add_modifier(Modifier::ITALIC));
            return;
        };
        let u = r.units.temp();
        put_centered(frame, inner, 0, &r.place.short(), pal.label());
        let line = format!(
            "{} {}{u} • {} • Feels like {}{u}",
            weather_icon(r.current.code),
            round(r.current.temp),
            weather_text(r.current.code),
            round(r.current.feels),
        );
        put_centered(frame, inner, 1, &line, pal.fill().add_modifier(Modifier::BOLD));
    }

    fn render_current(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let inner = dm_box(frame, area, "Current", pal, pal.fill(), false);
        let Some(r) = self.report() else {
            return;
        };
        let rows = current_rows(r);
        for (row, (label, value)) in rows.iter().enumerate() {
            let row = row as u16;
            if row + 1 >= inner.height {
                break;
            }
            put_line(frame, inner, row, &format!(" {label:<15}"), pal.label());
            let value_area = Rect { x: inner.x + 16, width: inner.width.saturating_sub(16), ..inner };
            put_line(frame, value_area, row, value, pal.fill());
        }
        // Data-age bar on the last row: full when the data is due for refresh.
        if inner.height >= 2 {
            let age = self.age().unwrap_or(0);
            let label = format!(" Updated {} ago ", age_text(age));
            let bar_width = (inner.width as usize).saturating_sub(label.chars().count() + 2);
            let filled = ((age.min(WEATHER_MAX_AGE) as usize) * bar_width) / WEATHER_MAX_AGE as usize;
            let bar = format!("{label}{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
            put_line(frame, inner, inner.height - 1, &bar, pal.fill().remove_modifier(Modifier::BOLD));
        }
    }

    fn render_forecast(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let title = match self.mode {
            ForecastMode::Daily => "Forecast",
            ForecastMode::Hourly => "Next 24 hours",
        };
        let inner = dm_box(frame, area, title, pal, pal.fill(), false);
        let Some(r) = self.report() else {
            return;
        };
        let lines = match self.mode {
            ForecastMode::Daily => daily_lines(r),
            ForecastMode::Hourly => hourly_lines(r, inner.width as usize, inner.height as usize),
        };
        for (row, line) in lines.iter().enumerate().take(inner.height as usize) {
            put_line(frame, inner, row as u16, line, pal.fill());
        }
    }

    fn render_bookmarks(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let inner = dm_box(frame, area, "Bookmarks", pal, pal.fill(), true);
        let entries = self.store.entries();
        let offset = self.cursor.saturating_sub(inner.height.saturating_sub(1) as usize);
        for (row, (entry, name)) in entries.iter().enumerate().skip(offset).enumerate() {
            if row as u16 >= inner.height {
                break;
            }
            let marker = if entry == self.store.selected { "►" } else { " " };
            let style = if entry == self.cursor { pal.selected() } else { pal.fill() };
            put_line(frame, inner, row as u16, &format!("{marker}{name}"), style);
            self.hits.push(Rect { y: inner.y + row as u16, height: 1, ..inner }, Hit::Bookmark(entry));
        }
    }

    fn render_pick(&self, frame: &mut Frame, screen: Rect, found: &[Location], sel: usize, pal: &Palette) {
        let width = found
            .iter()
            .map(|l| l.display().chars().count() as u16 + 6)
            .max()
            .unwrap_or(30)
            .clamp(30, screen.width);
        let height = (found.len() as u16 + 2).min(screen.height);
        let area = Rect {
            x: screen.x + (screen.width - width) / 2,
            y: screen.y + (screen.height - height) / 2,
            width,
            height,
        };
        let inner = dm_box(frame, area, "Pick a place", pal, pal.fill(), true);
        for (row, place) in found.iter().enumerate().take(inner.height as usize) {
            let style = if row == sel { pal.selected() } else { pal.fill() };
            put_line(frame, inner, row as u16, &format!(" {}", place.display()), style);
            self.hits.push(Rect { y: inner.y + row as u16, height: 1, ..inner }, Hit::Pick(row));
        }
    }
}

fn put_centered(frame: &mut Frame, area: Rect, row: u16, text: &str, style: ratatui::style::Style) {
    let text = fit_width(text, area.width as usize);
    let pad = (area.width as usize).saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str())) / 2;
    put_line(frame, area, row, &format!("{}{text}", " ".repeat(pad)), style);
}

fn age_text(seconds: i64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{} h", s / 3600),
    }
}

/// The Current panel, as (label, value) rows.
fn current_rows(r: &Report) -> Vec<(&'static str, String)> {
    let (t, speed, rain) = (r.units.temp(), r.units.speed(), r.units.rain());
    let c = &r.current;
    let mut rows = vec![
        ("Temperature:", format!("{}{t}", round(c.temp))),
        ("Humidity:", format!("{}%", round(c.humidity))),
        ("Pressure:", format!("{:.1} hPa", c.pressure)),
        (
            "Wind:",
            format!(
                "{} {speed} with gusts up to {} {speed} ({})",
                round(c.wind),
                round(c.gusts),
                compass(c.wind_dir)
            ),
        ),
    ];
    if let Some(today) = r.days.first() {
        rows.push(("Today:", format!("{} → {}{t}", round(today.min), round(today.max))));
        rows.push(("UV Index:", format!("{:.1}", today.uv)));
        rows.push((
            "Precipitation:",
            format!("{:.1} {rain} ({}% chance)", today.precip_sum, today.precip_chance),
        ));
    }
    if let Some(note) = precipitation_note(&r.hours) {
        rows.push(("", note));
    }
    if let Some(today) = r.days.first() {
        rows.push((
            "Sun:",
            format!("↑ {} • ↓ {}", twelve_hour(&today.sunrise), twelve_hour(&today.sunset)),
        ));
    }
    rows
}

/// Daily forecast rows: `Today      ☂   9 → 22°C  Light rain (53%)`.
fn daily_lines(r: &Report) -> Vec<String> {
    r.days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let text = if is_wet(d.code) {
                format!("{} ({}%)", weather_text(d.code), d.precip_chance)
            } else {
                weather_text(d.code).to_string()
            };
            format!(
                " {:<10} {}  {:>2} → {:<2}{}  {text}",
                day_label(i, &d.date),
                weather_icon(d.code),
                round(d.min),
                round(d.max),
                r.units.temp()
            )
        })
        .collect()
}

/// The next 24 hours in a grid read down then across: up to 3 columns of
/// `height` rows. Cells drop the rain amount (keeping the chance) when three
/// full cells don't fit.
fn hourly_lines(r: &Report, width: usize, height: usize) -> Vec<String> {
    const FULL: usize = 24;
    const COMPACT: usize = 17;
    const GAP: usize = 3;
    let full = width >= 3 * FULL + 2 * GAP + 1;
    let cell_width = if full { FULL } else { COMPACT };
    let cols = ((width + GAP) / (cell_width + GAP)).clamp(1, 3);
    let rows = height.max(1).min(r.hours.len().div_ceil(cols).max(1));
    let cell = |h: &Hour| {
        let rain = if full { format!(" {:>4.1}{}", h.precip, r.units.rain()) } else { String::new() };
        format!(
            "{} {} {:>3}{}{rain} {:>3}%",
            hour_label(&h.time),
            weather_icon(h.code),
            round(h.temp),
            r.units.temp(),
            h.chance
        )
    };
    (0..rows)
        .map(|row| {
            let mut line = String::from(" ");
            for col in 0..cols {
                if let Some(h) = r.hours.get(col * rows + row) {
                    let text = cell(h);
                    let pad = cell_width.saturating_sub(text.chars().count());
                    line.push_str(&text);
                    if col + 1 < cols {
                        line.push_str(&" ".repeat(pad + GAP));
                    }
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// Desktop box body: the last report for the selected place, in the spirit
/// of `outside -o simple`, or the place name / a hint. Never touches the
/// network.
pub fn preview_lines(path: &Path) -> Vec<(String, Option<(usize, usize)>)> {
    let store = Store::load(path);
    let entries = store.entries();
    let Some(r) = store.report(&store.key_for(store.selected)) else {
        let name = entries.get(store.selected).cloned().unwrap_or_default();
        return vec![(name, None), ("Enter to open".into(), None)];
    };
    let t = r.units.temp();
    let mut lines = vec![
        (r.place.short(), None),
        (
            format!("{} {}{t} {}", weather_icon(r.current.code), round(r.current.temp), weather_text(r.current.code)),
            None,
        ),
        (
            format!("Wind {}{} {}", round(r.current.wind), r.units.speed(), compass(r.current.wind_dir)),
            None,
        ),
    ];
    if let Some(today) = r.days.first() {
        lines.push((
            format!("{}→{}{t} | Rain {}%", round(today.min), round(today.max), today.precip_chance),
            None,
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "current": {"time": "2026-10-13T14:15", "temperature_2m": 17.6, "apparent_temperature": 17.5,
            "relative_humidity_2m": 72, "pressure_msl": 1006.9, "weather_code": 3,
            "wind_speed_10m": 6.6, "wind_direction_10m": 257, "wind_gusts_10m": 13.0},
        "hourly": {
            "time": ["2026-10-13T13:00", "2026-10-13T14:00", "2026-10-13T15:00", "2026-10-13T16:00", "2026-10-13T17:00"],
            "temperature_2m": [17.0, 17.6, 18.0, 16.0, 15.0],
            "precipitation_probability": [0, 10, 20, 70, 80],
            "precipitation": [0.0, 0.0, 0.0, 0.4, 1.2],
            "weather_code": [3, 3, 3, 61, 63]},
        "daily": {
            "time": ["2026-10-13", "2026-10-14", "2026-10-15"],
            "weather_code": [80, 95, 0],
            "temperature_2m_max": [21.7, 21.0, 24.0], "temperature_2m_min": [9.1, 13.0, 11.0],
            "sunrise": ["2026-10-13T07:42", "2026-10-14T07:44", "2026-10-15T07:45"],
            "sunset": ["2026-10-13T18:31", "2026-10-14T18:29", "2026-10-15T18:27"],
            "uv_index_max": [3.2, 2.0, 4.0], "precipitation_sum": [0.8, 4.0, 0.0],
            "precipitation_probability_max": [53, 90, 0]}
    }"#;

    fn sample() -> Report {
        let place = Location {
            name: "Edmonton".into(),
            country_code: "CA".into(),
            country: "Canada".into(),
            latitude: 53.55,
            longitude: -113.47,
            ..Location::default()
        };
        parse_forecast(SAMPLE, "k".into(), place, Units::Metric, 1_000).unwrap()
    }

    #[test]
    fn parses_an_open_meteo_reply() {
        let r = sample();
        assert_eq!(r.current.code, 3);
        assert_eq!(r.current.pressure, 1006.9);
        assert_eq!(r.days.len(), 3);
        assert_eq!(r.days[0].sunrise, "07:42");
        assert_eq!(r.days[1].precip_chance, 90);
        // Hours start at the current local hour (14:00), not the first one.
        assert_eq!(r.hours[0].time, "14:00");
        assert_eq!(r.hours.len(), 4);
        assert!(parse_forecast(r#"{"error":true,"reason":"bad lat"}"#, "k".into(), Location::default(), Units::Metric, 0)
            .unwrap_err()
            .contains("bad lat"));
    }

    #[test]
    fn current_panel_reads_like_outside() {
        let rows = current_rows(&sample());
        let find = |label: &str| rows.iter().find(|(l, _)| *l == label).unwrap().1.clone();
        assert_eq!(find("Temperature:"), "18°C");
        assert_eq!(find("Pressure:"), "1006.9 hPa");
        assert_eq!(find("Wind:"), "7 km/h with gusts up to 13 km/h (WSW)");
        assert_eq!(find("Precipitation:"), "0.8 mm (53% chance)");
        assert_eq!(find("Sun:"), "↑ 7:42am • ↓ 6:31pm");
        assert_eq!(find(""), "Rain likely in about 2 hours");
    }

    #[test]
    fn forecast_rows() {
        let r = sample();
        let daily = daily_lines(&r);
        assert_eq!(daily[0], " Today      ☂   9 → 22°C  Light showers (53%)");
        assert!(daily[1].contains("Tomorrow"));
        assert!(daily[2].contains("Thu 10/15") && daily[2].ends_with("Clear sky"), "{}", daily[2]);
        let hourly = hourly_lines(&r, 90, 8);
        assert_eq!(hourly.len(), 2); // 4 hours over 3 columns -> 2 rows
        assert!(hourly[0].starts_with("  2pm ☁  18°C  0.0mm  10%"), "{}", hourly[0]);
        assert!(hourly[0].contains(" 4pm ☂  16°C  0.4mm  70%"), "{}", hourly[0]);
        // Narrower: compact cells (no rain amount) so all 3 columns still fit.
        let narrow = hourly_lines(&r, 69, 8);
        assert_eq!(narrow[0], "  2pm ☁  18°C  10%    4pm ☂  16°C  70%");
    }

    #[test]
    fn helpers() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(359.0), "N");
        assert_eq!(compass(90.0), "E");
        assert_eq!(compass(257.0), "WSW");
        assert_eq!(twelve_hour("00:05"), "12:05am");
        assert_eq!(twelve_hour("2026-10-13T12:30"), "12:30pm");
        assert_eq!(hour_label("09:00"), " 9am");
        assert_eq!(split_query("London, gb"), ("London".into(), Some("GB".into())));
        assert_eq!(split_query("Paris, Texas"), ("Paris, Texas".into(), None));
        assert_eq!(urlencode("São Paulo"), "S%C3%A3o%20Paulo");
        assert_eq!(weather_text(61), "Light rain");
        assert_eq!(age_text(125), "2 min");
        let hours = |chances: &[i64]| -> Vec<Hour> {
            chances.iter().map(|&chance| Hour { chance, ..Hour::default() }).collect()
        };
        assert_eq!(precipitation_note(&hours(&[0, 0])), None);
        assert_eq!(precipitation_note(&hours(&[80, 60, 0])).unwrap(), "Rain stopping in about 2 hours");
        assert_eq!(precipitation_note(&hours(&[80, 60])).unwrap(), "Rain for the next 2 hours");
    }

    #[test]
    fn store_round_trips_and_reads_the_old_format() {
        let dir = std::env::temp_dir().join(format!("weather-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weather.json");
        // The first Weather box saved just one Location.
        fs::write(&path, r#"{"name":"Oslo","country":"Norway","latitude":59.9,"longitude":10.7}"#).unwrap();
        let store = Store::load(&path);
        assert_eq!(store.bookmarks.len(), 1);
        assert_eq!(store.selected, 1);
        assert_eq!(store.entries(), vec!["Automatic".to_string(), "Oslo, Norway".into()]);
        assert_eq!(preview_lines(&path)[0].0, "Oslo, Norway");

        let mut store = store;
        let mut report = sample();
        report.key = store.key_for(1);
        store.put_report(report);
        store.units = Units::Metric;
        store.save(&path).unwrap();
        let back = Store::load(&path);
        assert_eq!(back.reports.len(), 1);
        let preview: Vec<String> = preview_lines(&path).into_iter().map(|(l, _)| l).collect();
        assert_eq!(preview, ["Edmonton, CA", "☁ 18°C Overcast", "Wind 7km/h WSW", "9→22°C | Rain 53%"]);
        // Units are part of the cache key.
        assert_ne!(back.key_for(1), report_key(back.bookmarks.first(), Units::Imperial));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bookmarks_add_switch_and_delete_without_network() {
        let dir = std::env::temp_dir().join(format!("weather-app-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weather.json");
        let mut app = WeatherApp::open("W", path.clone());
        let oslo = Location { name: "Oslo".into(), latitude: 59.9, longitude: 10.7, ..Location::default() };
        // A fresh cached report means switching does not download.
        let mut report = sample();
        report.key = report_key(Some(&oslo), Units::Metric);
        report.fetched_at = now_unix();
        app.store.put_report(report);
        app.add_bookmark(oslo.clone());
        assert_eq!(app.store.selected, 1);
        assert!(app.loading.is_none());
        assert!(app.status.contains("updated"), "{}", app.status);
        // Adding the same place again just selects it.
        app.add_bookmark(oslo);
        assert_eq!(app.store.bookmarks.len(), 1);
        // Automatic can't be deleted; a bookmark can, with a confirmation.
        app.cursor = 0;
        app.handle_key(crate::dmui::key(KeyCode::Char('d')));
        assert!(matches!(app.overlay, Overlay::None));
        app.cursor = 1;
        app.handle_key(crate::dmui::key(KeyCode::Char('d')));
        assert!(matches!(app.overlay, Overlay::ConfirmDelete(1, _)));
        // Deleting the shown place falls back to Automatic (which then fetches).
        app.handle_key(crate::dmui::key(KeyCode::Char('n')));
        assert_eq!(app.store.bookmarks.len(), 1);
        app.handle_key(crate::dmui::key(KeyCode::Char('d')));
        app.handle_key(crate::dmui::key(KeyCode::Char('y')));
        assert!(app.store.bookmarks.is_empty());
        assert_eq!(app.store.selected, 0);
        // f flips the forecast between daily and hourly.
        app.handle_key(crate::dmui::key(KeyCode::Char('f')));
        assert!(app.mode == ForecastMode::Hourly);
        fs::remove_dir_all(dir).unwrap();
    }
}

