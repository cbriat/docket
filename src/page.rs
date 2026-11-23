//! Browser-equivalent model of a PaperCept page and its form.
//!
//! PaperCept buttons are links `javascript:command('Name')`. The script ticks
//! a hidden `Command` checkbox (value `Name` on journal sites, `Name.x` on the
//! conference site) and submits `document.forms[0]`. The server checks the
//! hidden `Fields`/`Hidden`/`Hash`/`RldCtl` inputs, so a request must carry the
//! latest page's form exactly as a browser would serialize it.

use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use std::sync::LazyLock;

static CMD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bcommand\s*\(\s*['"]([^'"]+)['"]"#).unwrap());

#[derive(Clone, Debug)]
pub struct SelectOption {
    pub value: String,
    pub selected: bool,
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Text,
    Hidden,
    Password,
    Checkbox,
    Radio,
    Submit,
    Button,
    Image,
    Reset,
    File,
    Select,
    Textarea,
}

#[derive(Clone, Debug)]
pub struct Control {
    pub name: String,
    pub kind: Kind,
    pub value: String,
    pub checked: bool,
    pub disabled: bool,
    pub multiple: bool,
    pub options: Vec<SelectOption>,
}

pub enum EntryValue {
    Text(String),
    File { filename: String, bytes: Vec<u8>, mime: String },
}

pub struct Entry {
    pub name: String,
    pub value: EntryValue,
}

#[derive(Clone, Debug)]
pub struct Form {
    pub action: String,
    pub method: String,
    pub multipart: bool,
    pub controls: Vec<Control>,
    /// Command names suffix: "" on journal sites, ".x" on the conference site.
    pub cmd_suffix: &'static str,
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

impl Form {
    pub fn get(&self, name: &str) -> Option<&Control> {
        self.controls.iter().find(|c| c.name == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.get(name).map(|c| c.value.as_str())
    }

    /// Names the server accepts from this page (hidden `Fields` input).
    pub fn allowed(&self) -> Vec<&str> {
        self.value("Fields").map(|f| f.split(',').collect()).unwrap_or_default()
    }

    pub fn set(&mut self, name: &str, value: &str) -> bool {
        match self.controls.iter_mut().find(|c| c.name == name) {
            Some(c) if matches!(c.kind, Kind::Text | Kind::Hidden | Kind::Password | Kind::Textarea) => {
                c.value = value.to_string();
                true
            }
            _ => false,
        }
    }

    pub fn set_checked(&mut self, name: &str, on: bool) -> bool {
        match self.controls.iter_mut().find(|c| c.name == name && c.kind == Kind::Checkbox) {
            Some(c) => {
                c.checked = on;
                true
            }
            None => false,
        }
    }

    /// Choose the option of a single-choice select.
    pub fn select(&mut self, name: &str, value: &str) -> bool {
        match self.controls.iter_mut().find(|c| c.name == name && c.kind == Kind::Select) {
            Some(c) if c.options.iter().any(|o| o.value == value && !o.disabled) => {
                for o in c.options.iter_mut() {
                    o.selected = o.value == value;
                }
                true
            }
            _ => false,
        }
    }

    pub fn attach(&mut self, field: &str, filename: &str, bytes: Vec<u8>) {
        self.attachments.push((field.to_string(), filename.to_string(), bytes));
    }

    /// The entry list a browser would build when `command(cmd)` submits the form.
    pub fn entries(&self, command: Option<&str>) -> Vec<Entry> {
        self.build_entries(command, None)
    }

    /// The entry list when the user clicks the submit button named `button`.
    pub fn entries_clicking(&self, button: &str) -> Vec<Entry> {
        self.build_entries(None, Some(button))
    }

    fn build_entries(&self, command: Option<&str>, submitter: Option<&str>) -> Vec<Entry> {
        let mut submitted = false;
        let mut out = Vec::new();
        let mut command_done = false;
        for c in &self.controls {
            if c.name.is_empty() || c.disabled {
                continue;
            }
            if c.name == "Command" && c.kind == Kind::Checkbox {
                if let Some(cmd) = command {
                    out.push(text_entry("Command", &format!("{cmd}{}", self.cmd_suffix)));
                    command_done = true;
                }
                continue;
            }
            match c.kind {
                Kind::Checkbox | Kind::Radio => {
                    if c.checked {
                        out.push(text_entry(&c.name, &c.value));
                    }
                }
                Kind::Submit if !submitted && submitter == Some(c.name.as_str()) => {
                    submitted = true;
                    out.push(text_entry(&c.name, &c.value));
                }
                Kind::Submit | Kind::Button | Kind::Image | Kind::Reset => {}
                Kind::File => {
                    if self.multipart {
                        let (filename, bytes) = self
                            .attachments
                            .iter()
                            .find(|(f, _, _)| *f == c.name)
                            .map(|(_, n, b)| (n.clone(), b.clone()))
                            .unwrap_or_default();
                        out.push(Entry {
                            name: c.name.clone(),
                            value: EntryValue::File { filename, bytes, mime: "application/octet-stream".into() },
                        });
                    } else {
                        out.push(text_entry(&c.name, ""));
                    }
                }
                Kind::Select => {
                    let mut chosen: Vec<&SelectOption> = c.options.iter().filter(|o| o.selected).collect();
                    if chosen.is_empty() && !c.multiple {
                        chosen.extend(c.options.iter().find(|o| !o.disabled));
                    }
                    for o in chosen {
                        out.push(text_entry(&c.name, &o.value));
                    }
                }
                Kind::Textarea => out.push(text_entry(&c.name, &to_crlf(&c.value))),
                _ => out.push(text_entry(&c.name, &c.value)),
            }
        }
        if let (Some(cmd), false) = (command, command_done) {
            out.push(text_entry("Command", &format!("{cmd}{}", self.cmd_suffix)));
        }
        out
    }
}

fn text_entry(name: &str, value: &str) -> Entry {
    Entry { name: name.to_string(), value: EntryValue::Text(value.to_string()) }
}

fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n")
}

pub struct Page {
    pub url: String,
    pub html: String,
    pub forms: Vec<Form>,
    pub title: String,
}

impl Page {
    pub fn parse(url: &str, html: String) -> Page {
        let doc = Html::parse_document(&html);
        let title = doc
            .select(&sel("title"))
            .next()
            .map(|t| norm(&t.text().collect::<String>()))
            .unwrap_or_default();
        let suffix = if html.contains("Command.value = arg + '.x'") || (url.contains("/conferences/") && !html.contains("external.v2.js")) {
            ".x"
        } else {
            ""
        };
        let forms = build_forms(&doc, url, suffix);
        Page { url: url.to_string(), html, forms, title }
    }

    pub fn doc(&self) -> Html {
        Html::parse_document(&self.html)
    }

    /// The form `command()` submits (`document.forms[0]`).
    pub fn form(&self) -> Option<&Form> {
        self.forms.first()
    }

    pub fn hidden(&self, name: &str) -> Option<String> {
        self.forms.iter().find_map(|f| f.value(name).map(str::to_string))
    }

    pub fn has_command(&self, cmd: &str) -> bool {
        self.form().is_some_and(|f| f.allowed().contains(&cmd))
            || self.html.contains(&format!("command('{cmd}')"))
    }

    /// (label, command) for every element whose href/onclick calls command().
    pub fn commands(&self) -> Vec<(String, String)> {
        let doc = self.doc();
        let mut out = Vec::new();
        for el in doc.select(&sel("[href], [onclick]")) {
            let js = format!("{} {}", el.value().attr("href").unwrap_or(""), el.value().attr("onclick").unwrap_or(""));
            if let Some(m) = CMD_RE.captures(&js) {
                out.push((element_text(&el), m[1].to_string()));
            }
        }
        out
    }

    pub fn is_login_form(&self) -> bool {
        self.forms.iter().any(|f| f.get("LoginId").is_some() && f.get("Password").is_some())
    }

    pub fn is_logged_in(&self) -> bool {
        self.has_command("Logout") && !self.is_login_form()
    }

    pub fn text(&self) -> String {
        let doc = self.doc();
        let body = doc.select(&sel("body")).next();
        body.map(|b| visible_text(&b)).unwrap_or_default()
    }
}

pub fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

pub fn norm(s: &str) -> String {
    s.replace('\u{a0}', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn element_text(el: &ElementRef) -> String {
    norm(&el.text().collect::<String>())
}

/// Text of an element without the contents of <script>/<style>.
pub fn visible_text(el: &ElementRef) -> String {
    let mut s = String::new();
    for node in el.descendants() {
        if let Node::Text(t) = node.value() {
            let hidden = node.ancestors().any(|a| {
                a.value().as_element().is_some_and(|e| matches!(e.name(), "script" | "style" | "template"))
            });
            if !hidden {
                s.push_str(t);
                s.push(' ');
            }
        }
    }
    norm(&s)
}

fn build_forms(doc: &Html, url: &str, suffix: &'static str) -> Vec<Form> {
    let base = reqwest::Url::parse(url).ok();
    let mut forms: Vec<Form> = Vec::new();
    let mut form_ids = Vec::new();
    for f in doc.select(&sel("form")) {
        let action = f.value().attr("action").unwrap_or("");
        let action = base
            .as_ref()
            .and_then(|b| b.join(action).ok())
            .map(|u| u.to_string())
            .unwrap_or_else(|| action.to_string());
        forms.push(Form {
            action,
            method: f.value().attr("method").unwrap_or("GET").to_uppercase(),
            multipart: f.value().attr("enctype").is_some_and(|e| e.eq_ignore_ascii_case("multipart/form-data")),
            controls: Vec::new(),
            cmd_suffix: suffix,
            attachments: Vec::new(),
        });
        form_ids.push(f.id());
    }
    if forms.is_empty() {
        return forms;
    }
    // A control belongs to its enclosing form or, when the parser closed or
    // displaced the form (forms inside tables), to the last form opened before it.
    let mut current: Option<usize> = None;
    for node in doc.root_element().descendants() {
        let Some(el) = ElementRef::wrap(node) else { continue };
        let name = el.value().name();
        if name == "form" {
            current = form_ids.iter().position(|id| *id == el.id());
            continue;
        }
        if !matches!(name, "input" | "select" | "textarea") {
            continue;
        }
        let owner = el
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|a| a.value().name() == "form")
            .and_then(|f| form_ids.iter().position(|id| *id == f.id()))
            .or(current);
        if let Some(i) = owner {
            if let Some(c) = control_of(&el) {
                forms[i].controls.push(c);
            }
        }
    }
    forms
}

fn control_of(el: &ElementRef) -> Option<Control> {
    let v = el.value();
    let name = v.attr("name").unwrap_or("").to_string();
    let disabled = v.attr("disabled").is_some();
    let mut c = Control {
        name,
        kind: Kind::Text,
        value: String::new(),
        checked: v.attr("checked").is_some(),
        disabled,
        multiple: v.attr("multiple").is_some(),
        options: Vec::new(),
    };
    match v.name() {
        "input" => {
            let t = v.attr("type").unwrap_or("text").to_ascii_lowercase();
            c.kind = match t.as_str() {
                "hidden" => Kind::Hidden,
                "password" => Kind::Password,
                "checkbox" => Kind::Checkbox,
                "radio" => Kind::Radio,
                "submit" => Kind::Submit,
                "button" => Kind::Button,
                "image" => Kind::Image,
                "reset" => Kind::Reset,
                "file" => Kind::File,
                _ => Kind::Text,
            };
            let default = if matches!(c.kind, Kind::Checkbox | Kind::Radio) { "on" } else { "" };
            c.value = v.attr("value").unwrap_or(default).to_string();
        }
        "textarea" => {
            c.kind = Kind::Textarea;
            let t: String = el.text().collect();
            c.value = t.strip_prefix('\n').unwrap_or(&t).to_string();
        }
        "select" => {
            c.kind = Kind::Select;
            for o in el.select(&sel("option")) {
                let text = element_text(&o);
                c.options.push(SelectOption {
                    value: o.value().attr("value").map(str::to_string).unwrap_or(text),
                    selected: o.value().attr("selected").is_some(),
                    disabled: o.value().attr("disabled").is_some(),
                });
            }
        }
        _ => return None,
    }
    Some(c)
}

// ---------------------------------------------------------------------------
// Tables

pub fn direct_rows<'a>(table: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    table
        .select(&sel("tr"))
        .filter(|tr| tr.ancestors().filter_map(ElementRef::wrap).find(|a| a.value().name() == "table").map(|t| t.id()) == Some(table.id()))
        .collect()
}

/// Cells of a row, expanded by colspan so indices line up with the header.
pub fn cells<'a>(tr: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    let mut out = Vec::new();
    for c in tr.children().filter_map(ElementRef::wrap) {
        if matches!(c.value().name(), "td" | "th") {
            let span = c.value().attr("colspan").and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1).max(1);
            for _ in 0..span {
                out.push(c);
            }
        }
    }
    out
}

