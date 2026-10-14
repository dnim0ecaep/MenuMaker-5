//! Address Book: vCard (.vcf) contacts with a DeskMate-style screen, modeled
//! on vcard_tui (https://github.com/kenianbei/vcard_tui) — a contact list,
//! per-property panels (name, birthday, emails, phones, addresses, websites,
//! notes), add/edit/delete of contacts and properties, sort, and .vcf
//! import/export.

use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::Frame;

use crate::boxes::expand_tilde;
use crate::dmui::{
    confirm_click, dm_box, is_left_click, key, put_line, render_confirm, screen_chrome, shortcut_click, wheel_key,
    FieldForm, FormField, FormResult, Hits, Palette,
};

const EMAIL_TYPES: &[&str] = &["home", "work", "other"];
const TEL_TYPES: &[&str] = &["cell", "home", "work", "fax", "other"];
const ADR_TYPES: &[&str] = &["home", "work", "other"];

/// A property value with a TYPE (home, work, cell, ...).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Typed {
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Address {
    pub kind: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub code: String,
    pub country: String,
}

impl Address {
    fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.street.is_empty() {
            out.push(self.street.clone());
        }
        let locality = [self.city.as_str(), self.region.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let locality = [locality.as_str(), self.code.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if !locality.is_empty() {
            out.push(locality);
        }
        if !self.country.is_empty() {
            out.push(self.country.clone());
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contact {
    pub full_name: String,
    pub given: String,
    pub family: String,
    pub bday: String,
    pub emails: Vec<Typed>,
    pub tels: Vec<Typed>,
    pub adrs: Vec<Address>,
    pub urls: Vec<String>,
    pub notes: Vec<String>,
    /// Properties this app does not edit (PHOTO, ORG, ...), kept verbatim.
    pub extra: Vec<String>,
}

impl Contact {
    pub fn display_name(&self) -> String {
        if !self.full_name.trim().is_empty() {
            return self.full_name.clone();
        }
        let joined = format!("{} {}", self.given, self.family).trim().to_string();
        if joined.is_empty() {
            "(no name)".into()
        } else {
            joined
        }
    }
}

// ---------------------------------------------------------------------------
// vCard reading / writing
// ---------------------------------------------------------------------------

fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

/// Split on `sep` where it is not backslash-escaped.
fn split_unescaped(value: &str, sep: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            parts.last_mut().unwrap().push('\\');
            parts.last_mut().unwrap().push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == sep {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    parts
}

/// TYPE from parameters: `TYPE=home,pref`, `type=WORK`, or vCard 2.1 bare `HOME`.
fn type_param(params: &[String]) -> String {
    for param in params {
        let (key, value) = match param.split_once('=') {
            Some((k, v)) => (k.to_ascii_uppercase(), v.to_string()),
            None => ("TYPE".to_string(), param.clone()),
        };
        if key != "TYPE" {
            continue;
        }
        for kind in value.trim_matches('"').split(',') {
            let kind = kind.trim().to_ascii_lowercase();
            if !kind.is_empty() && kind != "pref" && kind != "voice" && kind != "internet" {
                return kind;
            }
        }
    }
    String::new()
}

/// Parse every BEGIN:VCARD ... END:VCARD block in `text`.
pub fn parse_vcards(text: &str) -> Vec<Contact> {
    // Unfold continuation lines (start with a space or tab).
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        if (raw.starts_with(' ') || raw.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&raw[1..]);
        } else {
            lines.push(raw.trim_end_matches('\r').to_string());
        }
    }

    let mut contacts = Vec::new();
    let mut current: Option<Contact> = None;
    for line in lines {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let mut params: Vec<String> = head.split(';').map(|s| s.to_string()).collect();
        let name = params.remove(0);
        // Drop an "item1." style group prefix.
        let name = name.rsplit('.').next().unwrap_or(&name).to_ascii_uppercase();
        match name.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VCARD") => current = Some(Contact::default()),
            "END" if value.eq_ignore_ascii_case("VCARD") => {
                if let Some(contact) = current.take() {
                    contacts.push(contact);
                }
            }
            _ => {
                let Some(contact) = current.as_mut() else {
                    continue;
                };
                let kind = type_param(&params);
                match name.as_str() {
                    "VERSION" | "PRODID" => {}
                    "FN" => contact.full_name = unescape(value),
                    "N" => {
                        let parts = split_unescaped(value, ';');
                        contact.family = parts.first().map(|s| unescape(s)).unwrap_or_default();
                        contact.given = parts.get(1).map(|s| unescape(s)).unwrap_or_default();
                    }
                    "BDAY" => contact.bday = unescape(value),
                    "EMAIL" => contact.emails.push(Typed { kind, value: unescape(value) }),
                    "TEL" => contact.tels.push(Typed {
                        kind,
                        value: unescape(value.trim_start_matches("tel:")),
                    }),
                    "ADR" => {
                        let parts: Vec<String> =
                            split_unescaped(value, ';').iter().map(|s| unescape(s)).collect();
                        let get = |i: usize| parts.get(i).cloned().unwrap_or_default();
                        contact.adrs.push(Address {
                            kind,
                            street: get(2),
                            city: get(3),
                            region: get(4),
                            code: get(5),
                            country: get(6),
                        });
                    }
                    "URL" => contact.urls.push(unescape(value)),
                    "NOTE" => contact.notes.push(unescape(value)),
                    _ => contact.extra.push(line.clone()),
                }
            }
        }
    }
    contacts
}

/// Fold a content line to 75 characters per RFC 6350.
fn fold(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= 75 {
        return format!("{line}\r\n");
    }
    let mut out = String::new();
    let mut start = 0;
    let mut width = 75;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        if start > 0 {
            out.push(' ');
        }
        out.extend(&chars[start..end]);
        out.push_str("\r\n");
        start = end;
        width = 74;
    }
    out
}

fn type_suffix(kind: &str) -> String {
    if kind.is_empty() {
        String::new()
    } else {
        format!(";TYPE={kind}")
    }
}

/// Serialize a contact as a vCard 3.0 block.
pub fn to_vcard(contact: &Contact) -> String {
    let mut out = String::from("BEGIN:VCARD\r\nVERSION:3.0\r\n");
    out += &fold(&format!("FN:{}", escape(&contact.display_name())));
    out += &fold(&format!("N:{};{};;;", escape(&contact.family), escape(&contact.given)));
    if !contact.bday.is_empty() {
        out += &fold(&format!("BDAY:{}", escape(&contact.bday)));
    }
    for email in &contact.emails {
        out += &fold(&format!("EMAIL{}:{}", type_suffix(&email.kind), escape(&email.value)));
    }
    for tel in &contact.tels {
        out += &fold(&format!("TEL{}:{}", type_suffix(&tel.kind), escape(&tel.value)));
    }
    for adr in &contact.adrs {
        out += &fold(&format!(
            "ADR{}:;;{};{};{};{};{}",
            type_suffix(&adr.kind),
            escape(&adr.street),
            escape(&adr.city),
            escape(&adr.region),
            escape(&adr.code),
            escape(&adr.country)
        ));
    }
    for url in &contact.urls {
        out += &fold(&format!("URL:{}", escape(url)));
    }
    for note in &contact.notes {
        out += &fold(&format!("NOTE:{}", escape(note)));
    }
    for extra in &contact.extra {
        out += &fold(extra);
    }
    out += "END:VCARD\r\n";
    out
}

pub fn load_contacts(path: &Path) -> Vec<Contact> {
    fs::read_to_string(path)
        .map(|text| parse_vcards(&text))
        .unwrap_or_default()
}

pub fn save_contacts(path: &Path, contacts: &[Contact]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text: String = contacts.iter().map(to_vcard).collect();
    fs::write(path, text)
}

/// Contact names for the desktop box preview.
pub fn preview_names(path: &Path) -> Vec<String> {
    load_contacts(path).iter().map(|c| c.display_name()).collect()
}

/// Each contact's first phone number (blank when it has none), in the same
/// order as `preview_names`.
pub fn preview_phones(path: &Path) -> Vec<String> {
    load_contacts(path)
        .iter()
        .map(|c| c.tels.first().map(|t| t.value.trim().to_string()).unwrap_or_default())
        .collect()
}

// ---------------------------------------------------------------------------
// The Address Book screen
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Name,
    Birthday,
    Emails,
    Phones,
    Addresses,
    Websites,
    Notes,
}

