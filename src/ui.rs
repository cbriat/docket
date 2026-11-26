//! Local browser interface: a small HTTP server on 127.0.0.1 with a JSON API.
//! Requests must carry the random token printed at startup, and the Host
//! header must be the loopback address (blocks other web pages and DNS rebinding).

use crate::creds;
use crate::http::Download;
use crate::model::{Assignment, Candidate, InviteOptions, Paper, PaperInfo, Review, ReviewDates, RoleKind};
use crate::papercept::{ContactOptions, EXTENSIONS, SITES, Session, same_id};
use crate::scheduler::{self, State, Store};
use crate::templates::{self, Templates};
use crate::notes::{Kind, Notes, Repeat, ThanksSettings};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX: &str = include_str!("../assets/index.html");
const MAX_ATTACHMENT: usize = 5 * 1024 * 1024;

/// Start the local server on a background thread; returns the page's address.
pub fn start(trace: Option<PathBuf>) -> Result<String> {
    let server = Server::http("127.0.0.1:0").map_err(|e| anyhow!("cannot start the local server: {e}"))?;
    let port = server.server_addr().to_ip().context("no local port")?.port();
    let token = random_token();
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    std::thread::spawn(move || {
        let mut app = App::new(trace, token, port);
        loop {
            match server.recv_timeout(Duration::from_secs(20)) {
                Ok(Some(rq)) => app.handle(rq),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("local server stopped: {e}");
                    std::process::exit(1);
                }
            }
            app.tick();
        }
    });
    Ok(url)
}

/// The app in a web browser instead of its own windows.
pub fn serve(trace: Option<PathBuf>, open_browser: bool) -> Result<()> {
    let url = start(trace)?;
    println!("Docket is running at\n  {url}\nKeep this window open while you work. Press Ctrl+C to quit.");
    if open_browser {
        let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(&url).spawn();
    }
    loop {
        std::thread::park();
    }
}