pub struct Table<'a> {
    pub headers: Vec<String>,
    pub rows: Vec<ElementRef<'a>>,
}

impl Table<'_> {
    /// Index of the first header that, with spaces removed, contains `key` (case-insensitive).
    pub fn col(&self, key: &str) -> Option<usize> {
        let key = key.to_lowercase().replace(' ', "");
        self.headers.iter().position(|h| h.to_lowercase().replace(' ', "").contains(&key))
    }
}

/// The innermost table having a row whose cells contain every key.
pub fn find_table<'a>(doc: &'a Html, keys: &[&str]) -> Option<Table<'a>> {
    let mut best: Option<(usize, Table<'a>)> = None;
    for table in doc.select(&sel("table")) {
        let rows = direct_rows(&table);
        for (i, tr) in rows.iter().enumerate() {
            let hs: Vec<String> = cells(tr).iter().map(element_text).collect();
            let squashed: Vec<String> = hs.iter().map(|h| h.to_lowercase().replace(' ', "")).collect();
            let all = keys.iter().all(|k| {
                let k = k.to_lowercase().replace(' ', "");
                squashed.iter().any(|h| h.contains(&k))
            });
            if all && hs.len() < 40 {
                let depth = table.ancestors().count();
                if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                    best = Some((depth, Table { headers: hs, rows: rows[i + 1..].to_vec() }));
                }
                break;
            }
        }
    }
    best.map(|(_, t)| t)
}

