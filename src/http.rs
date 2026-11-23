use crate::page::{Entry, EntryValue, Form, Page};
use anyhow::{Context, Result, bail};
use encoding_rs::{Encoding, WINDOWS_1252};
use reqwest::blocking::{Client, Response, multipart};
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, ORIGIN, REFERER};
use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Minimum spacing between requests, to stay gentle with the server.
const MIN_GAP: Duration = Duration::from_millis(400);

/// Commands that only open or page through pages. In viewer mode no other
/// command is sent.
static VIEW_COMMANDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(Login|Logout|Access|Access_\d+_\d+|AccessReviewerspace|Workspace|Reviewers|Refresh|Next|Previous|First|Last|GoPage\d*|Reviews\d*|Details\d*|Inspect\d*|RequestReview|Search|SubmissionDetails\d*|CancelDetails|Download\w*)$").unwrap()
});

static FILENAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"filename\*?=(?:UTF-8'')?"?([^";]+)"?"#).unwrap());

/// A file the site sent instead of a page.
#[derive(Debug)]
pub struct Download {
    /// From Content-Disposition; may be empty.
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

pub enum Fetched {
    File(Download),
    Page(Page),
}

/// A request refused because the session is in viewer mode.
#[derive(Debug)]
pub struct ViewerMode(pub String);
impl std::fmt::Display for ViewerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "viewer mode: '{}' was not sent (log in without viewer mode to make changes)", self.0)
    }
}
impl std::error::Error for ViewerMode {}

/// Whether viewer mode lets this submission through.
pub fn viewer_allows(form: &Form, cmd: Option<&str>) -> bool {
    match cmd {
        // On Request a Review, "Next" leads to sending the invitations.
        Some("Next") if form.action.ends_with("/requestreview.pl") => false,
        // A search is only reading, except on pages that save something.
        Some("Search") => !form.multipart && form.action.ends_with("/pins.pl"),
        Some(c) => VIEW_COMMANDS.is_match(c) && !form.multipart,
        // Plain submits are only the "Show all" checkboxes of the two workspaces.
        None => !form.multipart && (form.action.ends_with("/workspace.pl") || form.action.ends_with("/reviewerspace.pl")),
    }
}

pub struct Http {
    client: Client,
    last: Option<Instant>,
    trace_dir: Option<PathBuf>,
    seq: u32,
    pub viewer: bool,
}

impl Http {
    pub fn new(trace_dir: Option<PathBuf>, viewer: bool) -> Result<Self> {
        let client = Client::builder()
            .cookie_store(true)
            .user_agent(concat!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Docket/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(90))
            .build()?;
        if let Some(d) = &trace_dir {
            std::fs::create_dir_all(d)?;
        }
        Ok(Http { client, last: None, trace_dir, seq: 0, viewer })
    }

    fn pace(&mut self) {
        if let Some(t) = self.last {
            let gap = t.elapsed();
            if gap < MIN_GAP {
                std::thread::sleep(MIN_GAP - gap);
            }
        }
        self.last = Some(Instant::now());
    }

    pub fn get(&mut self, url: &str, referer: Option<&str>) -> Result<Page> {
        self.pace();
        let mut rq = self.client.get(url);
        if let Some(r) = referer {
            rq = rq.header(REFERER, r);
        }
        let resp = rq.send().with_context(|| format!("GET {url}"))?;
        self.finish(resp, &format!("GET {url}"))
    }

    /// Submit `form` of `page` as `command(cmd)` would (or a plain submit if `cmd` is None).
    pub fn submit(&mut self, page: &Page, form: &Form, cmd: Option<&str>) -> Result<Page> {
        if self.viewer && !viewer_allows(form, cmd) {
            return Err(ViewerMode(cmd.unwrap_or("form submission").to_string()).into());
        }
        let label = format!("{} {} [{}]", form.method, form.action, cmd.unwrap_or("submit"));
        self.send(page, form, form.entries(cmd), label)
    }