fn random_token() -> String {
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        b = (t ^ (std::process::id() as u128) << 64).to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

enum Act {
    Contact { cc: String, opts: ContactOptions, attachment: Option<(String, Vec<u8>)> },
    Cancel,
    Extend { days: u32 },
}

struct PlanItem {
    review: Review,
    subject: String,
    message: String,
    skip: Option<String>,
}

struct Plan {
    act: Act,
    items: Vec<PlanItem>,
}

struct App {
    trace: Option<PathBuf>,
    token: String,
    port: u16,
    session: Option<Session>,
    papers: Vec<Paper>,
    reviews: Vec<Review>,
    plans: HashMap<u64, Plan>,
    next_plan: u64,
    store: Store,
    last_check: Option<Instant>,
    /// Ranked reviewer candidates per paper id (the Reviewers list plus PIN
    /// database hits), kept so searches do not reload the 30 MB page.
    cands: HashMap<String, (PaperInfo, Vec<Candidate>)>,
    /// Review Details dates already read, for the date placeholders.
    dates: HashMap<String, ReviewDates>,
    /// The reviewer workspace rows last loaded.
    assignments: Vec<Assignment>,
    /// Files downloaded this session, served at /file/<index>.
    files: Vec<Stored>,
    /// Raised whenever reviews are re-read without the page asking (automatic
    /// invitations), so the page knows to fetch them again.
    reviews_rev: u64,
    /// PaperPlaza's letters as message templates, per "site|journal".
    site_letters: HashMap<String, Vec<crate::templates::Template>>,
    /// Thank-you notes the site's list offers in the open journal (for
    /// agreeing, for the review), for the menus; None when not known.
    unthanked: Option<[usize; 2]>,
    last_count: Option<Instant>,
}

/// A downloaded file kept in creds::files_dir().
#[derive(Clone)]
struct Stored {
    name: String,
    mime: String,
    path: PathBuf,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlanRequest {
    action: String,
    review_ids: Vec<String>,
    subject: String,
    cc: String,
    message: String,
    days: u32,
    do_not_archive: bool,
    my_eyes_only: bool,
    do_not_copy_me: bool,
    attachment_name: String,
    attachment_b64: String,
}

impl App {
    fn new(trace: Option<PathBuf>, token: String, port: u16) -> App {
        let _ = std::fs::remove_dir_all(creds::files_dir());
        App {
            trace,
            token,
            port,
            session: None,
            papers: Vec::new(),
            reviews: Vec::new(),
            plans: HashMap::new(),
            next_plan: 1,
            store: Store::load(),
            last_check: None,
            cands: HashMap::new(),
            dates: HashMap::new(),
            assignments: Vec::new(),
            files: Vec::new(),
            reviews_rev: 0,
            site_letters: HashMap::new(),
            unthanked: None,
            last_count: None,
        }
    }

    /// Check the schedules and site notes every few minutes (Settings →
    /// Automation), once a workspace is open, unless paused or outside the
    /// sending hours (then as soon as they allow it).
    fn tick(&mut self) {
        let Some(site) = self.session.as_ref().filter(|s| s.role.is_some()).map(|s| s.site().to_string()) else { return };
        let auto = creds::load_config().automation;
        // The thank-you notes to send, for the menus (only read, so at any hour).
        if self.last_count.is_some_and(|t| t.elapsed() >= auto.every()) {
            self.count_unthanked();
        }
        let due = self.last_check.is_none_or(|t| t.elapsed() >= auto.every());
        if !due || auto.may_send(chrono::Local::now().naive_local()).is_err() {
            return;
        }
        let notes = Notes::load().is_on(&site);
        if self.store.has_active() || notes {
            self.last_check = Some(Instant::now());
        }
        if self.store.has_active() {
            let _ = self.run_schedules(None);
        }
        if notes {
            let _ = self.run_notes(None);
        }
    }

    /// Read the site's thank-you list of the open journal (Associate Editor
    /// role) for the menus' counts.
    fn count_unthanked(&mut self) {
        self.last_count = Some(Instant::now());
        self.unthanked = None;
        let Some(session) = self.session.as_mut().filter(|s| s.role.as_ref().is_some_and(|r| r.kind == RoleKind::Editor)) else { return };
        if let Ok(list) = session.with_relogin(|x| x.unthanked()) {
            self.unthanked = Some(crate::notes::thanks_counts(&list));
        }
    }

    /// Run the schedules, then return to the workspace the user was in and
    /// re-read the reviewers of the papers shown whose schedule did something.
    fn run_schedules(&mut self, only: Option<u64>) -> Result<Vec<String>> {
        let session = self.session.as_mut().context("not logged in")?;
        let (journal, role) = (session.journal.clone(), session.role.clone());
        self.store = Store::load();
        let before = self.store.schedules.iter().map(|s| (s.id, fingerprint(s))).collect::<HashMap<_, _>>();
        let report = scheduler::run(session, &mut self.store, only);
        session.restore(journal, role)?;
        let site = session.site().to_string();
        let here = session.journal.as_ref().map(|j| j.code.clone()).unwrap_or_default();
        let changed: Vec<String> = self
            .store
            .schedules
            .iter()
            .filter(|s| s.site == site && s.journal == here && before.get(&s.id) != Some(&fingerprint(s)))
            .map(|s| s.paper_id.clone())
            .collect();
        for pid in changed {
            if self.papers.iter().any(|p| same_id(&p.id, &pid)) {
                let _ = self.refresh_paper(&pid);
            }
        }
        Ok(report)
    }

    /// Read one paper's reviews again from PaperPlaza.
    fn refresh_paper(&mut self, id: &str) -> Result<()> {
        let paper = self.paper(id)?;
        let session = self.session.as_mut().context("not logged in")?;
        let fresh = session.with_relogin(|s| s.load_reviews(&paper))?;
        self.reviews.retain(|r| r.paper_id != paper.id);
        self.reviews.extend(fresh);
        self.reviews_rev += 1;
        Ok(())
    }

    /// Check the site notes (sending those approved, with `approved`), then
    /// return to the workspace the user was in.
    fn run_notes(&mut self, approved: Option<&[String]>) -> Result<Vec<String>> {
        let session = self.session.as_mut().context("not logged in")?;
        let (journal, role) = (session.journal.clone(), session.role.clone());
        let mut n = Notes::load();
        let report = crate::notes::run(session, &mut n, approved);
        session.restore(journal, role)?;
        self.count_unthanked();
        Ok(report)
    }

    /// The site-note settings, for the settings window (with the current journal's choice).
    fn notes_state(&self, n: &Notes) -> Value {
        let site = self.session.as_ref().map(|s| s.site().to_string()).unwrap_or_default();
        let journal = self.session.as_ref().and_then(|s| s.journal.as_ref());
        let here = journal.and_then(|j| n.place(&site, &j.code)).is_some();
        json!({"thanks": n.thanks, "confirm": n.confirm, "remind": n.remind, "journals": n.journals, "here": here,
               "on_here": n.is_on(&site), "pending": n.pending, "no_journal": n.any_on() && n.journals.is_empty()})
    }

    /// Everything the settings window shows.
    fn settings(&self) -> Value {
        let cfg = creds::load_config();
        let exe = std::env::current_exe().unwrap_or_default();
        json!({
            "auto_login": cfg.auto_login, "viewer": cfg.viewer, "dock": cfg.dock,
            "automation": cfg.automation, "notify": cfg.notify, "invite_defaults": cfg.invite_defaults,
            "message_defaults": cfg.message_defaults, "table": cfg.table,
            "open_at_login": creds::open_at_login(), "can_open_at_login": exe.to_string_lossy().contains(".app/Contents/MacOS/"),
            "folder": creds::config_dir(), "notes": self.notes_state(&Notes::load()),
        })
    }

    fn handle(&mut self, mut rq: Request) {
        let host_ok = rq.headers().iter().any(|h| {
            h.field.equiv("Host") && [format!("127.0.0.1:{}", self.port), format!("localhost:{}", self.port)].contains(&h.value.to_string())
        });
        if !host_ok {
            let _ = rq.respond(Response::from_string("Forbidden").with_status_code(403));
            return;
        }
        let url = rq.url().to_string();
        if *rq.method() == Method::Get && (url == "/" || url.starts_with("/?")) {
            let resp = if url.contains(&format!("t={}", self.token)) {
                html(INDEX.replace("__DOCKET_TOKEN__", &self.token))
            } else {
                Response::from_string("Open the link printed in the terminal window.").with_status_code(403)
            };
            let _ = rq.respond(resp);
            return;
        }
        if *rq.method() == Method::Get && url.starts_with("/file/") {
            let (path, query) = url.split_once('?').unwrap_or((&url, ""));
            let file = path[6..].parse::<usize>().ok().and_then(|i| self.files.get(i));
            let resp = match file {
                Some(f) if query.split('&').any(|kv| kv == format!("t={}", self.token)) => match std::fs::read(&f.path) {
                    Ok(bytes) => Response::from_data(bytes)
                        .with_header(Header::from_bytes("Content-Type", f.mime.as_bytes()).unwrap())
                        .with_header(Header::from_bytes("Content-Disposition", format!("inline; filename=\"{}\"", f.name.replace('"', "")).as_bytes()).unwrap())
                        .with_header("Cache-Control: no-store".parse::<Header>().unwrap()),
                    Err(_) => Response::from_string("The file is no longer there; open it again from Docket.").with_status_code(404),
                },
                _ => Response::from_string("Forbidden").with_status_code(403),
            };
            let _ = rq.respond(resp);
            return;
        }
        let authorized = rq.headers().iter().any(|h| h.field.equiv("X-Docket-Token") && h.value.as_str() == self.token);
        if !authorized || *rq.method() != Method::Post || !url.starts_with("/api/") {
            let _ = rq.respond(Response::from_string("Forbidden").with_status_code(403));
            return;
        }
        let mut body = String::new();
        let resp = match rq.as_reader().take(16 * 1024 * 1024).read_to_string(&mut body) {
            Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
            Ok(_) => {
                let input: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                match self.api(&url[5..], input) {
                    Ok(mut v) => {
                        v["ok"] = json!(true);
                        v
                    }
                    Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
                }
            }
        };
        let _ = rq.respond(
            Response::from_string(resp.to_string())
                .with_header("Content-Type: application/json".parse::<Header>().unwrap())
                .with_header("Cache-Control: no-store".parse::<Header>().unwrap()),
        );
    }

    fn api(&mut self, name: &str, input: Value) -> Result<Value> {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match name {
            "state" => Ok(self.state()),
            "login" => {
                let (id, mut pw) = (s("login_id"), s("password"));
                let site = Some(s("site")).filter(|x| !x.is_empty()).unwrap_or_else(|| SITES[0].0.to_string());
                if id.is_empty() {
                    bail!("enter your PIN, login alias or e-mail");
                }
                let account = creds::account(&site, &id);
                let saved = pw.is_empty();
                if saved {
                    pw = creds::get_password(&account).context("enter your password (none is saved for this login)")?;
                }
                let viewer = input.get("viewer").and_then(Value::as_bool).unwrap_or(false);
                let mut session = Session::new(self.trace.clone(), viewer)?.for_site(&site)?;
                session.login(&id, &pw)?;
                let mut cfg = creds::load_config();
                cfg.set_login(&site, &id);
                cfg.viewer = viewer;
                creds::save_config(&cfg)?;
                if !saved && input.get("remember").and_then(Value::as_bool).unwrap_or(false) {
                    creds::set_password(&account, &pw)?;
                }
                self.session = Some(session);
                self.papers.clear();
                self.reviews.clear();
                self.last_check = None;
                self.unthanked = None;
                self.last_count = None;
                self.cands.clear();
                self.dates.clear();
                Ok(self.state())
            }
            "journal" => {
                let code = s("code");
                self.session_mut()?.enter_journal(&code)?;
                let mut cfg = creds::load_config();
                cfg.journal = Some(code);
                creds::save_config(&cfg)?;
                self.papers.clear();
                self.reviews.clear();
                self.unthanked = None;
                Ok(self.state())
            }
            "role" => {
                let cmd = s("command");
                let session = self.session_mut()?;
                session.enter_role(&cmd)?;
                let label = session.role.as_ref().map(|r| r.label.clone());
                let mut cfg = creds::load_config();
                cfg.role_label = label;
                creds::save_config(&cfg)?;
                self.papers.clear();
                self.reviews.clear();
                self.plans.clear();
                self.unthanked = None;
                Ok(self.state())
            }
            "load" => {
                self.dates.clear();
                let include_all = input.get("include_all").and_then(Value::as_bool).unwrap_or(false);
                let session = self.session.as_mut().context("not logged in")?;
                if session.role.as_ref().is_some_and(|r| r.kind == RoleKind::Reviewer) {
                    let assignments = session.with_relogin(|s| s.load_assignments(include_all))?;
                    self.unthanked = None;
                    self.papers.clear();
                    self.reviews.clear();
                    self.assignments = assignments;
                    return Ok(json!({"mode": "reviewer", "assignments": self.assignments, "warnings": []}));
                }
                let papers = session.with_relogin(|s| s.load_papers(include_all))?;
                let mut reviews = Vec::new();
                let mut warnings = Vec::new();
                for p in &papers {
                    match session.with_relogin(|s| s.load_reviews(p)) {
                        Ok(rs) => reviews.extend(rs),
                        Err(e) => warnings.push(format!("{}: could not read the reviews ({e:#})", p.number)),
                    }
                }
                self.papers = papers;
                self.reviews = reviews;
                self.assignments.clear();
                self.count_unthanked();
                let folded = creds::load_config().folded.get(&self.fold_key()).cloned().unwrap_or_default();
                Ok(json!({"mode": "editor", "papers": self.papers, "reviews": self.reviews, "warnings": warnings, "folded": folded, "unthanked": self.unthanked}))
            }
            "plan" => {
                if self.session.as_ref().is_some_and(Session::viewer) {
                    bail!("viewer mode: nothing can be sent or changed. Log out and log in without viewer mode to do this.");
                }
                self.plan(serde_json::from_value(input)?)
            }
            "review_text" => {
                let id = s("review_id");
                let r = self.reviews.iter().find(|r| r.review_id == id).cloned().with_context(|| format!("unknown review {id}; reload the list"))?;
                let session = self.session.as_mut().context("not logged in")?;
                Ok(json!({"review": session.with_relogin(|x| x.review_text(&r))?}))
            }
            "inspect" => {
                let id = s("review_id");
                let session = self.session.as_mut().context("not logged in")?;
                Ok(json!({"inspection": session.with_relogin(|x| x.inspect_reviews(&id))?}))
            }
            "fill" => {
                let id = s("review_id");
                let r = self.reviews.iter().find(|r| r.review_id == id).cloned().with_context(|| format!("unknown review {id}; reload the list"))?;
                let (subject, message) = self.fill_for(&r, &s("subject"), &s("message"))?;
                Ok(json!({"subject": subject, "message": message}))
            }
            "refresh_paper" => {
                self.refresh_paper(&s("paper_id"))?;
                Ok(json!({"reviews": self.reviews, "reviews_rev": self.reviews_rev}))
            }
            "snapshot" => {
                // What the main window shows, for the other windows (nothing is read from the site).
                let reviewer = self.session.as_ref().and_then(|s| s.role.as_ref()).is_some_and(|r| r.kind == RoleKind::Reviewer);
                let folded = creds::load_config().folded.get(&self.fold_key()).cloned().unwrap_or_default();
                Ok(json!({"mode": if reviewer { "reviewer" } else { "editor" }, "papers": self.papers, "reviews": self.reviews,
                          "assignments": self.assignments, "folded": folded}))
            }
            "file" => {
                let (kind, cmd) = (s("kind"), s("command"));
                let session = self.session.as_mut().context("not logged in")?;
                let (d, fallback) = match kind.as_str() {
                    "manuscript" => {
                        let paper = self.papers.iter().find(|p| same_id(&p.id, &s("paper_id"))).cloned().context("this paper is not loaded; reload the list")?;
                        (session.with_relogin(|x| x.manuscript(&paper))?, paper.number.clone())
                    }
                    "review" => {
                        let id = s("review_id");
                        let r = self.reviews.iter().find(|r| r.review_id == id).cloned().with_context(|| format!("unknown review {id}; reload the list"))?;
                        (session.with_relogin(|x| x.review_attachment(&r, &cmd))?, format!("{} review {}", r.paper_id, r.review_id))
                    }
                    "submission" => {
                        let id = s("review_id");
                        let a = self.assignments.iter().find(|a| a.review_id == id).cloned().with_context(|| format!("unknown review {id}; reload the list"))?;
                        (session.with_relogin(|x| x.submission_file(&a.details_cmd, &cmd))?, format!("{} {}", a.number, s("label")))
                    }
                    other => bail!("unknown kind of file '{other}'"),
                };
                let id = self.keep_file(d, &fallback, &cmd)?;
                let f = &self.files[id];
                Ok(json!({"id": id, "name": f.name, "path": f.path, "url": format!("/file/{id}?t={}", self.token)}))
            }
            "submission_files" => {
                let id = s("review_id");
                let a = self.assignments.iter().find(|a| a.review_id == id).cloned().with_context(|| format!("unknown review {id}; reload the list"))?;
                if a.details_cmd.is_empty() {
                    bail!("the workspace shows no link to the files of {}", a.number);
                }
                let session = self.session.as_mut().context("not logged in")?;
                Ok(json!({"files": session.with_relogin(|x| x.submission_files(&a.details_cmd))?}))
            }
            "reveal" => {
                let i = input.get("id").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
                let f = self.files.get(i).cloned().context("that file is no longer known; open it again")?;
                reveal(&f.path, &s("how"))
            }
            "save_download" => {
                // CSV export: the app windows cannot save a page's download themselves.
                let path = unique_download(&s("name"))?;
                std::fs::write(&path, s("text"))?;
                reveal(&path, "finder")
            }
            "templates" => {
                let t = Templates::load();
                let placeholders: Vec<Value> = templates::PLACEHOLDERS.iter().map(|(p, d)| json!({"name": p, "about": d})).collect();
                Ok(json!({"signature": t.signature, "list": t.list, "defaults": Templates::defaults().list, "placeholders": placeholders}))
            }
            "templates_save" => {
                let t: Templates = serde_json::from_value(json!({"signature": input.get("signature").cloned().unwrap_or(json!("")), "list": input.get("list").cloned().unwrap_or(json!([]))}))?;
                t.save()?;
                Ok(json!({"signature": t.signature, "list": t.list}))
            }
            "fold" => {
                let ids: Vec<String> = input.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
                let mut cfg = creds::load_config();
                cfg.folded.insert(self.fold_key(), ids);
                creds::save_config(&cfg)?;
                Ok(json!({}))
            }
            "paper_details" => {
                let paper = self.paper(&s("paper_id"))?;
                if let Some((info, _)) = self.cands.get(&paper.id).filter(|(i, _)| !i.abstract_text.is_empty()) {
                    return Ok(json!({"abstract": info.abstract_text, "keywords": info.keywords}));
                }
                let session = self.session.as_mut().context("not logged in")?;
                let (abstract_text, keywords) = session.with_relogin(|x| x.load_abstract(&paper))?;
                Ok(json!({"abstract": abstract_text, "keywords": keywords}))
            }
            "candidates" => {
                let paper = self.paper(&s("paper_id"))?;
                let refresh = input.get("refresh").and_then(Value::as_bool).unwrap_or(false);
                if refresh || !self.cands.contains_key(&paper.id) {
                    let session = self.session.as_mut().context("not logged in")?;
                    let loaded = session.with_relogin(|x| x.load_candidates(&paper))?;
                    self.cands.insert(paper.id.clone(), loaded);
                }
                let (info, all) = &self.cands[&paper.id];
                let q = s("q").to_lowercase();
                let words: Vec<&str> = q.split_whitespace().collect();
                // PINs are numbered in order of registration: a higher one is
                // usually a younger researcher.
                let min_pin = s("min_pin").trim().parse::<u64>().ok();
                // Kinds of title to keep (see model::title_group); none listed: any.
                let titles: Vec<String> = input.get("titles").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
                let hit = |c: &&Candidate| {
                    let hay = format!("{} {} {} {} {}", c.name, c.pin, c.affiliation, c.email, c.keywords).to_lowercase();
                    words.iter().all(|w| hay.contains(w))
                        && min_pin.is_none_or(|m| c.pin.parse::<u64>().is_ok_and(|p| p >= m))
                        && (titles.is_empty() || titles.iter().any(|t| t == crate::model::title_group(&c.title)))
                };
                let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(150) as usize;
                let shown: Vec<&Candidate> = all.iter().filter(hit).take(limit).collect();
                let matching = if words.is_empty() && min_pin.is_none() && titles.is_empty() { all.len() } else { all.iter().filter(hit).count() };
                Ok(json!({"paper": info, "candidates": shown, "total": all.len(), "matching": matching,
                          "in_list": all.iter().filter(|c| c.in_list).count()}))
            }
            "pin_search" => {
                let paper = self.paper(&s("paper_id"))?;
                let session = self.session.as_mut().context("not logged in")?;
                let (field, query) = (s("field"), s("query"));
                let (found, more) = session.with_relogin(|x| x.search_pins(if field.is_empty() { "Keywords" } else { &field }, &query, 5))?;
                let (info, all) = self.cands.get_mut(&paper.id).context("open the reviewer list of this paper first")?;
                let mut new = 0;
                for c in found.iter() {
                    if !all.iter().any(|x| x.pin == c.pin) {
                        all.push(c.clone());
                        new += 1;
                    }
                }
                crate::recommend::rank(info, all);
                Ok(json!({"found": found.len(), "new": new, "more_pages": more}))
            }
            "add_to_list" => {
                if self.session.as_ref().is_some_and(Session::viewer) {
                    bail!("viewer mode: your Reviewers list cannot be changed. Log in without viewer mode to do this.");
                }
                let paper_id = s("paper_id");
                let pins: Vec<String> = input.get("pins").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
                if pins.is_empty() {
                    bail!("no one to add");
                }
                let session = self.session.as_mut().context("not logged in")?;
                let mut results = Vec::new();
                for pin in &pins {
                    let r = session.with_relogin(|x| x.add_to_list(pin));
                    if r.is_ok() {
                        if let Some((_, all)) = self.cands.get_mut(&paper_id) {
                            if let Some(c) = all.iter_mut().find(|c| &c.pin == pin) {
                                c.in_list = true;
                            }
                        }
                    }
                    results.push(match r {
                        Ok(m) => json!({"pin": pin, "ok": true, "message": m}),
                        Err(e) => json!({"pin": pin, "ok": false, "message": format!("{e:#}")}),
                    });
                }
                Ok(json!({"results": results}))
            }
            "invite" => {
                if self.session.as_ref().is_some_and(Session::viewer) {
                    bail!("viewer mode: no invitation can be sent. Log in without viewer mode to do this.");
                }
                let paper = self.paper(&s("paper_id"))?;
                let pins: Vec<String> = input.get("pins").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
                let opts: InviteOptions = serde_json::from_value(input.get("options").cloned().unwrap_or(json!({})))?;
                let session = self.session.as_mut().context("not logged in")?;
                let results = session.with_relogin(|x| x.request_reviews(&paper, &pins, &opts))?;
                if let Some((_, all)) = self.cands.get_mut(&paper.id) {
                    for r in results.iter().filter(|r| r.ok) {
                        if let Some(c) = all.iter_mut().find(|c| c.pin == r.pin) {
                            c.available = false;
                            c.in_list = true;
                        }
                    }
                }
                // The invitations went out: a failure to re-read the list is only a warning.
                let warning = self.refresh_paper(&paper.id).err().map(|e| format!("The reviewers of {} could not be read again ({e:#}); use Refresh this paper.", paper.number));
                Ok(json!({"results": results, "reviews": self.reviews, "warning": warning}))
            }
            "schedules" => {
                self.store = Store::load();
                Ok(json!({"schedules": self.store.schedules}))
            }
            "schedule_save" => {
                let paper = self.paper(&s("paper_id"))?;
                let journal = self.session.as_ref().and_then(|x| x.journal.as_ref()).map(|j| j.code.clone()).context("no journal is open")?;
                let n = |k: &str| input.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
                let days = n("no_answer_days");
                let queue: Vec<(String, String)> = input
                    .get("queue")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|b| (b["pin"].as_str().unwrap_or("").to_string(), b["name"].as_str().unwrap_or("").to_string())).collect())
                    .unwrap_or_default();
                let id = input.get("id").and_then(Value::as_u64);
                self.store = Store::load();
                let id = self.store.upsert(id, &paper, &journal, n("needed"), n("at_once"), (days > 0).then_some(days), queue)?;
                let opts: InviteOptions = serde_json::from_value(input.get("options").cloned().unwrap_or(json!({})))?;
                let site = self.session.as_ref().map(|x| x.site().to_string()).unwrap_or_default();
                if let Some(sch) = self.store.schedules.iter_mut().find(|x| x.id == id) {
                    sch.invite = InviteOptions { deadline: None, ..opts };
                    sch.site = site;
                }
                self.store.save()?;
                let report = self.run_schedules(Some(id))?;
                Ok(json!({"schedules": self.store.schedules, "report": report, "id": id, "reviews": self.reviews}))
            }
            "schedule_run" => {
                let id = input.get("id").and_then(Value::as_u64);
                let report = self.run_schedules(id)?;
                Ok(json!({"schedules": self.store.schedules, "report": report, "reviews": self.reviews}))
            }
            "staff_contacts" => {
                let paper = self.paper(&s("paper_id"))?;
                let session = self.session_mut()?;
                if session.viewer() {
                    bail!("viewer mode: messages cannot be sent. Log in without viewer mode to do this.");
                }
                let (handlers, subject) = session.with_relogin(|x| x.staff_contacts(&paper))?;
                Ok(json!({"handlers": handlers, "subject": subject}))
            }
            "staff_send" => {
                let paper = self.paper(&s("paper_id"))?;
                let to: Vec<String> = input.get("to").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let flag = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(false);
                let opts = ContactOptions { do_not_archive: false, my_eyes_only: flag("my_eyes_only"), do_not_copy_me: flag("do_not_copy_me") };
                let attachment = match s("attachment_b64") {
                    b if b.is_empty() => None,
                    b => {
                        let bytes = base64::engine::general_purpose::STANDARD.decode(b.as_bytes())?;
                        if bytes.len() > MAX_ATTACHMENT {
                            bail!("the attachment is larger than the site's 5 MB limit");
                        }
                        Some((s("attachment_name"), bytes))
                    }
                };
                let (subject, cc, message) = (s("subject"), s("cc"), s("message"));
                let session = self.session_mut()?;
                let sent = session.with_relogin(|x| x.contact_staff(&paper, &to, &subject, &cc, &message, &opts, attachment.as_ref().map(|(n, b)| (n.as_str(), b.as_slice()))))?;
                Ok(json!({"message": sent}))
            }
            "settings" => Ok(self.settings()),
            "settings_save" => {
                // Only the keys given are changed.
                let mut cfg = creds::load_config();
                let get = |k: &str| input.get(k).cloned();
                if let Some(v) = get("auto_login") {
                    cfg.auto_login = serde_json::from_value(v)?;
                }
                if let Some(v) = get("viewer") {
                    cfg.viewer = serde_json::from_value(v)?;
                }
                if let Some(v) = get("dock") {
                    let d: String = serde_json::from_value(v)?;
                    if !["", "always", "never"].contains(&d.as_str()) {
                        bail!("unknown Dock choice '{d}'");
                    }
                    cfg.dock = d;
                }
                if let Some(v) = get("automation") {
                    let a: creds::Automation = serde_json::from_value(v)?;
                    if a.hours.is_some_and(|[f, t]| f > 23 || t > 24) {
                        bail!("sending hours are from 0 to 24");
                    }
                    cfg.automation = a;
                }
                if let Some(v) = get("notify") {
                    cfg.notify = serde_json::from_value(v)?;
                }
                if let Some(v) = get("invite_defaults") {
                    let d: creds::InviteDefaults = serde_json::from_value(v)?;
                    if d.needed == 0 || d.at_once < d.needed {
                        bail!("invited at once must be at least the number of reviewers needed (at least 1)");
                    }
                    cfg.invite_defaults = d;
                }
                if let Some(v) = get("message_defaults") {
                    cfg.message_defaults = serde_json::from_value(v)?;
                }
                if let Some(v) = get("table") {
                    cfg.table = serde_json::from_value(v)?;
                }
                if let Some(v) = get("open_at_login") {
                    let on: bool = serde_json::from_value(v)?;
                    creds::set_open_at_login(on, &std::env::current_exe()?)?;
                    if on {
                        // Opening at login is only useful if Docket can log in by itself.
                        cfg.auto_login = true;
                    }
                }
                creds::save_config(&cfg)?;
                Ok(self.settings())
            }
            "notes" => Ok(self.notes_state(&Notes::load())),
            "notes_save" => {
                let thanks: ThanksSettings = serde_json::from_value(input.get("thanks").cloned().unwrap_or(json!({})))?;
                let confirm: Repeat = serde_json::from_value(input.get("confirm").cloned().unwrap_or(json!({})))?;
                let remind: Repeat = serde_json::from_value(input.get("remind").cloned().unwrap_or(json!({})))?;
                let site = self.session.as_ref().map(|x| x.site().to_string()).unwrap_or_default();
                let journal = self.session.as_ref().and_then(|x| x.journal.clone());
                let here = input.get("here").and_then(Value::as_bool);
                // A template chosen instead of the site's letter must exist and suit the site's pages.
                let all = crate::templates::Templates::load();
                for (what, name) in [("thanks for agreeing to review", &thanks.accept_letter), ("thanks for the review", &thanks.review_letter),
                                     ("confirmation requests", &confirm.letter), ("reminders", &remind.letter)] {
                    if name.is_empty() {
                        continue;
                    }
                    let t = all.list.iter().find(|t| &t.name == name).with_context(|| format!("the template “{name}” does not exist"))?;
                    crate::letters::check(&t.body).with_context(|| format!("letter for the {what} (“{name}”)"))?;
                }
                let mut n = Notes::load();
                let place = match (&journal, here) {
                    (Some(j), Some(on)) => Some((site.as_str(), j.code.as_str(), j.name.as_str(), on)),
                    _ => None,
                };
                n.set(thanks, confirm, remind, place, chrono::Local::now().date_naive());
                n.save()?;
                Ok(self.notes_state(&n))
            }
            "notes_remove_place" => {
                let mut n = Notes::load();
                n.remove_place(&s("site"), &s("journal"));
                n.save()?;
                Ok(self.notes_state(&n))
            }
            "notes_run" => {
                let report = self.run_notes(None)?;
                let mut v = self.notes_state(&Notes::load());
                v["report"] = json!(report);
                Ok(v)
            }
            "notes_send" => {
                let keys: Vec<String> = serde_json::from_value(input.get("keys").cloned().unwrap_or(json!([])))?;
                if keys.is_empty() {
                    bail!("select at least one note");
                }
                let report = self.run_notes(Some(&keys))?;
                let mut v = self.notes_state(&Notes::load());
                v["report"] = json!(report);
                Ok(v)
            }
            "site_offer" => {
                // What PaperPlaza's own page offers for one kind of note, in the open journal.
                let kind: Kind = serde_json::from_value(input.get("kind").cloned().unwrap_or(json!("accept")))?;
                let session = self.session.as_mut().context("not logged in")?;
                if session.role.as_ref().is_none_or(|r| r.kind != RoleKind::Editor) {
                    bail!("open the Associate Editor role of a journal first");
                }
                let (rows, letter) = crate::notes::offered(session, kind)?;
                let n = Notes::load();
                let all = crate::templates::Templates::load();
                let usable: Vec<Value> = all.list.iter().map(|t| json!({"name": t.name, "body": t.body, "problem": crate::letters::check(&t.body).err().map(|e| format!("{e:#}"))})).collect();
                let (copy, attach) = match kind {
                    Kind::Accept | Kind::Review => (n.thanks.copy_me, false),
                    Kind::Confirm => (n.confirm.copy_me, n.confirm.attach),
                    Kind::Remind => (n.remind.copy_me, n.remind.attach),
                };
                let rows: Vec<Value> = rows.iter().map(|d| json!({"review_id": d.review_id, "paper": d.paper, "reviewer": d.reviewer, "why": d.why})).collect();
                Ok(json!({"kind": kind, "what": kind.what(), "rows": rows, "letter": letter, "templates": usable, "chosen": n.letter_name(kind),
                          "copy_me": copy, "attach": attach, "journal": session.journal.as_ref().map(|j| j.name.clone()), "viewer": session.viewer()}))
            }
            "site_send" => {
                let kind: Kind = serde_json::from_value(input.get("kind").cloned().unwrap_or(json!(null)))?;
                let ids: Vec<String> = serde_json::from_value(input.get("ids").cloned().unwrap_or(json!([])))?;
                if ids.is_empty() {
                    bail!("select at least one review");
                }
                let letter = input.get("letter").and_then(Value::as_str).map(String::from);
                let copy = input.get("copy_me").and_then(Value::as_bool).unwrap_or(false);
                let attach = input.get("attach").and_then(Value::as_bool).unwrap_or(false);
                let session = self.session.as_mut().context("not logged in")?;
                if session.role.as_ref().is_none_or(|r| r.kind != RoleKind::Editor) {
                    bail!("open the Associate Editor role of a journal first");
                }
                let report = crate::notes::send_by_hand(session, kind, &ids, letter, copy, attach)?;
                if matches!(kind, Kind::Accept | Kind::Review) {
                    self.count_unthanked();
                }
                // The reviews' reminders and thanks shown in the table.
                let mut papers: Vec<String> = self.reviews.iter().filter(|r| ids.contains(&r.review_id)).map(|r| r.paper_id.clone()).collect();
                papers.dedup();
                for p in papers {
                    let _ = self.refresh_paper(&p);
                }
                Ok(json!({"report": report, "reviews": self.reviews, "reviews_rev": self.reviews_rev, "unthanked": self.unthanked}))
            }
            "site_letters" => {
                // PaperPlaza's letters of the open journal, as message templates (read once).
                let session = self.session.as_mut().context("not logged in")?;
                if session.role.as_ref().is_none_or(|r| r.kind != RoleKind::Editor) {
                    return Ok(json!({"templates": []}));
                }
                let k = format!("{}|{}", session.site(), session.journal.as_ref().map(|j| j.code.as_str()).unwrap_or_default());
                if !self.site_letters.contains_key(&k) {
                    let (_, accept, review) = session.with_relogin(|x| x.thank_you_page())?;
                    let (_, remind) = session.with_relogin(|x| x.site_list_page(crate::papercept::SiteList::Reminders))?;
                    let (_, confirm) = session.with_relogin(|x| x.site_list_page(crate::papercept::SiteList::Confirmations))?;
                    let list = [("accept", accept), ("review", review), ("confirm", confirm), ("remind", remind)]
                        .into_iter()
                        .filter(|(_, l)| !l.trim().is_empty())
                        .map(|(kind, l)| crate::letters::from_site(kind, &l))
                        .collect();
                    self.site_letters.insert(k.clone(), list);
                }
                Ok(json!({"templates": self.site_letters[&k]}))
            }
            "automation_status" => {
                let cfg = creds::load_config();
                let site = self.session.as_ref().map(|x| x.site().to_string()).unwrap_or_default();
                let n = Notes::load();
                Ok(json!({"paused": cfg.automation.paused, "pending": n.pending.iter().filter(|p| p.site == site).count(), "notes_on": n.is_on(&site),
                          "notes_no_journal": n.any_on() && n.journals.is_empty(), "unthanked": self.unthanked,
                          "reviews_rev": self.reviews_rev}))
            }
            "activity" => {
                let mut all = crate::activity::load();
                all.reverse();
                Ok(json!({"entries": all}))
            }
            "reveal_settings" => {
                let dir = creds::config_dir();
                std::fs::create_dir_all(&dir)?;
                if !cfg!(test) {
                    std::process::Command::new("open").arg(&dir).spawn()?;
                }
                Ok(json!({}))
            }
            "schedule_preview" => {
                let session = self.session.as_mut().context("not logged in")?;
                let (journal, role) = (session.journal.clone(), session.role.clone());
                self.store = Store::load();
                let (lines, any) = scheduler::preview(session, &self.store);
                session.restore(journal, role)?;
                Ok(json!({"lines": lines, "any": any}))
            }
            "schedule_set_all" => {
                // Pause every active schedule, or resume every paused one.
                let (from, to) = if s("state") == "paused" { (State::Active, State::Paused) } else { (State::Paused, State::Active) };
                self.store = Store::load();
                for sch in self.store.schedules.iter_mut().filter(|x| x.state == from) {
                    sch.state = to;
                }
                self.store.save()?;
                Ok(json!({"schedules": self.store.schedules}))
            }
            "settings_export" => {
                let doc = creds::export_settings()?;
                let path = unique_download(&format!("Docket settings {}.json", chrono::Local::now().format("%Y-%m-%d")))?;
                std::fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
                reveal(&path, "finder")?;
                Ok(json!({"path": path}))
            }
            "settings_import" => {
                let doc: Value = serde_json::from_str(&s("text")).context("this file is not a Docket settings export")?;
                let backup = creds::import_settings(&doc)?;
                // Start again from the imported settings: log out, reload the schedules.
                if let Some(mut x) = self.session.take() {
                    x.logout();
                }
                self.papers.clear();
                self.reviews.clear();
                self.plans.clear();
                self.cands.clear();
                self.store = Store::load();
                let mut st = self.state();
                st["backup"] = json!(backup);
                Ok(st)
            }
            "schedule_set" => {
                let id = input.get("id").and_then(Value::as_u64).unwrap_or(0);
                self.store = Store::load();
                let sch = self.store.schedules.iter_mut().find(|x| x.id == id).context("that schedule no longer exists")?;
                match s("state").as_str() {
                    "paused" if sch.state == State::Active => sch.state = State::Paused,
                    "active" if sch.state == State::Paused => sch.state = State::Active,
                    other => bail!("cannot set a {:?} schedule to '{other}'", sch.state),
                }
                self.store.save()?;
                Ok(json!({"schedules": self.store.schedules}))
            }
            "schedule_delete" => {
                let id = input.get("id").and_then(Value::as_u64).unwrap_or(0);
                self.store = Store::load();
                self.store.schedules.retain(|x| x.id != id);
                self.store.save()?;
                Ok(json!({"schedules": self.store.schedules}))
            }
            "execute" => self.execute(input.get("plan_id").and_then(Value::as_u64).unwrap_or(0)),
            "forget" => {
                let site = Some(s("site")).filter(|x| !x.is_empty()).unwrap_or_else(|| SITES[0].0.to_string());
                if let Some(id) = creds::load_config().login_for(&site) {
                    creds::delete_password(&creds::account(&site, &id)).ok();
                }
                Ok(self.state())
            }
            "logout" => {
                if let Some(mut s) = self.session.take() {
                    s.logout();
                }
                self.papers.clear();
                self.reviews.clear();
                self.plans.clear();
                Ok(self.state())
            }
            _ => bail!("unknown request {name}"),
        }
    }

    /// Save a downloaded file in files_dir() and remember it.
    fn keep_file(&mut self, d: Download, fallback: &str, cmd: &str) -> Result<usize> {
        let ext = if d.mime.contains("pdf") || cmd.ends_with("pdf") { "pdf" } else { cmd.rsplit("__").next().filter(|e| e.len() <= 5 && *e != cmd).unwrap_or("bin") };
        let name = if d.name.is_empty() { format!("{fallback}.{ext}") } else { d.name.clone() };
        let name: String = name.chars().map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c }).collect();
        let dir = creds::files_dir().join(self.files.len().to_string());
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(&name);
        std::fs::write(&path, &d.bytes)?;
        let mime = if ext == "pdf" { "application/pdf".to_string() } else if d.mime.is_empty() { "application/octet-stream".into() } else { d.mime };
        self.files.push(Stored { name, mime, path });
        Ok(self.files.len() - 1)
    }

    fn paper(&self, id: &str) -> Result<Paper> {
        self.papers.iter().find(|p| same_id(&p.id, id)).cloned().with_context(|| format!("paper {id} is not loaded; reload the list"))
    }

    fn session_mut(&mut self) -> Result<&mut Session> {
        self.session.as_mut().context("not logged in")
    }

    /// Where the collapsed papers of the open journal are remembered.
    fn fold_key(&self) -> String {
        let s = self.session.as_ref();
        format!("{} {}", s.map(|x| x.site()).unwrap_or(""), s.and_then(|x| x.journal.as_ref()).map(|j| j.code.as_str()).unwrap_or(""))
    }

    fn state(&self) -> Value {
        let cfg = creds::load_config();
        let sites: Vec<Value> = SITES
            .iter()
            .map(|(host, name)| {
                let id = cfg.login_for(host).unwrap_or_default();
                let saved = !id.is_empty() && creds::get_password(&creds::account(host, &id)).is_some();
                json!({"host": host, "name": name, "login_id": id, "has_saved_password": saved})
            })
            .collect();
        let site = self.session.as_ref().map(|s| s.site().to_string()).or(cfg.site.clone()).unwrap_or_else(|| SITES[0].0.to_string());
        let mine = sites.iter().find(|x| x["host"] == site.as_str()).cloned().unwrap_or(json!({}));
        let (saved_id, has_saved_password) = (mine["login_id"].as_str().unwrap_or("").to_string(), mine["has_saved_password"].as_bool().unwrap_or(false));
        let prefs = json!({"invite_defaults": cfg.invite_defaults, "message_defaults": cfg.message_defaults, "table": cfg.table});
        match &self.session {
            None => json!({"logged_in": false, "site": site, "sites": sites, "login_id": saved_id, "has_saved_password": has_saved_password,
                           "saved_journal": cfg.journal, "saved_role": cfg.role_label, "saved_viewer": cfg.viewer,
                           "auto_login": cfg.auto_login, "prefs": prefs}),
            Some(s) => json!({
                "logged_in": true, "site": site, "sites": sites, "login_id": s.login_id(), "has_saved_password": has_saved_password, "viewer": s.viewer(),
                "journals": s.journals, "journal": s.journal, "roles": s.roles, "role": s.role,
                "saved_journal": cfg.journal, "saved_role": cfg.role_label, "prefs": prefs,
            }),
        }
    }

    fn plan(&mut self, rq: PlanRequest) -> Result<Value> {
        if rq.review_ids.is_empty() {
            bail!("select at least one review");
        }
        let mut reviews = Vec::new();
        for id in &rq.review_ids {
            let r = self.reviews.iter().find(|r| &r.review_id == id).with_context(|| format!("unknown review {id}; reload the list"))?;
            reviews.push(r.clone());
        }
        let act = match rq.action.as_str() {
            "contact" => {
                if rq.message.trim().is_empty() {
                    bail!("write a message");
                }
                let attachment = if rq.attachment_b64.is_empty() {
                    None
                } else {
                    let bytes = base64::engine::general_purpose::STANDARD.decode(rq.attachment_b64.as_bytes())?;
                    if bytes.len() > MAX_ATTACHMENT {
                        bail!("the attachment is larger than the site's 5 MB limit");
                    }
                    Some((rq.attachment_name.clone(), bytes))
                };
                Act::Contact {
                    cc: rq.cc.clone(),
                    opts: ContactOptions { do_not_archive: rq.do_not_archive, my_eyes_only: rq.my_eyes_only, do_not_copy_me: rq.do_not_copy_me },
                    attachment,
                }
            }
            "cancel" => Act::Cancel,
            "extend" => {
                if !EXTENSIONS.contains(&rq.days) {
                    bail!("choose one of the extensions the site offers");
                }
                Act::Extend { days: rq.days }
            }
            other => bail!("unknown action {other}"),
        };
        let contact = matches!(act, Act::Contact { .. });
        if contact {
            templates::check(&rq.subject)?;
            templates::check(&rq.message)?;
        }
        let mut items: Vec<PlanItem> = Vec::new();
        for r in reviews {
            let mut skip = match act {
                Act::Contact { .. } => None,
                _ if !r.is_open() => Some(format!("skipped: {}", r.status_raw)),
                _ => None,
            };
            let (mut subject, mut message) = (String::new(), String::new());
            if contact {
                match self.fill_for(&r, &rq.subject, &rq.message) {
                    Ok((s, m)) => (subject, message) = (s, m),
                    Err(e) => skip = Some(format!("{e:#}")),
                }
            }
            items.push(PlanItem { review: r, subject, message, skip });
        }
        let id = self.next_plan;
        self.next_plan += 1;
        let preview: Vec<Value> = items
            .iter()
            .map(|i| {
                json!({"review_id": i.review.review_id, "paper_id": i.review.paper_id, "reviewer": i.review.reviewer,
                       "pin": i.review.pin, "status": i.review.status_raw, "subject": i.subject, "message": i.message, "skip": i.skip})
            })
            .collect();
        self.plans.insert(id, Plan { act, items });
        Ok(json!({"plan_id": id, "items": preview}))
    }

    /// Subject and message with the placeholders filled in for review `r`.
    fn fill_for(&mut self, r: &Review, subject: &str, message: &str) -> Result<(String, String)> {
        templates::check(subject)?;
        templates::check(message)?;
        if templates::needs_details(&format!("{subject} {message}")) && !self.dates.contains_key(&r.review_id) {
            let session = self.session.as_mut().context("not logged in")?;
            let d = session.with_relogin(|x| x.review_dates(r)).context("could not read the review dates")?;
            self.dates.insert(r.review_id.clone(), d);
        }
        let journal = self.session.as_ref().and_then(|x| x.journal.as_ref()).map(|j| j.name.clone()).unwrap_or_default();
        let paper = self.papers.iter().find(|p| p.id == r.paper_id);
        let v = templates::values(r, paper, &journal, self.dates.get(&r.review_id), &Templates::load().signature, chrono::Local::now().date_naive());
        Ok((templates::fill(subject, &v), templates::fill(message, &v)))
    }

    fn execute(&mut self, plan_id: u64) -> Result<Value> {
        let plan = self.plans.remove(&plan_id).context("this preview has expired; preview again")?;
        let session = self.session.as_mut().context("not logged in")?;
        let mut results = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        for item in &plan.items {
            let r = &item.review;
            let outcome = if let Some(why) = &item.skip {
                Err(anyhow!("{why}"))
            } else {
                match &plan.act {
                    Act::Contact { cc, opts, attachment } => session.with_relogin(|s| {
                        s.contact_reviewer(r, &item.subject, cc, &item.message, opts, attachment.as_ref().map(|(n, b)| (n.as_str(), b.as_slice())))
                    }),
                    Act::Cancel => session.with_relogin(|s| s.cancel_review(r)),
                    Act::Extend { days } => session.with_relogin(|s| s.extend_deadline(r, *days)),
                }
            };
            if !touched.contains(&r.paper_id) {
                touched.push(r.paper_id.clone());
            }
            results.push(match outcome {
                Ok(m) => json!({"review_id": r.review_id, "reviewer": r.reviewer, "paper_id": r.paper_id, "ok": true, "message": m}),
                Err(e) => json!({"review_id": r.review_id, "reviewer": r.reviewer, "paper_id": r.paper_id, "ok": false, "message": format!("{e:#}")}),
            });
        }
        // Refresh the papers that were touched so the table shows the server's state.
        for pid in touched {
            if let Some(p) = self.papers.iter().find(|p| p.id == pid).cloned() {
                if let Ok(fresh) = session.with_relogin(|s| s.load_reviews(&p)) {
                    self.reviews.retain(|r| r.paper_id != pid);
                    self.reviews.extend(fresh);
                }
            }
        }
        Ok(json!({"results": results, "reviews": self.reviews}))
    }
}


