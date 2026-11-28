//! Navigation and actions on the PaperPlaza journal workspace, as observed on
//! the IEEE-TAC Associate Editor pages (workspace.pl, reviews.pl,
//! contactreviewer.pl, reviews_aja.pl) and Reviewer pages (reviewerspace.pl).

use crate::http::{Download, Fetched, Http};
use crate::model::*;
use crate::page::{Page, cells, commands_in, element_text, find_table, sel};
use anyhow::{Context, Result, anyhow, bail};
use chrono::NaiveDate;
use regex::Regex;
use scraper::ElementRef;
use std::path::PathBuf;
use std::sync::LazyLock;

static PREVIOUS_SUBMISSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Reviews for .*Previous Submission (\S+)").unwrap());

/// Sites running the same PaperCept system: host and name.
pub const SITES: [(&str, &str); 3] = [("css.paperplaza.net", "IEEE CSS PaperPlaza"), ("ifac-sms.org", "IFAC"), ("controls.papercept.net", "PaperCept (controls)")];

/// Links on the sites are sometimes http://; the sites are served over https.
fn https(u: &str) -> String {
    for (host, _) in SITES {
        if let Some(rest) = u.strip_prefix(&format!("http://{host}/")) {
            return format!("https://{host}/{rest}");
        }
    }
    u.to_string()
}

static JOURNAL_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/journals/([A-Za-z0-9_-]+)/scripts/login\.pl\?PIN=").unwrap());
static ROLE_CMD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Access_\d+_\d+$").unwrap());
static PAGE_OF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Page (\d+) of (\d+)").unwrap());
static NAME_PIN_VALUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(.*?)\s*\((\d+)\)[^\p{L}\p{N}()]*$").unwrap());
static DATE_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Z][a-z]+ \d{1,2}, \d{4}").unwrap());
static ID_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(ID ([^)]+)\)").unwrap());

/// The start of a site. Debug builds can be pointed at a local test server
/// with DOCKET_PORTAL. Tests never use the real sites.
fn portal_of(site: &str) -> String {
    if cfg!(debug_assertions) || cfg!(test) {
        if let Ok(p) = std::env::var("DOCKET_PORTAL") {
            return p;
        }
    }
    if cfg!(test) {
        return "http://127.0.0.1:9/conferences/scripts/".to_string();
    }
    format!("https://{site}/conferences/scripts/")
}

pub const EXTENSIONS: [u32; 7] = [1, 3, 7, 10, 14, 21, 28];

/// The fewest extensions the site offers that add up to `days`, largest
/// first (18 → 14 + 3 + 1). Empty for 0 or less.
pub fn extension_steps(days: i64) -> Vec<u32> {
    let n = days.max(0) as usize;
    // best[d] = (steps, first step) for d days.
    let mut best: Vec<(usize, u32)> = vec![(0, 0); n + 1];
    for d in 1..=n {
        best[d] = EXTENSIONS
            .iter()
            .rev()
            .filter(|&&e| e as usize <= d)
            .map(|&e| (best[d - e as usize].0 + 1, e))
            .min_by_key(|&(k, _)| k)
            .unwrap_or((usize::MAX, 0));
    }
    let (mut out, mut d) = (Vec::new(), n);
    while d > 0 {
        out.push(best[d].1);
        d -= best[d].1 as usize;
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    out
}

/// The server returned a login form: the 60-minute session timed out.
#[derive(Debug)]
pub struct SessionExpired;
impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the PaperPlaza session expired")
    }
}
impl std::error::Error for SessionExpired {}

#[derive(Clone, Default)]
pub struct ContactOptions {
    pub do_not_archive: bool,
    pub my_eyes_only: bool,
    pub do_not_copy_me: bool,
}

pub struct Session {
    http: Http,
    site: String,
    portal: String,
    page: Option<Page>,
    login_id: String,
    password: String,
    pub journals: Vec<Journal>,
    pub journal: Option<Journal>,
    pub roles: Vec<Role>,
    pub role: Option<Role>,
}

impl Session {
    /// `viewer`: viewer mode, in which only page-viewing requests are sent.
    pub fn new(trace_dir: Option<PathBuf>, viewer: bool) -> Result<Self> {
        Ok(Session {
            http: Http::new(trace_dir, viewer)?,
            site: SITES[0].0.to_string(),
            portal: portal_of(SITES[0].0),
            page: None,
            login_id: String::new(),
            password: String::new(),
            journals: Vec::new(),
            journal: None,
            roles: Vec::new(),
            role: None,
        })
    }

    /// Use another portal base URL (tests replay saved pages from a local server).
    #[cfg(test)]
    pub fn with_portal(mut self, portal: &str) -> Self {
        self.portal = portal.to_string();
        self
    }

    /// Work on another of the SITES (before logging in).
    pub fn for_site(mut self, site: &str) -> Result<Self> {
        if !SITES.iter().any(|(h, _)| *h == site) {
            bail!("unknown site '{site}'");
        }
        self.site = site.to_string();
        self.portal = portal_of(site);
        Ok(self)
    }

    pub fn site(&self) -> &str {
        &self.site
    }

    pub fn login_id(&self) -> &str {
        &self.login_id
    }

    pub fn viewer(&self) -> bool {
        self.http.viewer
    }

    /// Open the journal's editor workspace unless it is already the current one.
    pub fn ensure_editor(&mut self, journal: &str) -> Result<()> {
        let here = self.journal.as_ref().is_some_and(|j| j.code.eq_ignore_ascii_case(journal))
            && self.role.as_ref().is_some_and(|r| r.kind == RoleKind::Editor);
        if here {
            return Ok(());
        }
        self.enter_journal(journal)?;
        let role = self.roles.iter().find(|r| r.kind == RoleKind::Editor).cloned();
        let role = role.ok_or_else(|| anyhow!("you have no editor role in {journal}"))?;
        self.enter_role(&role.command)
    }

    /// Return to a journal and role saved before (e.g. after a scheduled run).
    pub fn restore(&mut self, journal: Option<Journal>, role: Option<Role>) -> Result<()> {
        let same = self.journal.as_ref().map(|j| &j.code) == journal.as_ref().map(|j| &j.code)
            && self.role.as_ref().map(|r| &r.command) == role.as_ref().map(|r| &r.command);
        if same {
            return Ok(());
        }
        if let Some(j) = journal {
            self.enter_journal(&j.code)?;
            if let Some(r) = role {
                self.enter_role(&r.command)?;
            }
        }
        Ok(())
    }

    /// Log out on the server so the session token stops working.
    pub fn logout(&mut self) {
        if self.page.as_ref().is_some_and(|p| p.has_command("Logout")) {
            let _ = self.cmd("Logout");
        }
        self.page = None;
        self.password.clear();
    }

    // -- login and role selection ------------------------------------------

    pub fn login(&mut self, login_id: &str, password: &str) -> Result<()> {
        let start = self.http.get(&format!("{}start.pl", self.portal), None)?;
        let form = start.form().context("the start page has no form")?.clone();
        let login_page = self.http.submit(&start, &form, Some("Login"))?;
        let mut form = login_page
            .forms
            .iter()
            .find(|f| f.get("LoginId").is_some())
            .context("the login page has no login form")?
            .clone();
        form.set("LoginId", login_id);
        form.set("Password", password);
        let access = self.http.submit(&login_page, &form, Some("Login"))?;
        if access.is_login_form() || !access.is_logged_in() {
            bail!("login failed{}", server_message(&access).map(|m| format!(": {m}")).unwrap_or_default());
        }
        self.login_id = login_id.to_string();
        self.password = password.to_string();
        self.journals = parse_journals(&access);
        self.page = Some(access);
        Ok(())
    }

    pub fn enter_journal(&mut self, code: &str) -> Result<()> {
        let j = self
            .journals
            .iter()
            .find(|j| j.code.eq_ignore_ascii_case(code))
            .cloned()
            .ok_or_else(|| anyhow!("journal '{code}' is not on your Access page"))?;
        let referer = self.page.as_ref().map(|p| p.url.clone());
        let p = self.http.get(&j.url, referer.as_deref())?;
        if p.is_login_form() {
            return Err(SessionExpired.into());
        }
        let mut roles: Vec<Role> = Vec::new();
        for (label, command) in p.commands() {
            let kind = if ROLE_CMD.is_match(&command) {
                RoleKind::Editor
            } else if command == "AccessReviewerspace" {
                RoleKind::Reviewer
            } else {
                continue;
            };
            if !roles.iter().any(|r| r.command == command) {
                roles.push(Role { label, command, kind });
            }
        }
        self.roles = roles;
        self.journal = Some(j);
        self.role = None;
        self.page = Some(p);
        Ok(())
    }

    pub fn enter_role(&mut self, command: &str) -> Result<()> {
        let role = self
            .roles
            .iter()
            .find(|r| r.command == command)
            .cloned()
            .ok_or_else(|| anyhow!("role {command} is not offered on the journal Access page"))?;
        if !self.page.as_ref().is_some_and(|p| p.has_command(command)) {
            let j = self.journal.clone().context("no journal is open")?;
            self.enter_journal(&j.code)?;
        }
        self.cmd(command)?;
        let expected = match role.kind {
            RoleKind::Editor => "/workspace.pl",
            RoleKind::Reviewer => "/reviewerspace.pl",
        };
        if !self.current()?.form().is_some_and(|f| f.action.ends_with(expected)) {
            bail!("entering '{}' did not open a workspace (got '{}')", role.label, self.current()?.title);
        }
        self.role = Some(role);
        Ok(())
    }

    /// Log in again after a timeout and return to the same journal and role.
    pub fn relogin(&mut self) -> Result<()> {
        let (id, pw) = (self.login_id.clone(), self.password.clone());
        let journal = self.journal.clone();
        let role = self.role.clone();
        self.login(&id, &pw)?;
        if let Some(j) = journal {
            self.enter_journal(&j.code)?;
            if let Some(r) = role {
                self.enter_role(&r.command)?;
            }
        }
        Ok(())
    }