    /// Submit `form` by clicking its submit button `button`. Never allowed in viewer mode.
    pub fn submit_clicking(&mut self, page: &Page, form: &Form, button: &str) -> Result<Page> {
        if self.viewer {
            return Err(ViewerMode(button.to_string()).into());
        }
        if !form.controls.iter().any(|c| c.name == button && c.kind == crate::page::Kind::Submit) {
            bail!("the page has no '{button}' button");
        }
        let label = format!("{} {} [button {button}]", form.method, form.action);
        self.send(page, form, form.entries_clicking(button), label)
    }

    /// Click a download link (`command(cmd)`): the site answers with the file,
    /// or with a page (an error, or the login form if the session expired).
    pub fn download(&mut self, page: &Page, form: &Form, cmd: &str) -> Result<Fetched> {
        if self.viewer && !viewer_allows(form, Some(cmd)) {
            return Err(ViewerMode(cmd.to_string()).into());
        }
        let label = format!("{} {} [{cmd}]", form.method, form.action);
        let resp = self.send_raw(page, form, form.entries(Some(cmd)), &label)?;
        let header = |h| resp.headers().get(h).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).unwrap_or("").to_string();
        let (mime, disposition) = (header(CONTENT_TYPE), header(CONTENT_DISPOSITION));
        if mime.contains("html") && !disposition.contains("attachment") {
            return Ok(Fetched::Page(self.finish(resp, &label)?));
        }
        let status = resp.status();
        let bytes = resp.bytes()?.to_vec();
        self.trace_text(&label, &format!("<file {mime:?} {disposition:?} {} bytes>", bytes.len()));
        if !status.is_success() {
            bail!("{label}: HTTP {status}");
        }
        let name = FILENAME.captures(&disposition).map(|c| c[1].trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_default();
        Ok(Fetched::File(Download { name, mime, bytes }))
    }

    fn send(&mut self, page: &Page, form: &Form, entries: Vec<Entry>, label: String) -> Result<Page> {
        let resp = self.send_raw(page, form, entries, &label)?;
        self.finish(resp, &label)
    }

    fn send_raw(&mut self, page: &Page, form: &Form, entries: Vec<Entry>, label: &str) -> Result<Response> {
        self.pace();
        let enc = page_encoding(&page.html);
        let origin = origin_of(&form.action);
        let resp = if form.method == "POST" {
            let rq = self.client.post(&form.action).header(REFERER, &page.url).header(ORIGIN, origin);
            if form.multipart {
                rq.multipart(multipart_body(&entries, enc)?).send()
            } else {
                rq.header(CONTENT_TYPE, "application/x-www-form-urlencoded").body(urlencode(&entries, enc)).send()
            }
        } else {
            let sep = if form.action.contains('?') { '&' } else { '?' };
            self.client.get(format!("{}{}{}", form.action, sep, urlencode(&entries, enc))).header(REFERER, &page.url).send()
        }
        .with_context(|| label.to_string())?;
        self.trace_request(label, &entries);
        Ok(resp)
    }

    /// A raw XMLHttpRequest-style POST, as the site's AJAX helpers send it.
    pub fn post_ajax(&mut self, url: &str, body: String, content_type: &str, referer: &str) -> Result<String> {
        if self.viewer {
            return Err(ViewerMode(url.rsplit('/').next().unwrap_or(url).to_string()).into());
        }
        self.pace();
        let resp = self
            .client
            .post(url)
            .header(CONTENT_TYPE, content_type)
            .header(REFERER, referer)
            .header(ORIGIN, origin_of(url))
            .body(body)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let (text, _) = decode(resp)?;
        self.trace_text(&format!("AJAX {url}"), &text);
        if !status.is_success() {
            bail!("{url} answered HTTP {status}");
        }
        Ok(text)
    }