/// Open a file in Preview, show it in the Finder, or copy it to ~/Downloads.
pub fn reveal(path: &std::path::Path, how: &str) -> Result<Value> {
    let path = if how == "downloads" {
        let to = unique_download(&path.file_name().context("no file name")?.to_string_lossy())?;
        std::fs::copy(path, &to)?;
        to
    } else {
        path.to_path_buf()
    };
    if !cfg!(test) {
        let mut c = std::process::Command::new("open");
        match how {
            "preview" => c.args(["-a", "Preview"]).arg(&path),
            "finder" | "downloads" => c.arg("-R").arg(&path),
            _ => c.arg(&path),
        };
        c.spawn().context("cannot run 'open'")?;
    }
    Ok(json!({"path": path}))
}

/// A free name in ~/Downloads ("name.pdf", "name 2.pdf", …).
fn unique_download(name: &str) -> Result<PathBuf> {
    let name: String = name.chars().map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c }).collect();
    if name.is_empty() {
        bail!("no file name");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).context("no home folder")?;
    let dir = if cfg!(test) { creds::files_dir() } else { home.join("Downloads") };
    std::fs::create_dir_all(&dir)?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((a, b)) if !a.is_empty() => (a.to_string(), format!(".{b}")),
        _ => (name.clone(), String::new()),
    };
    (1..1000).map(|i| dir.join(if i == 1 { format!("{stem}{ext}") } else { format!("{stem} {i}{ext}") })).find(|p| !p.exists()).context("no free file name")
}