const SECTIONS: [Section; 7] = [
    Section::Name,
    Section::Birthday,
    Section::Emails,
    Section::Phones,
    Section::Addresses,
    Section::Websites,
    Section::Notes,
];

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Name => "FULL NAME",
            Section::Birthday => "BIRTHDAY",
            Section::Emails => "EMAILS",
            Section::Phones => "PHONE NUMBERS",
            Section::Addresses => "ADDRESSES",
            Section::Websites => "WEBSITES",
            Section::Notes => "NOTES",
        }
    }
}

/// What a mouse click can land on.
#[derive(Clone, Copy)]
enum ContactHit {
    ListBox,
    ListRow(usize),
    SectionBox(Section),
    SectionRow(Section, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Section(Section),
}

enum FormPurpose {
    NewContact,
    Name,
    Birthday,
    Email(Option<usize>),
    Phone(Option<usize>),
    Address(Option<usize>),
    Website(Option<usize>),
    Note(Option<usize>),
    Import,
    Export,
}

enum ConfirmPurpose {
    DeleteContact,
    DeleteValue(Section, usize),
}

enum Popup {
    Form(FieldForm, FormPurpose),
    Confirm(String, ConfirmPurpose),
}

pub enum AppResult {
    Continue,
    Close,
}

pub struct ContactsApp {
    pub box_name: String,
    path: PathBuf,
    contacts: Vec<Contact>,
    selected: usize,
    focus: Focus,
    /// Selected value inside the focused multi-value section.
    value_index: usize,
    popup: Option<Popup>,
    status: String,
    hits: Hits<ContactHit>,
}

impl ContactsApp {
    pub fn open(box_name: &str, path: PathBuf, select: Option<usize>) -> Self {
        let contacts = load_contacts(&path);
        let selected = select.unwrap_or(0).min(contacts.len().saturating_sub(1));
        Self {
            box_name: box_name.to_string(),
            status: format!("{} contacts in {}", contacts.len(), path.display()),
            path,
            contacts,
            selected,
            focus: Focus::List,
            value_index: 0,
            popup: None,
            hits: Hits::default(),
        }
    }