/// Command names called by `el` itself or by any element inside it.
pub fn commands_in(el: &ElementRef) -> Vec<String> {
    std::iter::once(*el)
        .chain(el.select(&sel("[href], [onclick]")))
        .filter_map(|a| {
            let js = format!("{} {}", a.value().attr("href").unwrap_or(""), a.value().attr("onclick").unwrap_or(""));
            CMD_RE.captures(&js).map(|m| m[1].to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOURNAL: &str = r#"<html><head><title>Manage Reviews</title>
<script src="external.v2.js"></script></head><body>
<table><tr><td><form method="POST" action="reviews.pl"></td></tr>
<tr><td><a href="javascript:command('Workspace')">Workspace</a>
<input type="checkbox" name="Command" value="" style="display:none">
<input type="hidden" name="Token" value="ab12">
<input type="checkbox" name="Opt" value="ON" checked>
<input type="checkbox" name="Off" value="ON">
<select name="Sel"><option>A</option><option value="b" selected>B</option></select>
<textarea name="Msg">
line1
line2</textarea>
<input type="button" name="SendMessage" value="Send">
<input type="hidden" name="Fields" value="Token,Command,Opt,Off,Sel,Msg,Workspace">
</td></tr></table></form></body></html>"#;

    #[test]
    fn journal_command_serialization() {
        let p = Page::parse("https://css.paperplaza.net/journals/tac/scripts/workspace.pl", JOURNAL.to_string());
        assert_eq!(p.title, "Manage Reviews");
        let f = p.form().unwrap();
        assert_eq!(f.action, "https://css.paperplaza.net/journals/tac/scripts/reviews.pl");
        let e: Vec<(String, String)> = f
            .entries(Some("Workspace"))
            .into_iter()
            .map(|e| match e.value {
                EntryValue::Text(t) => (e.name, t),
                _ => (e.name, "<file>".into()),
            })
            .collect();
        assert_eq!(
            e,
            vec![
                ("Command".into(), "Workspace".into()),
                ("Token".into(), "ab12".into()),
                ("Opt".into(), "ON".into()),
                ("Sel".into(), "b".into()),
                ("Msg".into(), "line1\r\nline2".into()),
                ("Fields".into(), "Token,Command,Opt,Off,Sel,Msg,Workspace".into()),
            ]
        );
        assert!(p.has_command("Workspace"));
        assert!(!p.has_command("CancelReview"));
    }

    #[test]
    fn conference_suffix() {
        let html = r#"<form method="POST" action="start.pl"><input type="checkbox" name="Command" value="ON">
<script>function command(arg){ document.forms[0].Command.checked = true; document.forms[0].Command.value = arg + '.x'; }</script></form>"#;
        let p = Page::parse("https://css.paperplaza.net/conferences/scripts/start.pl", html.to_string());
        let e = p.form().unwrap().entries(Some("Login"));
        assert!(matches!(&e[0].value, EntryValue::Text(t) if t == "Login.x"));
    }
}