    fn finish(&mut self, resp: Response, label: &str) -> Result<Page> {
        let status = resp.status();
        let url = resp.url().to_string();
        let (text, _) = decode(resp)?;
        self.trace_text(label, &text);
        if !status.is_success() {
            bail!("{label}: HTTP {status}");
        }
        Ok(Page::parse(&url, text))
    }

    fn trace_request(&mut self, label: &str, entries: &[Entry]) {
        let Some(dir) = &self.trace_dir else { return };
        let mut s = format!("{label}\n");
        for e in entries {
            let v = match &e.value {
                EntryValue::Text(_) if e.name == "Password" => "<redacted>".to_string(),
                EntryValue::Text(t) => t.clone(),
                EntryValue::File { filename, bytes, .. } => format!("<file {filename:?} {} bytes>", bytes.len()),
            };
            s.push_str(&format!("{}={}\n", e.name, v));
        }
        let _ = std::fs::write(dir.join(format!("{:04}-request.txt", self.seq + 1)), s);
    }

    fn trace_text(&mut self, label: &str, text: &str) {
        self.seq += 1;
        let Some(dir) = &self.trace_dir else { return };
        let _ = std::fs::write(dir.join(format!("{:04}-response.html", self.seq)), format!("<!-- {label} -->\n{text}"));
    }
}

fn decode(resp: Response) -> Result<(String, &'static Encoding)> {
    let header_charset = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("charset=").nth(1))
        .map(|c| c.trim_matches(|ch| ch == '"' || ch == ' ').to_string());
    let bytes = resp.bytes()?;
    let enc = header_charset
        .and_then(|c| Encoding::for_label(c.as_bytes()))
        .or_else(|| meta_charset(&bytes))
        .unwrap_or(WINDOWS_1252);
    let (text, _, _) = enc.decode(&bytes);
    Ok((text.into_owned(), enc))
}

fn meta_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_lowercase();
    let i = head.find("charset=")?;
    let label: String = head[i + 8..].chars().skip_while(|c| *c == '"' || *c == '\'').take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    Encoding::for_label(label.as_bytes())
}

/// The encoding a browser uses to submit this page's forms.
pub fn page_encoding(html: &str) -> &'static Encoding {
    meta_charset(html.as_bytes()).map(|e| e.output_encoding()).unwrap_or(WINDOWS_1252)
}

fn encode(s: &str, enc: &'static Encoding) -> Vec<u8> {
    // encoding_rs replaces unmappable characters with &#NNNN; as browsers do.
    enc.encode(s).0.into_owned()
}

pub fn urlencode(entries: &[Entry], enc: &'static Encoding) -> String {
    let mut out = String::new();
    for e in entries {
        let v = match &e.value {
            EntryValue::Text(t) => t.as_str(),
            EntryValue::File { filename, .. } => filename.as_str(),
        };
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&form_bytes(&encode(&e.name, enc)));
        out.push('=');
        out.push_str(&form_bytes(&encode(v, enc)));
    }
    out
}

fn form_bytes(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => s.push(b as char),
            b' ' => s.push('+'),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

fn multipart_body(entries: &[Entry], enc: &'static Encoding) -> Result<multipart::Form> {
    let mut form = multipart::Form::new();
    for e in entries {
        let part = match &e.value {
            EntryValue::Text(t) => multipart::Part::bytes(encode(t, enc)),
            EntryValue::File { filename, bytes, mime } => {
                multipart::Part::bytes(bytes.clone()).file_name(filename.clone()).mime_str(mime)?
            }
        };
        form = form.part(e.name.clone(), part);
    }
    Ok(form)
}

fn origin_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_like_browser() {
        let entries = vec![
            Entry { name: "Subject".into(), value: EntryValue::Text("Università & co".into()) },
            Entry { name: "Msg".into(), value: EntryValue::Text("a\u{2019}b \u{4e2d}".into()) },
        ];
        assert_eq!(urlencode(&entries, WINDOWS_1252), "Subject=Universit%E0+%26+co&Msg=a%92b+%26%2320013%3B");
    }
}