    fn save(&mut self) {
        if let Err(err) = save_contacts(&self.path, &self.contacts) {
            self.status = format!("Could not save {}: {err}", self.path.display());
        }
    }

    fn current(&self) -> Option<&Contact> {
        self.contacts.get(self.selected)
    }

    fn value_count(&self, section: Section) -> usize {
        let Some(c) = self.current() else { return 0 };
        match section {
            Section::Name => 1,
            Section::Birthday => usize::from(!c.bday.is_empty()),
            Section::Emails => c.emails.len(),
            Section::Phones => c.tels.len(),
            Section::Addresses => c.adrs.len(),
            Section::Websites => c.urls.len(),
            Section::Notes => c.notes.len(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        if let Some(popup) = self.popup.take() {
            self.handle_popup_key(popup, key);
            return AppResult::Continue;
        }
        // Commands available everywhere (the vcard_tui command bar).
        match key.code {
            KeyCode::Char('i') => {
                self.popup = Some(Popup::Form(
                    FieldForm::new("Import Contacts", vec![FormField::text("vCard file", "~/")]),
                    FormPurpose::Import,
                ));
                return AppResult::Continue;
            }
            KeyCode::Char('x') => {
                if self.contacts.is_empty() {
                    self.status = "No contacts to export".into();
                } else {
                    self.popup = Some(Popup::Form(
                        FieldForm::new(
                            "Export Contacts",
                            vec![
                                FormField::text("File", "~/contacts.vcf"),
                                FormField::choice("Export", "All contacts", &["All contacts", "Selected contact"]),
                            ],
                        ),
                        FormPurpose::Export,
                    ));
                }
                return AppResult::Continue;
            }
            KeyCode::Char('s') => {
                let name = self.current().map(|c| c.display_name());
                self.contacts
                    .sort_by_key(|c| c.display_name().to_lowercase());
                if let Some(name) = name {
                    self.selected = self
                        .contacts
                        .iter()
                        .position(|c| c.display_name() == name)
                        .unwrap_or(0);
                }
                self.save();
                self.status = "Contacts sorted by name".into();
                return AppResult::Continue;
            }
            _ => {}
        }
        match self.focus {
            Focus::List => self.handle_list_key(key),
            Focus::Section(section) => self.handle_section_key(section, key),
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> AppResult {
        let len = self.contacts.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return AppResult::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(len.saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = len.saturating_sub(1),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(len.saturating_sub(1)),
            KeyCode::Right | KeyCode::Tab | KeyCode::Enter if len > 0 => {
                self.focus = Focus::Section(Section::Name);
                self.value_index = 0;
            }
            KeyCode::Char('a') => {
                self.popup = Some(Popup::Form(
                    FieldForm::new(
                        "Add Contact",
                        vec![
                            FormField::text("First name", ""),
                            FormField::text("Last name", ""),
                            FormField::text("Phone", ""),
                            FormField::text("Email", ""),
                        ],
                    ),
                    FormPurpose::NewContact,
                ));
            }
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                let name = self.current().map(|c| c.display_name()).unwrap_or_default();
                self.popup = Some(Popup::Confirm(
                    format!("Delete contact \"{name}\"?"),
                    ConfirmPurpose::DeleteContact,
                ));
            }
            _ => {}
        }
        AppResult::Continue
    }

    fn handle_section_key(&mut self, section: Section, key: KeyEvent) -> AppResult {
        let pos = SECTIONS.iter().position(|s| *s == section).unwrap_or(0);
        let count = self.value_count(section);
        match key.code {
            KeyCode::Esc => self.focus = Focus::List,
            KeyCode::Tab | KeyCode::Right => {
                self.focus = Focus::Section(SECTIONS[(pos + 1) % SECTIONS.len()]);
                self.value_index = 0;
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.focus = if pos == 0 {
                    Focus::List
                } else {
                    Focus::Section(SECTIONS[pos - 1])
                };
                self.value_index = 0;
            }
            KeyCode::Up => self.value_index = self.value_index.saturating_sub(1),
            KeyCode::Down => self.value_index = (self.value_index + 1).min(count.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('e') => {
                if count == 0 {
                    self.open_value_form(section, None);
                } else {
                    self.open_value_form(section, Some(self.value_index));
                }
            }
            KeyCode::Char('a') => self.open_value_form(section, None),
            KeyCode::Char('d') | KeyCode::Delete if count > 0 => {
                if section == Section::Name {
                    self.status = "A contact always has a name; delete the contact from the list".into();
                } else {
                    self.popup = Some(Popup::Confirm(
                        format!("Delete this {}?", section.title().to_lowercase().trim_end_matches('s')),
                        ConfirmPurpose::DeleteValue(section, self.value_index),
                    ));
                }
            }
            _ => {}
        }
        AppResult::Continue
    }

    fn open_value_form(&mut self, section: Section, index: Option<usize>) {
        let Some(contact) = self.current().cloned() else { return };
        let (title, fields, purpose) = match section {
            Section::Name => (
                "Edit Name",
                vec![
                    FormField::text("First name", &contact.given),
                    FormField::text("Last name", &contact.family),
                    FormField::text("Full name", &contact.full_name),
                ],
                FormPurpose::Name,
            ),
            Section::Birthday => (
                "Birthday",
                vec![FormField::text("Date (YYYY-MM-DD)", &contact.bday)],
                FormPurpose::Birthday,
            ),
            Section::Emails => {
                let current = index.and_then(|i| contact.emails.get(i)).cloned().unwrap_or_default();
                (
                    if index.is_some() { "Edit Email" } else { "Add Email" },
                    vec![
                        FormField::choice("Type", &current.kind, EMAIL_TYPES),
                        FormField::text("Email", &current.value),
                    ],
                    FormPurpose::Email(index),
                )
            }
            Section::Phones => {
                let current = index.and_then(|i| contact.tels.get(i)).cloned().unwrap_or_default();
                (
                    if index.is_some() { "Edit Phone" } else { "Add Phone" },
                    vec![
                        FormField::choice("Type", &current.kind, TEL_TYPES),
                        FormField::text("Number", &current.value),
                    ],
                    FormPurpose::Phone(index),
                )
            }
            Section::Addresses => {
                let current = index.and_then(|i| contact.adrs.get(i)).cloned().unwrap_or_default();
                (
                    if index.is_some() { "Edit Address" } else { "Add Address" },
                    vec![
                        FormField::choice("Type", &current.kind, ADR_TYPES),
                        FormField::text("Street", &current.street),
                        FormField::text("City", &current.city),
                        FormField::text("State/Region", &current.region),
                        FormField::text("Postal code", &current.code),
                        FormField::text("Country", &current.country),
                    ],
                    FormPurpose::Address(index),
                )
            }
            Section::Websites => {
                let current = index.and_then(|i| contact.urls.get(i)).cloned().unwrap_or_default();
                (
                    if index.is_some() { "Edit Website" } else { "Add Website" },
                    vec![FormField::text("URL", &current)],
                    FormPurpose::Website(index),
                )
            }
            Section::Notes => {
                let current = index.and_then(|i| contact.notes.get(i)).cloned().unwrap_or_default();
                (
                    if index.is_some() { "Edit Note" } else { "Add Note" },
                    vec![FormField::text("Note", &current)],
                    FormPurpose::Note(index),
                )
            }
        };
        self.popup = Some(Popup::Form(FieldForm::new(title, fields), purpose));
    }

    fn handle_popup_key(&mut self, popup: Popup, key: KeyEvent) {
        match popup {
            Popup::Confirm(message, purpose) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm(purpose),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.popup = Some(Popup::Confirm(message, purpose)),
            },
            Popup::Form(mut form, purpose) => match form.handle_key(key) {
                FormResult::Continue => self.popup = Some(Popup::Form(form, purpose)),
                FormResult::Cancel => {}
                FormResult::Submit => {
                    if let Err(err) = self.submit(&form, &purpose) {
                        form.error = Some(err);
                        self.popup = Some(Popup::Form(form, purpose));
                    }
                }
            },
        }
    }

    fn confirm(&mut self, purpose: ConfirmPurpose) {
        match purpose {
            ConfirmPurpose::DeleteContact => {
                if self.selected < self.contacts.len() {
                    let removed = self.contacts.remove(self.selected);
                    self.selected = self.selected.min(self.contacts.len().saturating_sub(1));
                    self.status = format!("Deleted {}", removed.display_name());
                }
            }
            ConfirmPurpose::DeleteValue(section, index) => {
                let Some(contact) = self.contacts.get_mut(self.selected) else { return };
                match section {
                    Section::Birthday => contact.bday.clear(),
                    Section::Emails if index < contact.emails.len() => {
                        contact.emails.remove(index);
                    }
                    Section::Phones if index < contact.tels.len() => {
                        contact.tels.remove(index);
                    }
                    Section::Addresses if index < contact.adrs.len() => {
                        contact.adrs.remove(index);
                    }
                    Section::Websites if index < contact.urls.len() => {
                        contact.urls.remove(index);
                    }
                    Section::Notes if index < contact.notes.len() => {
                        contact.notes.remove(index);
                    }
                    _ => {}
                }
                self.value_index = self.value_index.saturating_sub(1);
                self.status = "Deleted".into();
            }
        }
        self.save();
    }

    fn submit(&mut self, form: &FieldForm, purpose: &FormPurpose) -> Result<(), String> {
        fn put<T>(list: &mut Vec<T>, index: Option<usize>, value: T) {
            match index {
                Some(i) if i < list.len() => list[i] = value,
                _ => list.push(value),
            }
        }
        match purpose {
            FormPurpose::Import => {
                let path = expand_tilde(form.value(0));
                let text = fs::read_to_string(&path).map_err(|e| format!("Cannot read file: {e}"))?;
                let imported = parse_vcards(&text);
                if imported.is_empty() {
                    return Err("No vCards found in that file".into());
                }
                let count = imported.len();
                self.contacts.extend(imported);
                self.save();
                self.status = format!("Imported {count} contacts from {}", path.display());
                return Ok(());
            }
            FormPurpose::Export => {
                let path = expand_tilde(form.value(0));
                let selection: Vec<Contact> = if form.value(1) == "Selected contact" {
                    self.current().cloned().into_iter().collect()
                } else {
                    self.contacts.clone()
                };
                save_contacts(&path, &selection).map_err(|e| format!("Cannot write file: {e}"))?;
                self.status = format!("Exported {} contacts to {}", selection.len(), path.display());
                return Ok(());
            }
            FormPurpose::NewContact => {
                let (given, family) = (form.value(0), form.value(1));
                if given.is_empty() && family.is_empty() {
                    return Err("Enter a first or last name".into());
                }
                let mut contact = Contact {
                    given: given.to_string(),
                    family: family.to_string(),
                    full_name: format!("{given} {family}").trim().to_string(),
                    ..Contact::default()
                };
                if !form.value(2).is_empty() {
                    contact.tels.push(Typed { kind: "cell".into(), value: form.value(2).into() });
                }
                if !form.value(3).is_empty() {
                    contact.emails.push(Typed { kind: "home".into(), value: form.value(3).into() });
                }
                self.status = format!("Added {}", contact.display_name());
                self.contacts.push(contact);
                self.selected = self.contacts.len() - 1;
                self.save();
                return Ok(());
            }
            _ => {}
        }

        let Some(contact) = self.contacts.get_mut(self.selected) else {
            return Err("No contact selected".into());
        };
        match purpose {
            FormPurpose::Name => {
                let (given, family, full) = (form.value(0), form.value(1), form.value(2));
                if given.is_empty() && family.is_empty() && full.is_empty() {
                    return Err("A contact needs a name".into());
                }
                let name_changed = given != contact.given || family != contact.family;
                contact.given = given.into();
                contact.family = family.into();
                // Keep the full name in step with first/last unless it was typed.
                contact.full_name = if full.is_empty() || (name_changed && full == contact.full_name) {
                    format!("{given} {family}").trim().to_string()
                } else {
                    full.into()
                };
            }
            FormPurpose::Birthday => {
                let value = form.value(0);
                if !value.is_empty() && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
                    return Err("Use the format YYYY-MM-DD".into());
                }
                contact.bday = value.into();
            }
            FormPurpose::Email(index) => {
                if form.value(1).is_empty() {
                    return Err("Enter an email address".into());
                }
                put(&mut contact.emails, *index, Typed { kind: form.value(0).into(), value: form.value(1).into() });
            }
            FormPurpose::Phone(index) => {
                if form.value(1).is_empty() {
                    return Err("Enter a phone number".into());
                }
                put(&mut contact.tels, *index, Typed { kind: form.value(0).into(), value: form.value(1).into() });
            }
            FormPurpose::Address(index) => {
                let adr = Address {
                    kind: form.value(0).into(),
                    street: form.value(1).into(),
                    city: form.value(2).into(),
                    region: form.value(3).into(),
                    code: form.value(4).into(),
                    country: form.value(5).into(),
                };
                if adr.lines().is_empty() {
                    return Err("Enter at least one address field".into());
                }
                put(&mut contact.adrs, *index, adr);
            }
            FormPurpose::Website(index) => {
                if form.value(0).is_empty() {
                    return Err("Enter a URL".into());
                }
                put(&mut contact.urls, *index, form.value(0).to_string());
            }
            FormPurpose::Note(index) => {
                if form.value(0).is_empty() {
                    return Err("Enter a note".into());
                }
                put(&mut contact.notes, *index, form.value(0).to_string());
            }
            FormPurpose::NewContact | FormPurpose::Import | FormPurpose::Export => {}
        }
        self.status = format!("Saved {}", contact.display_name());
        self.save();
        Ok(())
    }

    // ----- drawing -----

    /// Mouse: click the shortcut bar, a contact, or a value (click a selected
    /// one again to open / edit it); the wheel moves the selection; dialogs
    /// get clickable fields and Save / Cancel / Yes / No buttons.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> AppResult {
        match &mut self.popup {
            Some(Popup::Form(form, _)) => {
                return match form.handle_mouse(mouse, area) {
                    FormResult::Submit => self.handle_key(key(KeyCode::Enter)),
                    FormResult::Cancel => self.handle_key(key(KeyCode::Esc)),
                    FormResult::Continue => AppResult::Continue,
                };
            }
            Some(Popup::Confirm(message, _)) => {
                if let Some(k) = confirm_click(area, &message.clone(), mouse) {
                    return self.handle_key(k);
                }
                return AppResult::Continue;
            }
            None => {}
        }
        if let Some(k) = wheel_key(mouse).or_else(|| shortcut_click(area, self.shortcuts(), mouse)) {
            return self.handle_key(k);
        }
        if !is_left_click(mouse) {
            return AppResult::Continue;
        }
        match self.hits.at(mouse.column, mouse.row) {
            Some(ContactHit::ListBox) => self.focus = Focus::List,
            Some(ContactHit::ListRow(idx)) => {
                if self.focus == Focus::List && idx == self.selected {
                    return self.handle_key(key(KeyCode::Enter));
                }
                self.focus = Focus::List;
                self.selected = idx;
            }
            Some(ContactHit::SectionBox(section)) if self.current().is_some() => {
                if self.focus != Focus::Section(section) {
                    self.focus = Focus::Section(section);
                    self.value_index = 0;
                }
            }
            Some(ContactHit::SectionRow(section, value)) => {
                if self.focus == Focus::Section(section) && value == self.value_index {
                    return self.handle_key(key(KeyCode::Enter));
                }
                self.focus = Focus::Section(section);
                self.value_index = value;
            }
            _ => {}
        }
        AppResult::Continue
    }

    fn shortcuts(&self) -> &'static [(&'static str, &'static str)] {
        match self.focus {
            Focus::List => &[
                ("Esc", "Back"),
                ("↑/↓", "Select"),
                ("→", "Details"),
                ("a", "Add Contact"),
                ("d", "Delete"),
                ("s", "Sort"),
                ("i", "Import"),
                ("x", "Export"),
            ],
            Focus::Section(_) => &[
                ("Esc", "List"),
                ("Tab", "Next Box"),
                ("↑/↓", "Select"),
                ("Enter", "Edit"),
                ("a", "Add"),
                ("d", "Delete"),
            ],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, title: &str, pal: &Palette) {
        self.hits.clear();
        let shortcuts = self.shortcuts();
        let desk = screen_chrome(
            frame,
            area,
            &format!("{title} - {}", self.box_name),
            shortcuts,
            &self.status,
            pal,
        );
        if desk.width < 20 || desk.height < 6 {
            return;
        }
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Length(1), Constraint::Min(10)])
            .split(desk);
        self.render_list(frame, columns[0], pal);
        self.render_details(frame, columns[2], pal);