    /// Run `op`; if the session expired, log in again and retry once.
    pub fn with_relogin<T>(&mut self, mut op: impl FnMut(&mut Session) -> Result<T>) -> Result<T> {
        match op(self) {
            Err(e) if e.downcast_ref::<SessionExpired>().is_some() && !self.password.is_empty() => {
                self.relogin()?;
                op(self)
            }
            r => r,
        }
    }

    // -- navigation --------------------------------------------------------

    fn current(&self) -> Result<&Page> {
        self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))
    }

    /// Click `command(cmd)` on the current page.
    fn cmd(&mut self, cmd: &str) -> Result<()> {
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        if !page.has_command(cmd) {
            bail!("'{cmd}' is not available on the page '{}'", page.title);
        }
        let form = page.form().context("the page has no form")?.clone();
        let next = self.http.submit(page, &form, Some(cmd))?;
        if next.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        self.page = Some(next);
        Ok(())
    }

    fn goto_workspace(&mut self) -> Result<()> {
        let has_menu = self.page.as_ref().is_some_and(|p| p.has_command("Workspace"));
        if has_menu {
            self.cmd("Workspace")?;
        } else {
            // Unexpected page without the menu: re-enter through the journal Access page.
            let (journal, role) = (self.journal.clone(), self.role.clone());
            let (Some(j), Some(r)) = (journal, role) else { bail!("no journal workspace is open") };
            self.enter_journal(&j.code)?;
            self.enter_role(&r.command)?;
        }
        if !self.current()?.form().is_some_and(|f| f.action.ends_with("workspace.pl")) {
            bail!("could not open the workspace (got '{}')", self.current()?.title);
        }
        Ok(())
    }

    fn goto_reviews(&mut self, paper_id: &str, reviews_cmd: &str) -> Result<()> {
        let here = self.current()?;
        let on_paper = here.hidden("Submission_ID").is_some_and(|s| same_id(&s, paper_id));
        if on_paper && here.has_command("Reviews") {
            self.cmd("Reviews")?;
        } else {
            self.goto_workspace()?;
            self.cmd(reviews_cmd)?;
        }
        let p = self.current()?;
        if !p.hidden("Submission_ID").is_some_and(|s| same_id(&s, paper_id)) {
            bail!("opened '{}' but it is not the reviews page of {paper_id}", p.title);
        }
        Ok(())
    }

    fn goto_review_details(&mut self, r: &Review) -> Result<()> {
        self.goto_reviews(&r.paper_id, &r.reviews_cmd)?;
        self.cmd(&r.details_cmd)?;
        let p = self.current()?;
        if p.hidden("Review_ID").as_deref() != Some(r.review_id.as_str()) {
            bail!("opened '{}' but it is not review {}", p.title, r.review_id);
        }
        Ok(())
    }

    // -- reading -----------------------------------------------------------

    pub fn load_papers(&mut self, include_all: bool) -> Result<Vec<Paper>> {
        self.goto_workspace()?;
        // The site keeps the workspace's settings for the session or the user
        // ("Show all", "Show tagged submissions only", the search, the page
        // last shown), also when changed on the website or by an automatic
        // check, so they are set here before reading, or papers would be missing.
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().context("workspace has no form")?.clone();
        let on = |f: &crate::page::Form, n: &str| f.get(n).is_some_and(|c| c.checked);
        let wrong = (form.get("ShowAll").is_some() && on(&form, "ShowAll") != include_all)
            || on(&form, "ShowTaggedOnly")
            || form.value("SearchFor").is_some_and(|v| !v.trim().is_empty())
            || form.value("Page").is_some_and(|v| v.trim() != "1");
        if wrong {
            form.set_checked("ShowAll", include_all);
            form.set_checked("ShowTaggedOnly", false);
            form.set("SearchFor", "");
            form.set("Page", "1");
            if let Some(c) = form.controls.iter_mut().find(|c| c.name == "Order_by").filter(|_| include_all) {
                for (i, o) in c.options.iter_mut().enumerate() {
                    o.selected = i == 0;
                }
            }
            let next = self.http.submit(page, &form, None)?;
            if next.is_login_form() {
                return Err(SessionExpired.into());
            }
            self.page = Some(next);
        }
        if page_of(self.current()?).is_some_and(|(cur, _)| cur > 1) {
            self.cmd("First")?;
            if let Some((cur, _)) = page_of(self.current()?).filter(|(cur, _)| *cur > 1) {
                bail!("the workspace stays on page {cur} instead of the first one");
            }
        }
        let mut papers = parse_workspace(self.current()?);
        let mut guard = 0;
        while let Some((cur, total)) = page_of(self.current()?) {
            if cur >= total || guard > 50 {
                break;
            }
            guard += 1;
            self.cmd("Next")?;
            if page_of(self.current()?).is_some_and(|(next, _)| next <= cur) {
                break;
            }
            papers.extend(parse_workspace(self.current()?));
        }
        // A paper listed twice (e.g. on two pages while the list changed) counts once.
        let mut seen = std::collections::HashSet::new();
        papers.retain(|p| seen.insert(p.id.clone()));
        Ok(papers)
    }

    /// The reviews requested from you, from the Reviewer's workspace.
    /// `include_all` ticks "Show all items" (past and canceled reviews too).
    pub fn load_assignments(&mut self, include_all: bool) -> Result<Vec<Assignment>> {
        let (Some(j), Some(r)) = (self.journal.clone(), self.role.clone()) else { bail!("no workspace is open") };
        if r.kind != RoleKind::Reviewer {
            bail!("'{}' is not a reviewer workspace", r.label);
        }
        self.enter_journal(&j.code)?;
        self.enter_role(&r.command)?;
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().context("the reviewer workspace has no form")?.clone();
        let shows_all = form.get("ShowAll").is_some_and(|c| c.checked);
        if shows_all != include_all && form.set_checked("ShowAll", include_all) {
            let next = self.http.submit(page, &form, None)?;
            if next.is_login_form() {
                self.page = None;
                return Err(SessionExpired.into());
            }
            self.page = Some(next);
        }
        let mut out = parse_assignments(self.current()?, &j.code);
        let mut guard = 0;
        while let Some((cur, total)) = page_of(self.current()?) {
            if cur >= total || guard > 50 || !self.current()?.has_command("Next") {
                break;
            }
            guard += 1;
            self.cmd("Next")?;
            let before = out.len();
            for a in parse_assignments(self.current()?, &j.code) {
                if !out.iter().any(|x| x.review_id == a.review_id) {
                    out.push(a);
                }
            }
            // "All" rows per page still reports two pages; stop when nothing new comes.
            if out.len() == before {
                break;
            }
        }
        Ok(out)
    }

    pub fn load_reviews(&mut self, paper: &Paper) -> Result<Vec<Review>> {
        self.goto_reviews(&paper.id, &paper.reviews_cmd)?;
        Ok(parse_reviews(self.current()?, &paper.id, &paper.title, &paper.reviews_cmd))
    }

    fn fresh_review(&mut self, r: &Review) -> Result<Review> {
        self.goto_reviews(&r.paper_id, &r.reviews_cmd)?;
        parse_reviews(self.current()?, &r.paper_id, &r.paper_title, &r.reviews_cmd)
            .into_iter()
            .find(|x| x.review_id == r.review_id)
            .ok_or_else(|| anyhow!("review {} is no longer listed for {}", r.review_id, r.paper_id))
    }

    /// Your Reviewers list as shown on the paper's "Request a Review" page,
    /// with the paper's keywords that each reviewer lists.
    pub fn load_candidates(&mut self, paper: &Paper) -> Result<(PaperInfo, Vec<Candidate>)> {
        let (abstract_text, keywords) = self.load_abstract(paper).unwrap_or_default();
        self.open_request_review(paper)?;
        let doc = self.current()?.doc();
        let mut info = paper_info_in(&doc);
        let mut cands = candidates_in(&doc);
        drop(doc);
        info.abstract_text = abstract_text;
        if !keywords.is_empty() {
            info.keywords = keywords;
        }
        crate::recommend::rank(&info, &mut cands);
        Ok((info, cands))
    }

    /// Open the paper's "Request a Review" page (your whole Reviewers list).
    fn open_request_review(&mut self, paper: &Paper) -> Result<()> {
        self.goto_reviews(&paper.id, &paper.reviews_cmd)?;
        self.cmd("RequestReview")?;
        let p = self.current()?;
        let for_paper = match p.hidden("Submission_ID") {
            Some(s) => same_id(&s, &paper.id),
            None => p.text().contains(&format!("Submission {}", paper.id)),
        };
        if !p.form().is_some_and(|f| f.action.ends_with("/requestreview.pl")) || !for_paper {
            bail!("opened '{}' instead of the Request a Review page of {}", p.title, paper.id);
        }
        Ok(())
    }

    /// Abstract and keywords from the paper's Details page ("Database Entry").
    pub fn load_abstract(&mut self, paper: &Paper) -> Result<(String, Vec<String>)> {
        self.goto_reviews(&paper.id, &paper.reviews_cmd)?;
        self.cmd("Details")?;
        let p = self.current()?;
        if !p.title.starts_with("Database Entry") || !p.hidden("Submission_ID").is_some_and(|s| same_id(&s, &paper.id)) {
            bail!("opened '{}' instead of the details of {}", p.title, paper.id);
        }
        Ok(parse_abstract(p))
    }

    /// Open "Add PINs to Your Reviewers List" (pins.pl with AddReviewers=ON)
    /// through the link on your Reviewers page.
    fn open_pin_search(&mut self) -> Result<()> {
        let here = self.page.as_ref().is_some_and(|p| p.form().is_some_and(|f| f.action.ends_with("/pins.pl")) && p.hidden("AddReviewers").as_deref() == Some("ON"));
        if here {
            return Ok(());
        }
        self.goto_workspace()?;
        self.cmd("Reviewers")?;
        let p = self.current()?;
        let url = add_pins_link(p).ok_or_else(|| {
            anyhow!("could not find the link to “Add PINs to Your Reviewers List” on the '{}' page (that page has not been captured yet)", p.title)
        })?;
        let referer = p.url.clone();
        let next = self.http.get(&url, Some(&referer))?;
        if next.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        if !next.form().is_some_and(|f| f.action.ends_with("/pins.pl")) || next.hidden("AddReviewers").as_deref() != Some("ON") {
            bail!("the Reviewers page link opened '{}', not the Add PINs search", next.title);
        }
        self.page = Some(next);
        Ok(())
    }

    /// Search the whole PIN database (as "Add PINs to Your Reviewers List"
    /// does). `field` is one of the "Search in" values, e.g. "Keywords",
    /// "Surname", "Email_address", "PIN". Reads at most `max_pages` pages of 200.
    pub fn search_pins(&mut self, field: &str, query: &str, max_pages: u32) -> Result<(Vec<Candidate>, Option<u32>)> {
        if query.trim().is_empty() {
            bail!("type something to search for");
        }
        self.open_pin_search()?;
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().context("the PIN search page has no form")?.clone();
        if !form.select("SearchIn", field) {
            bail!("'{field}' is not a search field on this page");
        }
        form.set("SearchFor", query);
        form.set_checked("IncludeKeywords", true);
        form.select("PageviewRows", "200");
        let next = self.http.submit(page, &form, Some("Search"))?;
        if next.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        self.page = Some(next);
        let mut out = parse_pin_results(self.current()?);
        let mut pages = page_of(self.current()?).map(|(_, t)| t);
        let mut read = 1;
        while let Some((cur, total)) = page_of(self.current()?) {
            if cur >= total || read >= max_pages {
                break;
            }
            read += 1;
            self.cmd("Next")?;
            let before = out.len();
            for c in parse_pin_results(self.current()?) {
                if !out.iter().any(|x| x.pin == c.pin) {
                    out.push(c);
                }
            }
            if out.len() == before {
                break;
            }
        }
        if pages.is_some_and(|t| t <= read) {
            pages = None;
        }
        Ok((out, pages))
    }

    /// Add people to your Reviewers list, one PIN search each, as ticking them
    /// in the results and clicking "Add to list" does.
    pub fn add_to_list(&mut self, pin: &str) -> Result<String> {
        if self.viewer() {
            return Err(crate::http::ViewerMode("Add to list".into()).into());
        }
        let (found, _) = self.search_pins("PIN", pin, 1)?;
        let Some(c) = found.iter().find(|c| c.pin == pin) else { bail!("PIN {pin} was not found, or is inactive") };
        if c.in_list {
            return Ok(format!("{} is already on your Reviewers list", c.name));
        }
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().context("the results page has no form")?.clone();
        let boxname = format!("AddToReviewers{pin}");
        if !form.set_checked(&boxname, true) {
            bail!("{} cannot be added from the results", c.name);
        }
        let next = self.http.submit_clicking(page, &form, "SubmitAddToReviewersList")?;
        if next.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        let ok = next.text().contains(&format!("({pin}) was successfully added"));
        let name = c.name.clone();
        self.page = Some(next);
        if ok {
            Ok(format!("{name} was added to your Reviewers list"))
        } else {
            bail!("the site did not confirm adding {name}{}", server_message(self.current()?).map(|m| format!(": {m}")).unwrap_or_default())
        }
    }

    /// Invite reviewers (PINs) to review `paper`, as Request a Review does:
    /// tick them, "Next", then "Send" with the site's cover message. People
    /// not on your Reviewers list are added to it first. Each invitation is
    /// confirmed by finding the reviewer on the paper's Manage Reviews page.
    pub fn request_reviews(&mut self, paper: &Paper, pins: &[String], opts: &InviteOptions) -> Result<Vec<InviteResult>> {
        if self.viewer() {
            return Err(crate::http::ViewerMode("Request a Review".into()).into());
        }
        let mut pins: Vec<String> = pins.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        pins.dedup();
        if pins.is_empty() {
            bail!("no reviewers to invite");
        }
        let mut out: Vec<InviteResult> = Vec::new();
        let fail = |out: &mut Vec<InviteResult>, pin: &str, name: &str, msg: String| {
            out.push(InviteResult { pin: pin.to_string(), name: name.to_string(), ok: false, message: msg })
        };

        // Who is missing from the Reviewers list: add them, then reopen the page.
        self.open_request_review(paper)?;
        let missing: Vec<String> = {
            let form = self.current()?.form().context("the Request a Review page has no form")?;
            pins.iter().filter(|p| form.get(&format!("Include{p}")).is_none()).cloned().collect()
        };
        if !missing.is_empty() {
            for pin in &missing {
                if let Err(e) = self.add_to_list(pin) {
                    fail(&mut out, pin, "", format!("not invited: could not add PIN {pin} to your Reviewers list: {e:#}"));
                }
            }
            self.open_request_review(paper)?;
        }

        // Tick the ones that can be requested.
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let form = page.form().context("the Request a Review page has no form")?;
        let mut ok_pins = Vec::new();
        let mut names = std::collections::HashMap::new();
        for pin in &pins {
            if out.iter().any(|r| &r.pin == pin) {
                continue;
            }
            match form.get(&format!("Include{pin}")) {
                None => fail(&mut out, pin, "", format!("not invited: PIN {pin} is still not on your Reviewers list")),
                Some(c) => {
                    let name = NAME_PIN_VALUE.captures(&c.value).map(|m| m[1].trim().to_string()).unwrap_or_else(|| c.value.trim().to_string());
                    if c.disabled {
                        fail(&mut out, pin, &name, format!("{name} was not invited: not available (already approached for this paper, an author, or inactive)"));
                    } else {
                        names.insert(pin.clone(), name);
                        ok_pins.push(pin.clone());
                    }
                }
            }
        }
        if ok_pins.is_empty() {
            return Ok(out);
        }
        let form = prepare_request_form(page, &ok_pins)?;
        let next = self.http.submit(page, &form, Some("Next"))?;
        if next.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        self.page = Some(next);

        // The page with the deadline and the cover message.
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = prepare_send_form(page, &paper.id, &ok_pins, opts)?;
        form.set_checked("SendCopy", opts.copy);
        let sent = self.http.submit(page, &form, Some("Send"))?;
        if sent.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        let reply = server_message(&sent);
        self.page = Some(sent);

        // Confirm on Manage Reviews.
        let reviews = self.load_reviews(paper)?;
        for pin in &ok_pins {
            let name = names.get(pin).cloned().unwrap_or_default();
            match reviews.iter().find(|r| &r.pin == pin && r.of_version(paper.version()) && r.is_open()) {
                Some(r) => out.push(InviteResult { pin: pin.clone(), name: name.clone(), ok: true, message: format!("{name} was invited ({})", r.status_raw) }),
                None => fail(&mut out, pin, &name, format!(
                    "{name} does not appear on the Manage Reviews page after sending{}",
                    reply.as_ref().map(|m| format!("; the site said: {m}")).unwrap_or_default()
                )),
            }
        }
        Ok(out)
    }

    /// The dates on a review's Review Details page.
    pub fn review_dates(&mut self, r: &Review) -> Result<ReviewDates> {
        self.goto_review_details(r)?;
        Ok(parse_review_dates(self.current()?))
    }

    /// The text of a submitted review, from its Review Details page.
    pub fn review_text(&mut self, r: &Review) -> Result<ReviewText> {
        self.goto_review_details(r)?;
        parse_review_text(self.current()?).ok_or_else(|| anyhow!("the details of review {} show no review text ({})", r.review_id, r.status_raw))
    }

    // -- files -------------------------------------------------------------

    /// Download the file that `command(cmd)` sends from the current page.
    fn fetch_file(&mut self, cmd: &str) -> Result<Download> {
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        if !page.has_command(cmd) {
            bail!("'{cmd}' is not available on the page '{}'", page.title);
        }
        let form = page.form().context("the page has no form")?.clone();
        match self.http.download(page, &form, cmd)? {
            Fetched::File(d) => Ok(d),
            Fetched::Page(p) if p.is_login_form() => {
                self.page = None;
                Err(SessionExpired.into())
            }
            Fetched::Page(p) => {
                let why = server_message(&p).unwrap_or_else(|| p.title.clone());
                self.page = Some(p);
                bail!("PaperPlaza showed the page '{why}' instead of the file")
            }
        }
    }

    /// The manuscript of the paper's current version (its title link on the workspace).
    pub fn manuscript(&mut self, paper: &Paper) -> Result<Download> {
        if paper.download_cmd.is_empty() {
            bail!("the workspace shows no manuscript link for {}", paper.number);
        }
        self.find_in_workspace(&paper.download_cmd)?;
        self.fetch_file(&paper.download_cmd)
    }

    /// A file attached to a review ("Attachment to the Comments to the Author").
    pub fn review_attachment(&mut self, r: &Review, cmd: &str) -> Result<Download> {
        let here = self.current()?;
        if !(here.hidden("Review_ID").as_deref() == Some(r.review_id.as_str()) && here.has_command(cmd)) {
            self.goto_review_details(r)?;
        }
        self.fetch_file(cmd)
    }

    /// The files listed in a submission's popup of the Reviewer's workspace.
    pub fn submission_files(&mut self, details_cmd: &str) -> Result<Vec<FileLink>> {
        self.find_in_reviewer_workspace(details_cmd)?;
        self.cmd(details_cmd)?;
        parse_submission_files(self.current()?).ok_or_else(|| anyhow!("the submission data and files did not open"))
    }

    /// One of the files of `submission_files`.
    pub fn submission_file(&mut self, details_cmd: &str, cmd: &str) -> Result<Download> {
        if !self.current()?.has_command(cmd) {
            self.submission_files(details_cmd)?;
        }
        self.fetch_file(cmd)
    }

    /// Open the editor workspace page that offers `cmd` (next pages, then "Show all").
    fn find_in_workspace(&mut self, cmd: &str) -> Result<()> {
        if self.page.as_ref().is_some_and(|p| p.has_command(cmd) && p.form().is_some_and(|f| f.action.ends_with("workspace.pl"))) {
            return Ok(());
        }
        self.goto_workspace()?;
        for show_all in [false, true] {
            if show_all {
                let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
                let mut form = page.form().context("workspace has no form")?.clone();
                if form.get("ShowAll").is_some_and(|c| c.checked) || !form.set_checked("ShowAll", true) {
                    break;
                }
                let next = self.http.submit(page, &form, None)?;
                if next.is_login_form() {
                    self.page = None;
                    return Err(SessionExpired.into());
                }
                self.page = Some(next);
            }
            for _ in 0..50 {
                if self.current()?.has_command(cmd) {
                    return Ok(());
                }
                match page_of(self.current()?) {
                    Some((cur, total)) if cur < total && self.current()?.has_command("Next") => self.cmd("Next")?,
                    _ => break,
                }
            }
        }
        bail!("the paper is no longer listed in your workspace")
    }

    /// The reviews of a paper you reviewed, as "Inspect the reviews" shows
    /// them in the Reviewer's workspace.
    pub fn inspect_reviews(&mut self, review_id: &str) -> Result<Inspection> {
        let cmd = format!("Inspect{review_id}");
        self.find_in_reviewer_workspace(&cmd)?;
        self.cmd(&cmd)?;
        let p = self.current()?;
        if !p.form().is_some_and(|f| f.action.ends_with("/inspectreviews.pl")) || p.hidden("Review_ID").as_deref() != Some(review_id) {
            bail!("opened '{}' instead of the reviews of review {review_id}", p.title);
        }
        Ok(parse_inspection(p))
    }

    /// Open the Reviewer's workspace page that offers `cmd` (with "Show all
    /// items" and the next pages if needed).
    fn find_in_reviewer_workspace(&mut self, cmd: &str) -> Result<()> {
        if self.page.as_ref().is_some_and(|p| p.has_command(cmd)) {
            return Ok(());
        }
        let (Some(j), Some(r)) = (self.journal.clone(), self.role.clone()) else { bail!("no workspace is open") };
        if r.kind != RoleKind::Reviewer {
            bail!("'{}' is not a reviewer workspace", r.label);
        }
        self.enter_journal(&j.code)?;
        self.enter_role(&r.command)?;
        if self.current()?.has_command(cmd) {
            return Ok(());
        }
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().context("the reviewer workspace has no form")?.clone();
        if !form.get("ShowAll").is_some_and(|c| c.checked) && form.set_checked("ShowAll", true) {
            let next = self.http.submit(page, &form, None)?;
            if next.is_login_form() {
                self.page = None;
                return Err(SessionExpired.into());
            }
            self.page = Some(next);
        }
        for _ in 0..50 {
            if self.current()?.has_command(cmd) {
                return Ok(());
            }
            match page_of(self.current()?) {
                Some((cur, total)) if cur < total && self.current()?.has_command("Next") => self.cmd("Next")?,
                _ => break,
            }
        }
        bail!("this review is not listed in your Reviewer's workspace, or its reviews cannot be inspected")
    }

    // -- actions -----------------------------------------------------------

    /// Cancel a review request (no cancellation notice is sent).
    pub fn cancel_review(&mut self, r: &Review) -> Result<String> {
        let now = self.fresh_review(r)?;
        if !now.is_open() {
            bail!("not canceled: the review is '{}'", now.status_raw);
        }
        self.cmd(&r.details_cmd)?;
        if self.current()?.hidden("Review_ID").as_deref() != Some(r.review_id.as_str()) {
            bail!("opened the wrong review page; nothing was changed");
        }
        self.cmd("CancelReview")?;
        let after = self.fresh_review(r)?;
        if after.state.to_lowercase().starts_with("cancel") {
            Ok(format!("Canceled. Status: {}", after.status_raw))
        } else {
            bail!("the server did not cancel it; status is still '{}'", after.status_raw)
        }
    }

    /// Send a message to the reviewer through "Contact the reviewer".
    /// An empty `subject` keeps the site's default subject.
    pub fn contact_reviewer(
        &mut self,
        r: &Review,
        subject: &str,
        cc: &str,
        message: &str,
        opts: &ContactOptions,
        attachment: Option<(&str, &[u8])>,
    ) -> Result<String> {
        if message.trim().is_empty() {
            bail!("the message is empty");
        }
        self.goto_review_details(r)?;
        self.cmd("ContactReviewer")?;
        let page = self.current()?;
        let mut form = page.form().context("the contact page has no form")?.clone();
        if !form.action.ends_with("contactreviewer.pl") || form.value("Review_ID") != Some(r.review_id.as_str()) {
            bail!("opened '{}' instead of the contact form for review {}", page.title, r.review_id);
        }
        let to = contact_recipient(page).unwrap_or_default();
        if !subject.trim().is_empty() {
            form.set("Subject", subject);
        }
        form.set("Cc", cc);
        form.set("Cover_message", message);
        form.set_checked("DoNotArchive", opts.do_not_archive);
        form.set_checked("MyEyesOnly", opts.my_eyes_only && !opts.do_not_archive);
        form.set_checked("DoNotCopyMe", opts.do_not_copy_me);
        if let Some((name, bytes)) = attachment {
            form.attach("Attachment", name, bytes.to_vec());
        }
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let result = self.http.submit(page, &form, Some("SendMessage"))?;
        if result.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        let still_form = result.form().is_some_and(|f| f.action.ends_with("contactreviewer.pl") && f.get("Cover_message").is_some());
        let msg = server_message(&result);
        self.page = Some(result);
        if still_form {
            bail!("the site kept the message form open{}", msg.map(|m| format!(": {m}")).unwrap_or_default());
        }
        Ok(format!("Sent{}.", if to.is_empty() { String::new() } else { format!(" to {to}") }))
    }

    /// Open the site's "Send Thank You Notes" page (Reminders and thanks →
    /// "Send a thank you note for all or some reviews…") of the open journal.
    fn open_thank_you_notes(&mut self) -> Result<()> {
        self.goto_workspace()?;
        self.cmd("Reminders")?;
        self.cmd("SendThankYouNotesAll")?;
        let p = self.current()?;
        if !p.form().is_some_and(|f| f.action.ends_with("/sendthankyounotes.pl")) {
            bail!("opened '{}' instead of the Send Thank You Notes page", p.title);
        }
        Ok(())
    }

    /// The reviews the site has not yet thanked (its Send Thank You Notes list).
    pub fn unthanked(&mut self) -> Result<Vec<Unthanked>> {
        self.open_thank_you_notes()?;
        Ok(parse_unthanked(self.current()?))
    }

    /// The Send Thank You Notes list with the site's two letters (for
    /// agreeing to review, for the review).
    pub fn thank_you_page(&mut self) -> Result<(Vec<Unthanked>, String, String)> {
        self.open_thank_you_notes()?;
        let p = self.current()?;
        let f = p.form().context("the Send Thank You Notes page has no form")?;
        let letter = |n: &str| f.value(n).unwrap_or_default().to_string();
        Ok((parse_unthanked(p), letter("Cover_message_1"), letter("Cover_message_2")))
    }

    /// Send the site's own thank-you notes, with its own letters, to exactly
    /// the boxes named in `boxes` (see Unthanked). Errors before anything is
    /// sent are returned as Err; once the notes were submitted, the result says
    /// what the list shows afterwards, so the caller can tell what was sent.
    /// `letters`: replace the site's letter for agreeing to review, for the
    /// review (None: the site's own).
    pub fn send_thank_you_notes(&mut self, boxes: &[String], copy_me: bool, letters: [Option<&str>; 2]) -> Result<AfterSend<Unthanked>> {
        if boxes.is_empty() {
            bail!("no review to thank");
        }
        self.open_thank_you_notes()?;
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().context("the Send Thank You Notes page has no form")?.clone();
        let offered: Vec<String> = form.controls.iter().filter(|c| c.name.starts_with("ThankYouFor")).map(|c| c.name.clone()).collect();
        if let Some(gone) = boxes.iter().find(|b| !offered.contains(b)) {
            bail!("{gone} is no longer on the site's list");
        }
        for name in &offered {
            form.set_checked(name, boxes.contains(name));
        }
        form.set_checked("C1", false);
        form.set_checked("C2", false);
        form.set_checked("SendCopies", copy_me);
        for (name, text) in ["Cover_message_1", "Cover_message_2"].into_iter().zip(letters) {
            if let Some(t) = text {
                form.set(name, t);
            }
        }
        for (prefix, letter) in [("ThankYouForAccepting", "Cover_message_1"), ("ThankYouForSubmitting", "Cover_message_2")] {
            if boxes.iter().any(|b| b.starts_with(prefix)) && form.value(letter).is_none_or(|v| v.trim().is_empty()) {
                bail!("the site's thank-you letter ({letter}) is empty");
            }
        }
        let result = self.http.submit(page, &form, Some("SendThankYouNotes"));
        let after = match result {
            Err(e) => return Ok(AfterSend { problem: Some(format!("{e:#}")), remaining: None }),
            Ok(p) if p.is_login_form() => {
                self.page = None;
                return Ok(AfterSend { problem: Some("the session expired while sending".into()), remaining: None });
            }
            Ok(p) => p,
        };
        let msg = server_message(&after);
        self.page = Some(after);
        match self.unthanked() {
            Ok(remaining) => Ok(AfterSend { problem: None, remaining: Some(remaining) }),
            Err(e) => Ok(AfterSend { problem: Some(format!("sent, but the list could not be read again ({e:#}){}", msg.map(|m| format!("; the site said: {m}")).unwrap_or_default())), remaining: None }),
        }
    }

    /// Open one of the site's reminder lists (Reminders and thanks).
    fn open_site_list(&mut self, which: SiteList) -> Result<()> {
        self.goto_workspace()?;
        self.cmd("Reminders")?;
        self.cmd(which.command())?;
        let p = self.current()?;
        if !p.form().is_some_and(|f| f.action.ends_with(which.script())) {
            bail!("opened '{}' instead of the {} page", p.title, which.title());
        }
        Ok(())
    }

    /// The reviews on one of the site's reminder lists.
    pub fn site_list(&mut self, which: SiteList) -> Result<Vec<SiteItem>> {
        self.open_site_list(which)?;
        Ok(parse_site_list(self.current()?))
    }

    /// One of the site's reminder lists with its letter.
    pub fn site_list_page(&mut self, which: SiteList) -> Result<(Vec<SiteItem>, String)> {
        self.open_site_list(which)?;
        let p = self.current()?;
        let letter = p.form().and_then(|f| f.value("Cover_message")).unwrap_or_default().to_string();
        Ok((parse_site_list(p), letter))
    }

    /// Send the site's own reminders or confirmation requests, with its own
    /// letter, to exactly the reviews `review_ids`. As send_thank_you_notes:
    /// Err only when nothing was sent.
    /// `letter`: replace the site's letter (None: the site's own).
    pub fn send_site_list(&mut self, which: SiteList, review_ids: &[String], copy_me: bool, attach: bool, letter: Option<&str>) -> Result<AfterSend<SiteItem>> {
        if review_ids.is_empty() {
            bail!("no review selected");
        }
        self.open_site_list(which)?;
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let mut form = page.form().with_context(|| format!("the {} page has no form", which.title()))?.clone();
        let offered: Vec<(String, String)> = form.controls.iter().filter(|c| c.name.starts_with("Include")).map(|c| (c.name.clone(), c.value.clone())).collect();
        if let Some(gone) = review_ids.iter().find(|id| !offered.iter().any(|(_, v)| v == *id)) {
            bail!("review {gone} is no longer on the site's {} list", which.title());
        }
        for (name, value) in &offered {
            form.set_checked(name, review_ids.contains(value));
        }
        form.set_checked("CheckAll", false);
        form.set_checked("SendCopies", copy_me);
        form.set_checked("AttachSubmission", attach);
        if let Some(t) = letter {
            form.set("Cover_message", t);
        }
        if form.value("Cover_message").is_none_or(|v| v.trim().is_empty()) {
            bail!("the site's letter is empty");
        }
        let after = match self.http.submit(page, &form, Some("SendReminders")) {
            Err(e) => return Ok(AfterSend { problem: Some(format!("{e:#}")), remaining: None }),
            Ok(p) if p.is_login_form() => {
                self.page = None;
                return Ok(AfterSend { problem: Some("the session expired while sending".into()), remaining: None });
            }
            Ok(p) => p,
        };
        let msg = server_message(&after);
        self.page = Some(after);
        match self.site_list(which) {
            Ok(remaining) => Ok(AfterSend { problem: None, remaining: Some(remaining) }),
            Err(e) => Ok(AfterSend { problem: Some(format!("sent, but the list could not be read again ({e:#}){}", msg.map(|m| format!("; the site said: {m}")).unwrap_or_default())), remaining: None }),
        }
    }

    /// Open the paper's "Contact One or Several Editorial Staff Members" page
    /// (the Contact link of its submission menu, from Manage Reviews).
    fn open_staff_contact(&mut self, paper: &Paper) -> Result<()> {
        self.goto_reviews(&paper.id, &paper.reviews_cmd)?;
        self.cmd("Contact")?;
        let p = self.current()?;
        if !p.form().is_some_and(|f| f.action.ends_with("/contact.pl")) || !p.hidden("Submission_ID").is_some_and(|s| same_id(&s, &paper.id)) {
            bail!("opened '{}' instead of the Contact page of {}", p.title, paper.id);
        }
        Ok(())
    }

    /// The editors handling the paper (senior editor, you) and the default subject.
    pub fn staff_contacts(&mut self, paper: &Paper) -> Result<(Vec<Handler>, String)> {
        self.open_staff_contact(paper)?;
        let p = self.current()?;
        let subject = p.form().and_then(|f| f.value("Subject")).unwrap_or_default().to_string();
        Ok((parse_handlers(p), subject))
    }

    /// Send a message through the paper's Contact page to some of its handlers
    /// (`to`: their addresses as the page lists them). An empty `subject`
    /// keeps the site's default subject.
    #[allow(clippy::too_many_arguments)]
    pub fn contact_staff(
        &mut self,
        paper: &Paper,
        to: &[String],
        subject: &str,
        cc: &str,
        message: &str,
        opts: &ContactOptions,
        attachment: Option<(&str, &[u8])>,
    ) -> Result<String> {
        if message.trim().is_empty() {
            bail!("the message is empty");
        }
        if to.is_empty() {
            bail!("choose at least one recipient");
        }
        self.open_staff_contact(paper)?;
        let page = self.current()?;
        let handlers = parse_handlers(page);
        let mut names = Vec::new();
        for a in to {
            let h = handlers.iter().find(|h| &h.address == a).with_context(|| format!("{a} is not listed as a handler of {} any more; nothing was sent", paper.id))?;
            names.push(format!("{} ({})", h.name, h.role));
        }
        let mut form = page.form().context("the Contact page has no form")?.clone();
        // As a click on each name does (the browser sends the line breaks as CR LF).
        form.set("To", &to.join(",\r\n"));
        if !subject.trim().is_empty() {
            form.set("Subject", subject);
        }
        form.set("Cc", cc);
        form.set("Cover_message", message);
        form.set_checked("MyEyesOnly", opts.my_eyes_only);
        form.set_checked("DoNotCopyMe", opts.do_not_copy_me);
        if let Some((name, bytes)) = attachment {
            form.attach("Attachment", name, bytes.to_vec());
        }
        let page = self.page.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        let result = self.http.submit(page, &form, Some("SendMessage"))?;
        if result.is_login_form() {
            self.page = None;
            return Err(SessionExpired.into());
        }
        let still_form = result.form().is_some_and(|f| f.action.ends_with("/contact.pl") && f.get("Cover_message").is_some());
        let msg = server_message(&result);
        self.page = Some(result);
        if still_form {
            bail!("the site kept the message form open{}", msg.map(|m| format!(": {m}")).unwrap_or_default());
        }
        Ok(format!("Sent to {}.", names.join(", ")))
    }

    /// Extend the review deadline by `days`, as the "Extend the deadline" box does.
    pub fn extend_deadline(&mut self, r: &Review, days: u32) -> Result<String> {
        let due = self.extend_once(r, days)?;
        Ok(format!("New due date: {due}"))
    }

    /// One extension; returns the new due date as the site writes it.
    fn extend_once(&mut self, r: &Review, days: u32) -> Result<String> {
        if !EXTENSIONS.contains(&days) {
            bail!("the site only offers extensions of {EXTENSIONS:?} days");
        }
        if !r.is_open() {
            bail!("not extended: the review is '{}'", r.status_raw);
        }
        let page = self.current()?;
        let token = page.hidden("Token").context("no session token on the current page")?;
        let base = page.url.rsplit_once('/').map(|(b, _)| b.to_string()).context("bad page URL")?;
        let referer = page.url.clone();
        let body = format!("Token={token}&ExtendDeadline=ON&ReviewID={}&Extension={days}", r.review_id);
        let text = self.http.post_ajax(&format!("{base}/reviews_aja.pl"), body, "text/plain;charset=UTF-8", &referer)?;
        let text = text.trim();
        if text.starts_with("Error") || text.is_empty() {
            bail!("{}", if text.is_empty() { "empty answer from the server" } else { text });
        }
        let due = text.split('&').nth(1).unwrap_or(text);
        Ok(strip_tags(due).trim().to_string())
    }

    /// Move the deadline to `until` with several extensions in a row, as the
    /// site's box allows ("Click again for a further extension"). Each step
    /// is chosen from the date the site answered, so the result is checked.
    pub fn extend_until(&mut self, r: &Review, until: NaiveDate) -> Result<String> {
        let start = r.due.context("the current due date is not known; reload the list")?;
        let mut due = start;
        let mut done: Vec<u32> = Vec::new();
        while due < until {
            if done.len() >= 12 {
                bail!("stopped after 12 extensions; the deadline is now {}", long(due));
            }
            let step = extension_steps((until - due).num_days())[0];
            let answer = self.extend_once(r, step).map_err(|e| {
                if done.is_empty() { e } else { anyhow!("{e:#}; the deadline had already been moved to {} ({})", long(due), plus_list(&done)) }
            })?;
            let new = parse_date(&answer).with_context(|| format!("PaperPlaza answered '{answer}' after {} days; stopped there", plus_list(&[step])))?;
            if new <= due {
                let before = if done.is_empty() { String::new() } else { format!(" (it had moved from {} by {} days)", long(start), plus_list(&done)) };
                bail!("PaperPlaza kept the deadline at {} after an extension of {step} days; stopped there{before}", long(new));
            }
            done.push(step);
            due = new;
        }
        let how = if done.len() > 1 { format!(" ({} days)", plus_list(&done)) } else { String::new() };
        let off = if due != until { format!("; you asked for {}", long(until)) } else { String::new() };
        Ok(format!("New due date: {}{how}{off}", long(due)))
    }
}