fn html(body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_header("Content-Type: text/html; charset=utf-8".parse::<Header>().unwrap())
        .with_header("Cache-Control: no-store".parse::<Header>().unwrap())
        .with_header("Content-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'".parse::<Header>().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same API calls the page makes, against the replay server (see replay_test.rs).
    #[test]
    #[ignore]
    fn ui_replay() {
        use crate::replay_test::{x, xn};
        let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
        let _guard = creds::folder_lock();
        let (base, seen) = crate::replay_test::serve(dir);
        let cfg = std::env::temp_dir().join(format!("docket-test-{}", std::process::id()));
        unsafe {
            std::env::set_var("DOCKET_PORTAL", format!("{base}/conferences/scripts/"));
            std::env::set_var("DOCKET_CONFIG_DIR", &cfg);
        }
        let mut app = App::new(None, "t".into(), 0);
        let st = app.api("login", json!({"login_id": "someone@example.org", "password": "x"})).unwrap();
        assert_eq!(st["logged_in"], true);
        let st = app.api("journal", json!({"code": "tac"})).unwrap();
        assert_eq!(st["roles"][0]["label"], "Associate Editor");
        assert_eq!(st["roles"][1]["kind"], "reviewer");

        app.api("role", json!({"command": "AccessReviewerspace"})).unwrap();
        let mine = app.api("load", json!({"include_all": true})).unwrap();
        assert_eq!(mine["mode"], "reviewer");
        assert_eq!(mine["assignments"].as_array().unwrap().len(), xn("reviewer.items"));
        assert!(app.api("plan", json!({"action": "cancel", "review_ids": [x("reviewer.canceled.id")]})).is_err(), "no actions on own reviews");

        // Switching back to the editor role from the reviewer workspace.
        app.api("role", json!({"command": "Access_597_4"})).unwrap();
        let loaded = app.api("load", json!({})).unwrap();
        assert_eq!(loaded["papers"].as_array().unwrap().len(), xn("papers"));
        // The replay only has the reviews pages of two papers; the others must be reported, not abort the load.
        assert_eq!(loaded["reviews"].as_array().unwrap().len(), xn("ui.reviews"));
        assert_eq!(loaded["warnings"].as_array().unwrap().len(), xn("ui.warnings"), "{}", loaded["warnings"]);
        // The thank-you notes on the site's list, counted for the menus.
        let counts = loaded["unthanked"].as_array().map(|a| a.iter().map(|n| n.as_u64().unwrap()).collect::<Vec<_>>()).unwrap();
        assert!(counts[0] >= 1, "{counts:?}");
        assert_eq!(app.api("automation_status", json!({})).unwrap()["unthanked"], loaded["unthanked"]);
        let offer = app.api("site_offer", json!({"kind": "review"})).unwrap();
        assert_eq!(offer["rows"].as_array().unwrap().len() as u64, counts[1]);

        // Refreshing one paper re-reads only its reviews and keeps the others.
        let of = |j: &Value, pid: &str| j["reviews"].as_array().unwrap().iter().filter(|r| r["paper_id"] == pid).count();
        let before = seen.lock().unwrap().len();
        let fresh = app.api("refresh_paper", json!({"paper_id": x("paper.id")})).unwrap();
        assert_eq!(fresh["reviews"].as_array().unwrap().len(), xn("ui.reviews"));
        assert_eq!(of(&fresh, &x("paper.id")), of(&loaded, &x("paper.id")));
        assert!(of(&fresh, &x("paper.id")) > 0);
        assert_eq!(fresh["reviews_rev"], 1);
        assert_eq!(app.api("automation_status", json!({})).unwrap()["reviews_rev"], 1);
        assert!(seen.lock().unwrap().len() > before, "read from the site");
        assert!(app.api("refresh_paper", json!({"paper_id": "no-such-paper"})).is_err());

        // PaperPlaza's letters as message templates, with Docket's placeholders.
        let letters = app.api("site_letters", json!({})).unwrap();
        let names: Vec<&str> = letters["templates"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["PaperPlaza: thanks for agreeing to review", "PaperPlaza: thanks for the review", "PaperPlaza: confirmation request", "PaperPlaza: review reminder"]);
        for t in letters["templates"].as_array().unwrap() {
            let body = t["body"].as_str().unwrap();
            assert!(body.starts_with("Dear %reviewername,") && !body.contains("%YOUR_") && !body.contains('\r'), "{body}");
            crate::templates::check(body).unwrap();
        }
        // What the site's thank-you page offers, with its letter.
        let offer = app.api("site_offer", json!({"kind": "accept"})).unwrap();
        assert!(offer["rows"].as_array().unwrap().iter().any(|r| r["review_id"] == x("thanks.review_id")));
        assert!(offer["letter"].as_str().unwrap().starts_with("Dear %YOUR_TITLE%"));
        // A template chosen as a letter must exist.
        crate::templates::Templates::defaults().save().unwrap();
        app.api("notes_save", json!({"thanks": {"accept": {"on": true}, "accept_letter": "Thank you for your review"}})).unwrap();
        assert!(app.api("notes_save", json!({"thanks": {"accept": {"on": true}, "accept_letter": "No such template"}})).is_err());

        // "PIN from" keeps only higher (more recent) PINs.
        let (paper, min) = (x("paper.id"), x("ui.candidates_min_pin"));
        let floor: u64 = min.parse().unwrap();
        let all = app.api("candidates", json!({"paper_id": paper, "limit": 10000})).unwrap();
        let young = app.api("candidates", json!({"paper_id": paper, "min_pin": min, "limit": 10000})).unwrap();
        let pins = |j: &Value| j["candidates"].as_array().unwrap().iter().map(|c| c["pin"].as_str().unwrap().parse::<u64>().unwrap()).collect::<Vec<_>>();
        let expect = pins(&all).into_iter().filter(|&p| p >= floor).count();
        assert!(expect > 0 && expect < pins(&all).len());
        assert!(pins(&young).iter().all(|&p| p >= floor));
        assert_eq!((young["matching"].as_u64(), pins(&young).len()), (Some(expect as u64), expect));
        let both = app.api("candidates", json!({"paper_id": paper, "min_pin": min, "q": x("paper.authors.affiliation_word")})).unwrap();
        assert!(pins(&both).iter().all(|&p| p >= floor));
        // Title filter: only the chosen kinds, and all of them.
        let group = |c: &Value| crate::model::title_group(c["title"].as_str().unwrap());
        let seniors = app.api("candidates", json!({"paper_id": paper, "titles": ["prof", "dr"], "limit": 10000})).unwrap();
        let list = seniors["candidates"].as_array().unwrap();
        let expect = all["candidates"].as_array().unwrap().iter().filter(|c| ["prof", "dr"].contains(&group(c))).count();
        assert!(expect > 0 && list.len() == expect && list.iter().all(|c| ["prof", "dr"].contains(&group(c))));
        assert_eq!(seniors["matching"].as_u64(), Some(expect as u64));

        let (canceled, received, open) = (x("paper.canceled_review.id"), x("paper.received_review.id"), x("paper.open_review"));
        let plan = app
            .api("plan", json!({"action": "contact", "review_ids": [canceled], "subject": "IEEE-TAC {paper}: your review",
                                "message": "Dear {first},\nplease send your review of “{title}” by {due}.", "do_not_copy_me": true}))
            .unwrap();
        let item = &plan["items"][0];
        assert_eq!(item["subject"].as_str().unwrap(), format!("IEEE-TAC {paper}: your review"));
        let start = format!("Dear {},\nplease send your review of “{}", x("paper.canceled_review.first_name"), x("paper.title_start"));
        assert!(item["message"].as_str().unwrap().starts_with(&start), "{}", item["message"]);
        // Templates and placeholders: the live preview reads the review dates once.
        let t = app.api("templates", json!({})).unwrap();
        assert!(t["list"].as_array().unwrap().iter().any(|x| x["name"] == "Reminder: review overdue"));
        app.api("templates_save", json!({"signature": "Jane Doe", "list": t["list"]})).unwrap();
        let f = app
            .api("fill", json!({"review_id": received, "subject": "%journal %paper: thank you",
                                "message": "Dear %reviewerfirstname, received on %receiveddate (accepted %accepteddate, asked %requesteddate).\n%signature"}))
            .unwrap();
        assert_eq!(f["subject"].as_str().unwrap(), format!("IEEE-TAC {paper}.1: thank you"));
        assert_eq!(f["message"].as_str().unwrap(), format!("Dear {}, {}.\nJane Doe", x("paper.received_review.first_name"), x("paper.received_review.dates")));
        assert!(app.api("fill", json!({"review_id": received, "subject": "", "message": "Dear %reviwer"})).is_err(), "typos are refused");
        assert!(app.api("templates_save", json!({"signature": "", "list": [{"name": "x", "subject": "", "body": "%nope"}]})).is_err());

        let before = seen.lock().unwrap().len();
        let done = app.api("execute", json!({"plan_id": plan["plan_id"]})).unwrap();
        assert_eq!(done["results"][0]["ok"], true, "{}", done["results"][0]);
        assert!(seen.lock().unwrap()[before..].iter().any(|x| x.path.ends_with("contactreviewer.pl")));
        assert!(app.api("execute", json!({"plan_id": plan["plan_id"]})).is_err(), "a plan runs only once");

        let plan = app.api("plan", json!({"action": "cancel", "review_ids": [canceled, open]})).unwrap();
        assert!(plan["items"][0]["skip"].as_str().unwrap().contains("Canceled"));
        assert!(plan["items"][1]["skip"].is_null());
        let done = app.api("execute", json!({"plan_id": plan["plan_id"]})).unwrap();
        assert_eq!(done["results"][0]["ok"], false);
        assert_eq!(done["results"][1]["ok"], false, "replay serves the wrong details page, so the safety check must refuse");

        let plan = app.api("plan", json!({"action": "extend", "days": 14, "review_ids": [open]})).unwrap();
        let done = app.api("execute", json!({"plan_id": plan["plan_id"]})).unwrap();
        assert_eq!(done["results"][0]["message"], "New due date: October 13, 2026");
        assert!(app.api("plan", json!({"action": "extend", "days": 5, "review_ids": [open]})).is_err());
        let _ = std::fs::remove_dir_all(cfg);
    }
}

/// What a schedule has done so far (its last check time left out).
fn fingerprint(s: &scheduler::Schedule) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("last_check");
    }
    v
}