        match &self.popup {
            Some(Popup::Form(form, _)) => form.render(frame, area, pal),
            Some(Popup::Confirm(message, _)) => render_confirm(frame, area, message, pal),
            None => {}
        }
    }

    fn render_list(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let inner = dm_box(
            frame,
            area,
            &format!("CONTACTS ({})", self.contacts.len()),
            pal,
            pal.fill(),
            self.focus == Focus::List,
        );
        self.hits.push(area, ContactHit::ListBox);
        if self.contacts.is_empty() {
            put_line(frame, inner, 0, " (none - press a to add)", pal.fill().add_modifier(Modifier::ITALIC));
            return;
        }
        let height = inner.height as usize;
        let top = if height > 0 && self.selected >= height { self.selected + 1 - height } else { 0 };
        for (row, (idx, contact)) in self.contacts.iter().enumerate().skip(top).take(height).enumerate() {
            let style = if idx == self.selected { pal.selected() } else { pal.fill() };
            self.hits.push(
                Rect { x: inner.x, y: inner.y + row as u16, width: inner.width, height: 1 },
                ContactHit::ListRow(idx),
            );
            put_line(
                frame,
                inner,
                row as u16,
                &format!(" {}. {}", idx + 1, contact.display_name()),
                style,
            );
        }
    }