fn long(d: NaiveDate) -> String {
    d.format("%B %-d, %Y").to_string()
}

fn plus_list(steps: &[u32]) -> String {
    steps.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" + ")
}

// ---------------------------------------------------------------------------
// Parsers (pure functions of a page, also used by `docket parse`)

pub fn parse_journals(page: &Page) -> Vec<Journal> {
    let doc = page.doc();
    let mut out: Vec<Journal> = Vec::new();
    for a in doc.select(&sel("a[href]")) {
        let href = a.value().attr("href").unwrap_or("");
        let Some(c) = JOURNAL_LINK.captures(href) else { continue };
        let code = c[1].to_lowercase();
        if out.iter().any(|j| j.code == code) {
            continue;
        }
        let row = a.ancestors().filter_map(ElementRef::wrap).find(|e| e.value().name() == "tr");
        let cs = row.map(|r| cells(&r)).unwrap_or_default();
        let name = cs.first().map(element_text).unwrap_or_else(|| code.to_uppercase());
        let description = cs.get(2).map(element_text).unwrap_or_default();
        let url = https(&href);
        out.push(Journal { code, name, description, url });
    }
    out
}

pub fn parse_workspace(page: &Page) -> Vec<Paper> {
    let doc = page.doc();
    let Some(t) = find_table(&doc, &["Number.Version", "Status", "Title"]) else { return Vec::new() };
    let (num, kind, status) = (t.col("Number"), t.col("Type of submission"), t.col("Status"));
    let (rec, dec, due) = (t.col("Date first received"), t.col("Date of latest decision"), t.col("Next decision due"));
    let (sup, hand, auth, title) = (t.col("Supervised"), t.col("Handled"), t.col("Authors"), t.col("Title"));
    let mut out = Vec::new();
    for row in &t.rows {
        let Some(reviews_cmd) = commands_in(row).into_iter().find(|c| c.starts_with("Reviews")) else {
            continue;
        };
        let cs = cells(row);
        let text = |i: Option<usize>| i.and_then(|i| cs.get(i)).map(element_text).unwrap_or_default();
        let number_cell = text(num);
        let number = number_cell.split_whitespace().next().unwrap_or("").to_string();
        let id = ID_IN
            .captures(&number_cell)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| number.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or(number.clone()));
        // The `value` attribute holds the clean value (no superscript markers).
        let value = |i: Option<usize>| {
            i.and_then(|i| cs.get(i)).and_then(|c| c.value().attr("value").map(|v| v.trim().to_string())).unwrap_or_else(|| text(i))
        };
        out.push(Paper {
            id,
            number,
            reviews_cmd,
            kind: text(kind),
            status: value(status),
            received: text(rec),
            latest_decision: text(dec),
            next_due: text(due),
            editor: text(sup),
            handled_by: text(hand),
            authors: value(auth),
            title: text(title),
            download_cmd: commands_in(row).into_iter().find(|c| c.starts_with("Download")).unwrap_or_default(),
        });
    }
    out
}

pub fn parse_reviews(page: &Page, paper_id: &str, paper_title: &str, reviews_cmd: &str) -> Vec<Review> {
    let doc = page.doc();
    let Some(t) = find_table(&doc, &["Review ID", "Review status"]) else { return Vec::new() };
    let reviewer_col = t.headers.iter().position(|h| h.eq_ignore_ascii_case("Reviewer")).or(t.col("Reviewer ("));
    let (ver, no, rid, st, rec) =
        (t.col("version"), t.col("Reviewer number"), t.col("Review ID"), t.col("Review status"), t.col("recommendation"));
    let today = chrono::Local::now().date_naive();
    let mut out = Vec::new();
    // A row "Reviews for IEEE-TAC 26-1234 Previous Submission 25-5678 (Submission ID 25-5678)"
    // starts the reviews of the earlier submission; their versions count within it.
    let mut from_submission = String::new();
    for row in &t.rows {
        if let Some(c) = PREVIOUS_SUBMISSION.captures(&element_text(row)) {
            from_submission = c[1].to_string();
            continue;
        }
        let Some(details_cmd) = commands_in(row).into_iter().find(|c| c.starts_with("Details")) else { continue };
        let cs = cells(row);
        let text = |i: Option<usize>| i.and_then(|i| cs.get(i)).map(element_text).unwrap_or_default();
        let (reviewer, pin) = split_name_pin(&text(reviewer_col));
        let (first_name, last_name) = split_first_last(&reviewer);
        let status_raw = text(st);
        let s = parse_status(&status_raw);
        let mut r = Review {
            paper_id: paper_id.to_string(),
            paper_title: paper_title.to_string(),
            reviews_cmd: reviews_cmd.to_string(),
            version: text(ver),
            reviewer_no: text(no),
            reviewer,
            first_name,
            last_name,
            pin,
            review_id: text(rid),
            details_cmd,
            status_raw,
            state: s.state,
            state_date: s.date,
            due: s.due,
            reminders: s.reminders,
            thanked: s.thanked,
            overdue: false,
            recommendation: text(rec),
            from_submission: from_submission.clone(),
        };
        r.overdue = r.is_open() && r.due.is_some_and(|d| d < today);
        out.push(r);
    }
    out
}