    fn section_lines(&self, section: Section) -> Vec<(usize, String)> {
        let Some(c) = self.current() else { return Vec::new() };
        let typed = |list: &[Typed]| -> Vec<(usize, String)> {
            list.iter()
                .enumerate()
                .map(|(i, t)| {
                    if t.kind.is_empty() {
                        (i, t.value.clone())
                    } else {
                        (i, format!("{:<5} {}", t.kind, t.value))
                    }
                })
                .collect()
        };
        match section {
            Section::Name => vec![(0, c.display_name())],
            Section::Birthday => {
                if c.bday.is_empty() {
                    Vec::new()
                } else {
                    vec![(0, c.bday.clone())]
                }
            }
            Section::Emails => typed(&c.emails),
            Section::Phones => typed(&c.tels),
            Section::Addresses => c
                .adrs
                .iter()
                .enumerate()
                .flat_map(|(i, a)| {
                    let mut lines = a.lines();
                    if !a.kind.is_empty() {
                        if let Some(first) = lines.first_mut() {
                            *first = format!("{:<5} {first}", a.kind);
                        }
                    }
                    lines.into_iter().map(move |l| (i, l))
                })
                .collect(),
            Section::Websites => c.urls.iter().cloned().enumerate().collect(),
            Section::Notes => c
                .notes
                .iter()
                .enumerate()
                .flat_map(|(i, n)| n.lines().map(move |l| (i, l.to_string())).collect::<Vec<_>>())
                .collect(),
        }
    }