/// Rows of the Reviewer's workspace table (reviewerspace.pl).
pub fn parse_assignments(page: &Page, journal: &str) -> Vec<Assignment> {
    let doc = page.doc();
    let Some(t) = find_table(&doc, &["Review ID", "Review requested by", "Review status"]) else { return Vec::new() };
    let (rid, no, req, auth, title) =
        (t.col("Review ID"), t.col("Reviewer number"), t.col("Review requested by"), t.col("Authors"), t.col("Title"));
    let (sid, num, ver, st) = (t.col("Submission ID"), t.col("Submission number"), t.col("Version"), t.col("Review status"));
    let (kind, rec) = (t.col("Under review for publication"), t.col("recommendation"));
    let today = chrono::Local::now().date_naive();
    let mut out = Vec::new();
    for row in &t.rows {
        let cs = cells(row);
        let text = |i: Option<usize>| i.and_then(|i| cs.get(i)).map(element_text).unwrap_or_default();
        let review_id = text(rid);
        if review_id.is_empty() || !review_id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let (mut requested_by, mut requester_email) = (String::new(), String::new());
        let full = text(req);
        if let Some(cell) = req.and_then(|i| cs.get(i)) {
            if let Some(a) = cell.select(&sel("a[href^='mailto:']")).next() {
                requested_by = element_text(&a);
                let href = a.value().attr("href").unwrap_or("");
                requester_email = href.trim_start_matches("mailto:").split('?').next().unwrap_or("").to_string();
            }
        }
        if requested_by.is_empty() {
            requested_by = full.split(" (").next().unwrap_or("").to_string();
        }
        let requester_role = full.rsplit_once('(').map(|(_, r)| r.trim_end_matches(')').trim().to_string()).unwrap_or_default();
        let status_raw = text(st);
        let s = parse_status(&status_raw);
        let open = state_is_open(&s.state);
        out.push(Assignment {
            journal: journal.to_string(),
            review_id,
            reviewer_no: text(no),
            requested_by,
            requester_role,
            requester_email,
            authors: text(auth),
            title: text(title),
            submission_id: text(sid),
            number: text(num),
            version: text(ver),
            kind: text(kind),
            overdue: open && s.due.is_some_and(|d| d < today),
            status_raw,
            state: s.state,
            state_date: s.date,
            due: s.due,
            reminders: s.reminders,
            recommendation: text(rec),
            can_inspect: commands_in(row).iter().any(|c| c.starts_with("Inspect")),
            details_cmd: commands_in(row).into_iter().find(|c| c.starts_with("SubmissionDetails")).unwrap_or_default(),
            other_actions: commands_in(row)
                .into_iter()
                .filter(|c| !c.starts_with("SubmissionDetails") && !c.starts_with("Inspect"))
                .collect(),
        });
    }
    out
}