    fn render_section(&self, frame: &mut Frame, area: Rect, section: Section, pal: &Palette) {
        let focused = self.focus == Focus::Section(section);
        let inner = dm_box(frame, area, section.title(), pal, pal.fill(), focused);
        self.hits.push(area, ContactHit::SectionBox(section));
        let lines = self.section_lines(section);
        if lines.is_empty() {
            let hint = if self.current().is_some() && focused { " (empty - press a to add)" } else { "" };
            put_line(frame, inner, 0, hint, pal.fill().add_modifier(Modifier::ITALIC));
            return;
        }
        for (row, (value, text)) in lines.iter().take(inner.height as usize).enumerate() {
            let style = if focused && *value == self.value_index { pal.selected() } else { pal.fill() };
            self.hits.push(
                Rect { x: inner.x, y: inner.y + row as u16, width: inner.width, height: 1 },
                ContactHit::SectionRow(section, *value),
            );
            put_line(frame, inner, row as u16, &format!(" {text}"), style);
        }
    }

    fn render_details(&self, frame: &mut Frame, area: Rect, pal: &Palette) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Ratio(1, 3),
                Constraint::Length(1),
                Constraint::Ratio(1, 3),
                Constraint::Length(1),
                Constraint::Min(3),
            ])
            .split(area);
        let pair = |r: Rect| {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Ratio(1, 2), Constraint::Length(1), Constraint::Ratio(1, 2)])
                .split(r)
        };
        let top = pair(rows[0]);
        self.render_section(frame, top[0], Section::Name, pal);
        self.render_section(frame, top[2], Section::Birthday, pal);
        let middle = pair(rows[2]);
        self.render_section(frame, middle[0], Section::Emails, pal);
        self.render_section(frame, middle[2], Section::Phones, pal);
        let lower = pair(rows[4]);
        self.render_section(frame, lower[0], Section::Addresses, pal);
        self.render_section(frame, lower[2], Section::Websites, pal);
        self.render_section(frame, rows[6], Section::Notes, pal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:John Doe\r\nN:Doe;John;;;\r\nBDAY:2000-01-01\r\n\
EMAIL;TYPE=home;TYPE=pref:user@example.com\r\nitem1.EMAIL;type=WORK:acme@example.com\r\n\
TEL;TYPE=CELL,VOICE:+1 (555) 555-5555\r\nADR;TYPE=home:;;1600 Pennsylvania Avenue NW;Washington;DC;20500;United States\r\n\
NOTE:Lorem ipsum\\, dolor\\nsecond line\r\nORG:Acme\r\nPHOTO;VALUE=uri:http://example.com/a\r\n b.jpg\r\nEND:VCARD\r\n";

    #[test]
    fn parses_vcard() {
        let contacts = parse_vcards(SAMPLE);
        assert_eq!(contacts.len(), 1);
        let c = &contacts[0];
        assert_eq!(c.display_name(), "John Doe");
        assert_eq!((c.given.as_str(), c.family.as_str()), ("John", "Doe"));
        assert_eq!(c.emails[0], Typed { kind: "home".into(), value: "user@example.com".into() });
        assert_eq!(c.emails[1].kind, "work");
        assert_eq!(c.tels[0].kind, "cell");
        assert_eq!(c.adrs[0].city, "Washington");
        assert_eq!(c.notes[0], "Lorem ipsum, dolor\nsecond line");
        assert_eq!(c.extra, vec!["ORG:Acme", "PHOTO;VALUE=uri:http://example.com/ab.jpg"]);
    }

    #[test]
    fn round_trips() {
        let original = parse_vcards(SAMPLE);
        let written: String = original.iter().map(to_vcard).collect();
        assert_eq!(parse_vcards(&written), original);
    }

    #[test]
    fn folds_long_lines() {
        let folded = fold(&format!("NOTE:{}", "x".repeat(200)));
        assert!(folded.lines().all(|l| l.chars().count() <= 75));
        assert_eq!(parse_vcards(&format!("BEGIN:VCARD\r\n{folded}END:VCARD\r\n"))[0].notes[0].len(), 200);
    }
}