/// Title, keywords and authors from the header of requestreview.pl.
pub fn parse_paper_info(page: &Page) -> PaperInfo {
    paper_info_in(&page.doc())
}

fn paper_info_in(doc: &scraper::Html) -> PaperInfo {
    static AUTHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*([^;\[]+?)\*?\s*\[(.*?)\s*\((\d+)\)\]").unwrap());
    let mut info = PaperInfo::default();
    for tr in doc.select(&sel("tr")) {
        let cs = cells(&tr);
        if cs.len() < 2 {
            continue;
        }
        match element_text(&cs[0]).as_str() {
            "Title" if info.title.is_empty() => info.title = element_text(&cs[1]),
            "Authors" if info.authors.is_empty() => {
                let t = element_text(&cs[1]);
                info.authors = AUTHOR
                    .captures_iter(&t)
                    .map(|c| Author { name: c[1].trim().to_string(), affiliation: c[2].trim().to_string(), pin: c[3].to_string() })
                    .collect();
            }
            _ => {}
        }
    }
    for td in doc.select(&sel("td")) {
        if !element_text(&td).starts_with("Submission keywords:") {
            continue;
        }
        if let Some(b) = td.select(&sel("b")).next() {
            info.keywords = element_text(&b).split(';').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
            break;
        }
    }
    info
}

/// The "Select Names from your Reviewers List" table of requestreview.pl.
pub fn parse_candidates(page: &Page) -> Vec<Candidate> {
    candidates_in(&page.doc())
}

fn candidates_in(doc: &scraper::Html) -> Vec<Candidate> {
    let Some(t) = find_table(doc, &["Name", "Affiliation", "E-mail address", "Keywords"]) else { return Vec::new() };
    // The name cell reads "Doe, Jane (Prof., 12345)", or "Doe, Jane (12345)" without a title.
    static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(([^(),]+),\s*\d+\)\s*$").unwrap());
    let (name_c, aff, mail, kw) = (t.col("Name"), t.col("Affiliation"), t.col("E-mail address"), t.col("Keywords"));
    let mut out = Vec::new();
    for row in &t.rows {
        let Some(cb) = row.select(&sel("input[type=checkbox]")).find(|i| i.value().attr("name").is_some_and(|n| n.starts_with("Include")))
        else {
            continue;
        };
        let pin = cb.value().attr("name").unwrap_or("").trim_start_matches("Include").to_string();
        let (name, _) = split_name_pin(cb.value().attr("value").unwrap_or(""));
        let cs = cells(row);
        let text = |i: Option<usize>| i.and_then(|i| cs.get(i)).map(element_text).unwrap_or_default();
        let matches = kw
            .and_then(|i| cs.get(i))
            .map(|c| c.select(&sel(".hl")).map(|e| element_text(&e)).collect())
            .unwrap_or_default();
        let gray = row.value().attr("style").is_some_and(|s| s.replace(' ', "").contains("#888888"));
        let title = TITLE.captures(&text(name_c)).map(|c| c[1].trim().to_string()).unwrap_or_default();
        out.push(Candidate {
            pin,
            name,
            title,
            affiliation: text(aff),
            email: text(mail),
            keywords: text(kw),
            matches,
            available: cb.value().attr("disabled").is_none() && !gray,
            in_list: true,
            score: 0,
            reasons: Vec::new(),
            conflict: None,
        });
    }
    out
}

/// Abstract and keywords rows of the Details ("Database Entry") page.
pub fn parse_abstract(page: &Page) -> (String, Vec<String>) {
    let doc = page.doc();
    let (mut abs, mut kws) = (String::new(), Vec::new());
    for tr in doc.select(&sel("tr")) {
        let cs = cells(&tr);
        if cs.len() < 2 {
            continue;
        }
        let last = cs.last().unwrap();
        match element_text(&cs[0]).as_str() {
            "Abstract" if abs.is_empty() => abs = last.text().collect::<String>().trim().to_string(),
            "Keywords" if kws.is_empty() => kws = element_text(last).split(';').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect(),
            _ => {}
        }
    }
    (abs, kws)
}

/// Results of the PIN database search (pins.pl). A greyed-out "Add to
/// reviewers" box means the person is already on your list; inactive PINs
/// have no box.
pub fn parse_pin_results(page: &Page) -> Vec<Candidate> {
    let doc = page.doc();
    // "E-mail address" tells the results header apart from the "Search in" menu (which says "Email address").
    let Some(t) = find_table(&doc, &["PIN", "Surname", "Given name", "Affiliation", "E-mail address"]) else { return Vec::new() };
    let (pin_c, sur, given, title, aff, mail, kw) =
        (t.col("PIN"), t.col("Surname"), t.col("Given name"), t.col("Title"), t.col("Affiliation"), t.col("E-mail address"), t.col("Keywords"));
    let mut out = Vec::new();
    for row in &t.rows {
        let cs = cells(row);
        let text = |i: Option<usize>| i.and_then(|i| cs.get(i)).map(element_text).unwrap_or_default();
        let pin_text = text(pin_c);
        let pin: String = pin_text.chars().take_while(char::is_ascii_digit).collect();
        if pin.is_empty() {
            continue;
        }
        let cb = row.select(&sel("input[type=checkbox]")).find(|i| i.value().attr("name") == Some(format!("AddToReviewers{pin}").as_str()));
        let (surname, given_name) = (text(sur), text(given));
        out.push(Candidate {
            name: if given_name.is_empty() { surname } else { format!("{surname}, {given_name}") },
            title: text(title),
            affiliation: text(aff),
            email: text(mail),
            keywords: text(kw),
            matches: Vec::new(),
            available: cb.is_some() && !pin_text.contains("Inactive"),
            in_list: cb.is_some_and(|c| c.value().attr("disabled").is_some()),
            score: 0,
            reasons: Vec::new(),
            conflict: None,
            pin,
        });
    }
    out
}

/// The URL of "Add PINs to Your Reviewers List" linked from a page.
fn add_pins_link(page: &Page) -> Option<String> {
    static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"((?:https?://[^'"\s]+/)?pins\.pl\?[^'"\s]*AddReviewers=ON[^'"\s]*)"#).unwrap());
    let doc = page.doc();
    for el in doc.select(&sel("[href], [onclick]")) {
        for attr in ["href", "onclick"] {
            if let Some(m) = el.value().attr(attr).and_then(|v| LINK.captures(v)) {
                let u = m[1].to_string();
                if u.starts_with("http") {
                    return Some(https(&u));
                }
                let base = page.url.rsplit_once('/').map(|(b, _)| b)?;
                return Some(format!("{base}/{u}"));
            }
        }
    }
    None
}

/// The Request a Review form with these reviewers ticked and listed in the
/// Request List, as `next()` would submit it.
pub fn prepare_request_form(page: &Page, pins: &[String]) -> Result<crate::page::Form> {
    let mut form = page.form().context("the Request a Review page has no form")?.clone();
    if !form.action.ends_with("/requestreview.pl") || form.get("RequestList").is_none() {
        bail!("this is not a complete Request a Review page");
    }
    let mut list = String::new();
    for pin in pins {
        let name = format!("Include{pin}");
        let c = form.get(&name).with_context(|| format!("PIN {pin} is not on your Reviewers list"))?;
        if c.disabled {
            bail!("PIN {pin} cannot be requested (already approached, an author, or inactive)");
        }
        list.push_str(&c.value);
        list.push('\r');
        form.set_checked(&name, true);
    }
    form.set("RequestList", &list);
    Ok(form)
}

/// The page after "Next" on Request a Review, filled in for "Send": only
/// the reviewers asked for, the deadline and attachment as chosen, and the
/// site's cover message.
pub fn prepare_send_form(page: &Page, paper_id: &str, pins: &[String], opts: &InviteOptions) -> Result<crate::page::Form> {
    let mut form = page.form().context("the page after Next has no form")?.clone();
    let for_paper = page.hidden("Submission_ID").is_some_and(|s| same_id(&s, paper_id));
    if !form.action.ends_with("/requestreview.pl") || page.hidden("Next").as_deref() != Some("ON") || !for_paper || form.get("Cover_message").is_none() {
        bail!("“Next” opened '{}', not the page that sends the review requests of {paper_id}; nothing was sent", page.title);
    }
    let listed: Vec<String> = page.hidden("ReviewerPINs").unwrap_or_default().split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).map(String::from).collect();
    let mut want: Vec<&String> = pins.iter().collect();
    want.sort();
    let mut got: Vec<&String> = listed.iter().collect();
    got.sort();
    if want != got {
        bail!("the site listed PINs {} instead of {}; nothing was sent", listed.join(", "), pins.join(", "));
    }
    for pin in pins {
        if !form.get(&format!("Include{pin}")).is_some_and(|c| c.checked) {
            bail!("PIN {pin} is not ticked on the page after Next; nothing was sent");
        }
    }
    if let Some(d) = opts.deadline {
        use chrono::Datelike;
        let ok = form.select("MonthDue", &d.month().to_string()) && form.select("DayDue", &d.day().to_string()) && form.select("YearDue", &d.year().to_string());
        if !ok {
            bail!("the deadline {d} cannot be chosen on this page; nothing was sent");
        }
    }
    if !form.set_checked("AttachSubmission", opts.attach) && opts.attach {
        bail!("the page has no “Attach the files” box; nothing was sent");
    }
    if form.value("Cover_message").is_none_or(|m| m.trim().is_empty()) {
        bail!("the page has no cover message; nothing was sent");
    }
    Ok(form)
}

/// The cells of a row as written (a cell spanning columns counts once).
fn tds<'a>(tr: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    tr.children().filter_map(ElementRef::wrap).filter(|c| matches!(c.value().name(), "td" | "th")).collect()
}

/// Text of a cell, keeping the line breaks of <pre> blocks.
fn cell_text(el: &ElementRef) -> String {
    if let Some(pre) = el.select(&sel("pre")).next() {
        return pre.text().collect::<String>().replace("\r\n", "\n").trim().to_string();
    }
    element_text(el).trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}').to_string()
}

/// Rows of the same table that follow `tr`.
fn rows_after<'a>(tr: &ElementRef<'a>) -> impl Iterator<Item = ElementRef<'a>> {
    tr.next_siblings().filter_map(ElementRef::wrap).filter(|e| e.value().name() == "tr")
}

/// The dates on a Review Details page ("Date requested", "Reminders sent", …).
pub fn parse_review_dates(page: &Page) -> ReviewDates {
    let doc = page.doc();
    let mut d = ReviewDates::default();
    for tr in doc.select(&sel("tr")) {
        let cs = tds(&tr);
        if cs.len() != 2 {
            continue;
        }
        let value = cell_text(&cs[1]);
        let dates: Vec<chrono::NaiveDate> = DATE_IN.find_iter(&value).filter_map(|m| parse_date(m.as_str())).collect();
        match element_text(&cs[0]).as_str() {
            "Date requested" => d.requested = dates.first().copied(),
            "Date confirmed" => d.confirmed = dates.first().copied(),
            "Date submitted" => d.submitted = dates.first().copied(),
            "Date canceled" => d.canceled = dates.first().copied(),
            "Reminders sent" => d.last_reminder = dates.iter().max().copied(),
            _ => {}
        }
    }
    d
}

/// The review on a Review Details page (None before it is submitted).
pub fn parse_review_text(page: &Page) -> Option<ReviewText> {
    let doc = page.doc();
    let rows: Vec<ElementRef> = doc.select(&sel("tr")).collect();
    let field = |name: &str| {
        rows.iter().find_map(|tr| {
            let cs = tds(tr);
            (cs.len() == 2 && element_text(&cs[0]) == name).then(|| cell_text(&cs[1]))
        })
    };
    let start = rows.iter().find(|tr| tds(tr).len() == 1 && element_text(tr) == "Review Contents")?;
    let (mut answers, mut attachments) = (Vec::new(), Vec::new());
    for tr in rows_after(start) {
        let cs = tds(&tr);
        if cs.len() < 2 {
            continue;
        }
        let last = cs.last().unwrap();
        // "Attachment to the Comments to the Author": a Download link and "Pdf file, 573 KB".
        if let Some(command) = commands_in(last).into_iter().find(|c| c.starts_with("Download")) {
            let about = element_text(last).trim_start_matches("Download").trim().to_string();
            attachments.push(FileLink { label: element_text(&cs[0]), about, command });
            continue;
        }
        let answer = cell_text(last);
        if !answer.is_empty() {
            answers.push(Answer { question: element_text(&cs[0]), answer });
        }
    }
    Some(ReviewText {
        review_id: page.hidden("Review_ID").unwrap_or_default(),
        reviewer: field("Reviewer").unwrap_or_default(),
        form: field("Review form").unwrap_or_default(),
        answers,
        attachments,
    })
}

/// The "Files of Submission … – Click on the Link to Download" table of the
/// submission popup in the Reviewer's workspace (after SubmissionDetails<id>).
/// Columns: Timestamp, Type, Version, Extension, Size. Every link in a row is
/// taken as the file's download command.
pub fn parse_submission_files(page: &Page) -> Option<Vec<FileLink>> {
    let doc = page.doc();
    let rows: Vec<ElementRef> = doc.select(&sel("tr")).collect();
    let head = rows.iter().find(|tr| tds(tr).len() == 1 && element_text(tr).starts_with("Files of Submission"))?;
    let mut out = Vec::new();
    for tr in rows_after(head) {
        let cs: Vec<String> = tds(&tr).iter().map(cell_text).collect();
        let Some(command) = commands_in(&tr).into_iter().next() else { continue };
        let at = |i: usize| cs.get(i).cloned().unwrap_or_default();
        let label = [at(1), if at(2).is_empty() { String::new() } else { format!("version {}", at(2)) }].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(", ");
        let about = [at(3), at(4), at(0)].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
        out.push(FileLink { label: if label.is_empty() { cs.join(" ") } else { label }, about, command });
    }
    Some(out)
}

/// The "Inspect the Reviews" page of the Reviewer's workspace.
pub fn parse_inspection(page: &Page) -> Inspection {
    let doc = page.doc();
    let rows: Vec<ElementRef> = doc.select(&sel("tr")).collect();
    let mut out = Inspection::default();
    for tr in &rows {
        let cs = tds(tr);
        let first = cs.first().map(element_text).unwrap_or_default();
        if cs.len() == 1 {
            if let Some(n) = first.strip_prefix("Details of Submission Number ") {
                out.number = n.trim().to_string();
            }
        }
        if cs.len() == 2 {
            let v = cell_text(&cs[1]);
            match first.as_str() {
                "Title" => out.title = v,
                "Authors" => out.authors = v,
                "Current version" => out.current_version = v,
                "Current status" => out.current_status = v,
                "Current type of submission" => out.kind = v,
                _ => {}
            }
        }
    }
    for start in rows.iter().filter(|tr| tds(tr).len() == 1 && element_text(tr).starts_with("Reviews of Version")) {
        let mut v = VersionReviews { heading: element_text(start), reviews: Vec::new(), decisions: Vec::new() };
        let mut decision: Option<String> = None;
        for tr in rows_after(start) {
            let cs = tds(&tr);
            let first = cs.first().map(element_text).unwrap_or_default();
            if cs.len() == 1 {
                decision = Some(first);
            } else if cs.len() == 4 && !first.is_empty() && first.chars().all(|c| c.is_ascii_digit()) {
                v.reviews.push(SharedReview {
                    review_id: first,
                    reviewer_no: element_text(&cs[1]),
                    recommendation: cell_text(&cs[2]),
                    comments: cell_text(&cs[3]),
                    mine: cs[0].select(&sel("b, a")).next().is_some(),
                });
            } else if let Some(d) = decision.take() {
                let parts: Vec<String> = cs.iter().map(cell_text).filter(|t| !t.is_empty() && t != "More...").collect();
                v.decisions.push(format!("{d}: {}", parts.join(" · ")));
            }
        }
        out.versions.push(v);
    }
    out
}

/// "Handlers of the submission" on the Contact page: links reading "Jane Doe,
/// Senior Editor, 12345, jane.doe@example.org" whose click
/// (`addhandler('1')`) copies the hidden field Handler1 into To.
pub fn parse_handlers(page: &Page) -> Vec<Handler> {
    static ADD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"addhandler\('(\d+)'\)").unwrap());
    let doc = page.doc();
    let mut out = Vec::new();
    for a in doc.select(&sel("a[onclick]")) {
        let Some(n) = a.value().attr("onclick").and_then(|v| ADD.captures(v)).map(|c| c[1].to_string()) else { continue };
        let Some(address) = page.hidden(&format!("Handler{n}")) else { continue };
        let text = element_text(&a);
        let parts: Vec<&str> = text.rsplitn(4, ',').map(str::trim).collect();
        let [email, pin, role, name] = parts[..] else { continue };
        out.push(Handler { name: name.into(), role: role.into(), pin: pin.into(), email: email.into(), address });
    }
    out
}

/// What happened after notes were submitted on one of the site's
/// "Reminders and thanks" pages.
pub struct AfterSend<T> {
    pub problem: Option<String>,
    /// The site's list read again afterwards.
    pub remaining: Option<Vec<T>>,
}

/// The site's lists of reviews to remind (Reminders and thanks page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteList {
    /// "Send review reminders for some or all unfulfilled review requests".
    Reminders,
    /// "Request confirmation of some or all unconfirmed review requests".
    Confirmations,
}

impl SiteList {
    fn command(self) -> &'static str {
        match self {
            SiteList::Reminders => "SendRemindersAll",
            SiteList::Confirmations => "RequestConfirmationsAll",
        }
    }
    fn script(self) -> &'static str {
        match self {
            SiteList::Reminders => "/sendreminders.pl",
            SiteList::Confirmations => "/requestconfirmations.pl",
        }
    }
    fn title(self) -> &'static str {
        match self {
            SiteList::Reminders => "Send Review Reminders",
            SiteList::Confirmations => "Request Review Confirmations",
        }
    }
}

/// The rows of a Send Review Reminders or Request Review Confirmations page,
/// read by column name.
pub fn parse_site_list(page: &Page) -> Vec<SiteItem> {
    let doc = page.doc();
    let rows: Vec<ElementRef> = doc.select(&sel("tr")).collect();
    let Some(head) = rows.iter().map(tds).find(|cs| cs.iter().any(|c| element_text(c).trim() == "Review ID")) else { return Vec::new() };
    let names: Vec<String> = head.iter().map(|c| element_text(c).trim().to_string()).collect();
    let col = |n: &str| names.iter().position(|x| x == n);
    let (id, rev, sub) = (col("Review ID"), col("Reviewer"), col("Submission number"));
    let status = col("Review status");
    let days = col("Days overdue").or(col("Days elapsed"));
    let prev = col("Previous reminders").or(col("Previous confirmation requests"));
    let mut out = Vec::new();
    for tr in &rows {
        let cs = tds(tr);
        let Some(cb) = tr.select(&sel("input[type=checkbox]")).find(|i| i.value().attr("name").is_some_and(|n| n.starts_with("Include"))) else { continue };
        let text = |i: Option<usize>| i.and_then(|i| cs.get(i)).map(cell_text).unwrap_or_default();
        let review_id = text(id);
        if review_id.is_empty() || cb.value().attr("value") != Some(review_id.as_str()) {
            continue;
        }
        let status_raw = text(status);
        let mut previous: Vec<chrono::NaiveDate> = DATE_IN.find_iter(&text(prev)).filter_map(|m| parse_date(m.as_str())).collect();
        previous.sort();
        out.push(SiteItem {
            review_id,
            checkbox: cb.value().attr("name").unwrap_or_default().to_string(),
            reviewer: text(rev),
            paper: text(sub),
            due: parse_status(&status_raw).due,
            status_raw,
            days: text(days).replace('\u{2212}', "-").trim().parse().ok(),
            previous,
        });
    }
    out
}


/// The rows of the Send Thank You Notes page.
pub fn parse_unthanked(page: &Page) -> Vec<Unthanked> {
    let doc = page.doc();
    let mut out = Vec::new();
    for tr in doc.select(&sel("tr")) {
        let cs = tds(&tr);
        if cs.len() < 9 {
            continue;
        }
        let review_id = cell_text(&cs[3]);
        if review_id.is_empty() || !review_id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let boxed = |c: &ElementRef, prefix: &str| {
            c.select(&sel("input[type=checkbox]")).filter_map(|i| i.value().attr("name")).find(|n| n.starts_with(prefix)).map(String::from)
        };
        let status_raw = cell_text(&cs[4]);
        let s = parse_status(&status_raw);
        out.push(Unthanked {
            review_id,
            state: s.state,
            date: s.date,
            status_raw,
            reviewer: cell_text(&cs[5]),
            paper: cell_text(&cs[8]),
            accept_box: boxed(&cs[1], "ThankYouForAccepting"),
            review_box: boxed(&cs[2], "ThankYouForSubmitting"),
        });
    }
    out
}

/// The "To" address shown on the Contact the Reviewer page.
pub fn contact_recipient(page: &Page) -> Option<String> {
    let doc = page.doc();
    for tr in doc.select(&sel("tr")) {
        let cs = cells(&tr);
        if cs.len() >= 2 && element_text(&cs[0]).trim_end_matches('*') == "To" {
            return Some(element_text(&cs[1]));
        }
    }
    None
}

fn page_of(page: &Page) -> Option<(u32, u32)> {
    let t = page.text();
    PAGE_OF.captures(&t).and_then(|c| Some((c[1].parse().ok()?, c[2].parse().ok()?)))
}

pub(crate) fn same_id(a: &str, b: &str) -> bool {
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    a == b || (!digits(a).is_empty() && digits(a) == digits(b))
}

fn strip_tags(s: &str) -> String {
    Regex::new(r"<[^>]*>").unwrap().replace_all(s, "").trim().to_string()
}

/// A short error or notice the server put on the page, if any.
pub fn server_message(page: &Page) -> Option<String> {
    let t = page.text();
    let rx = Regex::new(r"(?i)[^.!?]*\b(incorrect|invalid|not recognized|wrong|error|failed|denied|not allowed|successfully|has been sent|was sent)\b[^.!?]*[.!?]?").unwrap();
    rx.find(&t).map(|m| m.as_str().trim().chars().take(300).collect())
}

#[cfg(test)]
mod extension_tests {
    use super::extension_steps;

    #[test]
    fn fewest_extensions() {
        assert_eq!(extension_steps(18), vec![14, 3, 1]);
        assert_eq!(extension_steps(17), vec![14, 3]);
        assert_eq!(extension_steps(28), vec![28]);
        assert_eq!(extension_steps(2), vec![1, 1]);
        assert_eq!(extension_steps(19).len(), 4);
        assert_eq!(extension_steps(0), Vec::<u32>::new());
        assert_eq!(extension_steps(-4), Vec::<u32>::new());
        for d in 1..=120 {
            let s = extension_steps(d);
            assert_eq!(s.iter().map(|&x| x as i64).sum::<i64>(), d);
            assert!(s.len() <= (d as usize).div_ceil(28) + 3, "{d}: {s:?}");
        }
    }
}
