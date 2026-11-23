//! Automatic invitations: keep a paper supplied with invited reviewers from an
//! ordered list until enough of them have accepted.
//!
//! Each check reads the paper's Manage Reviews page and counts the reviews of
//! the current version: accepted (confirmed, received, submitted), lost
//! (declined, canceled) and pending (anything else). It then invites the next
//! names from the list so that accepted + pending reaches `at_once`, and stops
//! once `needed` reviewers have accepted. Optionally, an invitation sent by the
//! scheduler that is still unanswered after `no_answer_days` is canceled and
//! replaced. Reviews requested by hand count toward the totals but are never
//! canceled by the scheduler.

use crate::creds;
use crate::model::{InviteOptions, Paper, Review};
use crate::papercept::{Session, same_id};
use anyhow::{Result, anyhow, bail};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};

const MAX_LOG: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    pub pin: String,
    pub name: String,
    #[serde(default)]
    pub invited_on: Option<NaiveDate>,
    /// Why this name could not be invited; it is skipped until the list is edited.
    #[serde(default)]
    pub skipped: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Active,
    Paused,
    Done,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    pub accepted: u32,
    pub pending: u32,
    pub lost: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub at: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schedule {
    pub id: u64,
    /// Host of the site (papercept::SITES).
    #[serde(default = "first_site")]
    pub site: String,
    pub journal: String,
    pub paper_id: String,
    pub paper_number: String,
    pub title: String,
    /// Stop once this many reviewers have accepted.
    pub needed: u32,
    /// Keep this many reviewers invited or accepted at any time.
    pub at_once: u32,
    pub no_answer_days: Option<u32>,
    /// In order; the first `at_once` are invited first.
    pub queue: Vec<Backup>,
    pub state: State,
    pub problem: Option<String>,
    pub last_check: Option<String>,
    #[serde(default)]
    pub counts: Counts,
    #[serde(default)]
    pub log: Vec<LogLine>,
    /// Attachment and copy choices for the invitations (the site's deadline is kept).
    #[serde(default)]
    pub invite: InviteOptions,
}

fn first_site() -> String {
    crate::papercept::SITES[0].0.to_string()
}

impl Schedule {
    fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.log.last().is_some_and(|l| l.text == text) {
            return;
        }
        self.log.push(LogLine { at: now(), text });
        if self.log.len() > MAX_LOG {
            self.log.drain(..self.log.len() - MAX_LOG);
        }
    }

    fn set_problem(&mut self, p: Option<String>) {
        if p.is_some() && p != self.problem {
            notify(&self.paper_number, p.as_deref().unwrap_or(""));
        }
        self.problem = p;
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    pub next_id: u64,
    pub schedules: Vec<Schedule>,
}

impl Store {
    pub fn load() -> Store {
        std::fs::read(creds::config_dir().join("schedules.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(creds::config_dir())?;
        let path = creds::config_dir().join("schedules.json");
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn has_active(&self) -> bool {
        self.schedules.iter().any(|s| s.state == State::Active)
    }

    /// Create a schedule, or replace the settings and list of an existing one
    /// (keeping who was already invited and when).
    pub fn upsert(&mut self, id: Option<u64>, paper: &Paper, journal: &str, needed: u32, at_once: u32, no_answer_days: Option<u32>, queue: Vec<(String, String)>) -> Result<u64> {
        if needed == 0 {
            bail!("at least one reviewer must be needed");
        }
        if at_once < needed {
            bail!("“invited at once” must be at least the number of reviewers needed");
        }
        if no_answer_days == Some(0) {
            bail!("the no-answer delay must be at least one day");
        }
        if queue.is_empty() {
            bail!("add at least one reviewer to the list");
        }
        for (i, (pin, _)) in queue.iter().enumerate() {
            if pin.is_empty() || !pin.chars().all(|c| c.is_ascii_digit()) {
                bail!("'{pin}' is not a PIN");
            }
            if queue[..i].iter().any(|(p, _)| p == pin) {
                bail!("PIN {pin} is in the list twice");
            }
        }
        let other = self.schedules.iter().find(|s| s.journal == journal && same_id(&s.paper_id, &paper.id) && Some(s.id) != id && s.state != State::Done);
        if let Some(o) = other {
            bail!("{} already has an automatic invitation schedule (#{})", paper.number, o.id);
        }
        let old = id.and_then(|id| self.schedules.iter().find(|s| s.id == id)).cloned();
        if id.is_some() && old.is_none() {
            bail!("that schedule no longer exists");
        }
        let queue: Vec<Backup> = queue
            .into_iter()
            .map(|(pin, name)| {
                let invited_on = old.as_ref().and_then(|o| o.queue.iter().find(|b| b.pin == pin)).and_then(|b| b.invited_on);
                Backup { pin, name, invited_on, skipped: None }
            })
            .collect();
        if let Some(o) = &old {
            if let Some(b) = o.queue.iter().find(|b| b.invited_on.is_some() && !queue.iter().any(|q| q.pin == b.pin)) {
                bail!("{} was already invited by this schedule and cannot be removed from the list", b.name);
            }
        }
        let mut s = match old {
            Some(o) => o,
            None => {
                self.next_id += 1;
                Schedule {
                    id: self.next_id,
                    site: first_site(),
                    journal: journal.to_string(),
                    paper_id: paper.id.clone(),
                    paper_number: paper.number.clone(),
                    title: paper.title.clone(),
                    needed,
                    at_once,
                    no_answer_days,
                    queue: Vec::new(),
                    state: State::Active,
                    problem: None,
                    last_check: None,
                    counts: Counts::default(),
                    log: Vec::new(),
                    invite: InviteOptions::default(),
                }
            }
        };
        let fresh = s.queue.is_empty();
        s.needed = needed;
        s.at_once = at_once;
        s.no_answer_days = no_answer_days;
        s.queue = queue;
        s.state = State::Active;
        s.problem = None;
        s.note(format!(
            "{}: {needed} needed, {at_once} invited at once, {} names in the list{}",
            if fresh { "Created" } else { "Changed" },
            s.queue.len(),
            no_answer_days.map(|d| format!(", replace after {d} days without answer")).unwrap_or_default()
        ));
        let id = s.id;
        self.schedules.retain(|x| x.id != id);
        self.schedules.push(s);
        Ok(id)
    }
}

#[derive(Debug, PartialEq)]
enum Class {
    Accepted,
    Lost,
    Pending,
}

fn classify(state: &str) -> Class {
    let s = state.to_lowercase();
    if ["confirm", "receiv", "submit"].iter().any(|p| s.starts_with(p)) {
        Class::Accepted
    } else if ["cancel", "declin"].iter().any(|p| s.starts_with(p)) {
        Class::Lost
    } else {
        Class::Pending
    }
}

#[derive(Debug)]
pub struct Decision {
    pub counts: Counts,
    pub done: bool,
    /// Unanswered invitations from this schedule, past the no-answer delay.
    pub cancel: Vec<Review>,
    pub invite: Vec<Backup>,
    /// More acceptances are needed than the list can still provide.
    pub short_by: u32,
}

/// What to do now, given the paper's reviews of the current `version`.
pub fn decide(s: &Schedule, version: &str, reviews: &[Review], today: NaiveDate) -> Decision {
    let cur: Vec<&Review> = reviews.iter().filter(|r| r.from_submission.is_empty() && (version.is_empty() || r.version == version)).collect();
    let mut counts = Counts::default();
    let mut cancel = Vec::new();
    for r in &cur {
        match classify(&r.state) {
            Class::Accepted => counts.accepted += 1,
            Class::Lost => counts.lost += 1,
            Class::Pending => {
                counts.pending += 1;
                let ours = s.queue.iter().find(|b| b.pin == r.pin).and_then(|b| b.invited_on);
                if let (Some(days), Some(on)) = (s.no_answer_days, ours) {
                    if (today - on).num_days() >= days as i64 {
                        cancel.push((*r).clone());
                    }
                }
            }
        }
    }
    if counts.accepted >= s.needed {
        return Decision { counts, done: true, cancel: Vec::new(), invite: Vec::new(), short_by: 0 };
    }
    let active = counts.accepted + counts.pending - cancel.len() as u32;
    let slots = s.at_once.saturating_sub(active) as usize;
    let fresh: Vec<&Backup> = s.queue.iter().filter(|b| b.invited_on.is_none() && b.skipped.is_none() && !cur.iter().any(|r| r.pin == b.pin)).collect();
    let invite: Vec<Backup> = fresh.iter().take(slots).map(|b| (*b).clone()).collect();
    let left = (fresh.len() - invite.len()) as u32;
    let reachable = active + invite.len() as u32 + left;
    let short_by = s.needed.saturating_sub(reachable);
    Decision { counts, done: false, cancel, invite, short_by }
}

/// Check every active schedule (or only `only`). Returns one line per schedule.
/// In viewer mode nothing is sent; the log says what would have been done.
pub fn run(session: &mut Session, store: &mut Store, only: Option<u64>) -> Vec<String> {
    let mut report = Vec::new();
    let ids: Vec<u64> = store
        .schedules
        .iter()
        .filter(|s| s.site == session.site() && only.map_or(s.state == State::Active, |id| s.id == id && s.state != State::Done))
        .map(|s| s.id)
        .collect();
    for id in ids {
        let Some(i) = store.schedules.iter().position(|s| s.id == id) else { continue };
        let mut s = store.schedules[i].clone();
        match run_one(session, &mut s) {
            Ok(line) => report.push(format!("{}: {line}", s.paper_number)),
            Err(e) => {
                let msg = format!("check failed: {e:#}");
                s.note(msg.clone());
                s.set_problem(Some(msg.clone()));
                report.push(format!("{}: {msg}", s.paper_number));
            }
        }
        s.last_check = Some(now());
        store.schedules[i] = s;
        if let Err(e) = store.save() {
            report.push(format!("could not save the schedules: {e:#}"));
        }
    }
    report
}

/// What checking every active schedule would do now, one line per schedule,
/// without sending anything or changing the schedules. The second value is
/// true when at least one invitation or cancellation would be sent.
pub fn preview(session: &mut Session, store: &Store) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut any = false;
    let site = session.site().to_string();
    for s in store.schedules.iter().filter(|s| s.site == site && s.state == State::Active) {
        let line = match current(session, s) {
            Err(e) => format!("cannot check: {e:#}"),
            Ok((_, d)) if d.done => format!("finished, {} of {} accepted", d.counts.accepted, s.needed),
            Ok((_, d)) => {
                let mut parts = Vec::new();
                if !d.cancel.is_empty() {
                    let names = d.cancel.iter().map(|r| r.reviewer.as_str()).collect::<Vec<_>>().join(", ");
                    parts.push(format!("cancel the unanswered invitation of {names}"));
                }
                if !d.invite.is_empty() {
                    parts.push(format!("invite {}", d.invite.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")));
                }
                any |= !parts.is_empty();
                let c = &d.counts;
                let tally = format!("{} accepted, {} awaiting an answer", c.accepted, c.pending);
                if parts.is_empty() { format!("nothing to send ({tally})") } else { format!("{} ({tally})", parts.join("; ")) }
            }
        };
        lines.push(format!("{}: {line}", s.paper_number));
    }
    (lines, any)
}

/// The paper of `s` and what to do for it today.
fn current(session: &mut Session, s: &Schedule) -> Result<(Paper, Decision)> {
    session.with_relogin(|x| x.ensure_editor(&s.journal))?;
    let paper = find_paper(session, &s.paper_id)?;
    let reviews = session.with_relogin(|x| x.load_reviews(&paper))?;
    let d = decide(s, paper.version(), &reviews, Local::now().date_naive());
    Ok((paper, d))
}

fn run_one(session: &mut Session, s: &mut Schedule) -> Result<String> {
    let viewer = session.viewer();
    let today = Local::now().date_naive();
    let (paper, d) = current(session, s)?;
    s.counts = d.counts.clone();
    let c = &d.counts;
    let tally = format!("{} accepted, {} awaiting an answer, {} declined or canceled", c.accepted, c.pending, c.lost);
    if d.done {
        s.state = State::Done;
        s.set_problem(None);
        s.note(format!("Finished: {} of {} needed reviewers accepted", c.accepted, s.needed));
        notify(&s.paper_number, &format!("{} reviewers accepted; automatic invitations finished", c.accepted));
        return Ok(format!("finished ({tally})"));
    }
    let mut problems = Vec::new();
    let mut failed_cancels = 0;
    for r in &d.cancel {
        let days = s.no_answer_days.unwrap_or(0);
        if viewer {
            s.note(format!("Viewer mode — would cancel the unanswered invitation of {} (more than {days} days)", r.reviewer));
            continue;
        }
        match session.with_relogin(|x| x.cancel_review(r)) {
            Ok(_) => {
                s.note(format!("Canceled the invitation of {}: no answer after {days} days", r.reviewer));
            }
            Err(e) => {
                failed_cancels += 1;
                problems.push(format!("could not cancel {}: {e:#}", r.reviewer));
                s.note(format!("Could not cancel the invitation of {}: {e:#}", r.reviewer));
            }
        }
    }
    let n = d.invite.len().saturating_sub(failed_cancels);
    let invite = &d.invite[..n];
    let mut sent = 0;
    if !invite.is_empty() {
        let names = invite.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ");
        if viewer {
            s.note(format!("Viewer mode — would invite {names}"));
        } else {
            let pins: Vec<String> = invite.iter().map(|b| b.pin.clone()).collect();
            let opts = InviteOptions { deadline: None, ..s.invite.clone() };
            match session.with_relogin(|x| x.request_reviews(&paper, &pins, &opts)) {
                Ok(results) => {
                    let mut done = Vec::new();
                    for r in &results {
                        let Some(b) = s.queue.iter_mut().find(|b| b.pin == r.pin) else { continue };
                        if r.ok {
                            b.invited_on = Some(today);
                            done.push(b.name.clone());
                        } else {
                            b.skipped = Some(r.message.clone());
                        }
                    }
                    sent = done.len();
                    if !done.is_empty() {
                        let names = done.join(", ");
                        s.note(format!("Invited {names}"));
                        notify(&s.paper_number, &format!("Invited {names}"));
                    }
                    for r in results.iter().filter(|r| !r.ok) {
                        problems.push(r.message.clone());
                        s.note(format!("Skipped: {}", r.message));
                    }
                }
                Err(e) => {
                    problems.push(format!("could not invite {names}: {e:#}"));
                    s.note(format!("Could not invite {names}: {e:#}"));
                }
            }
        }
    }
    if d.short_by > 0 {
        problems.push(format!("the list is too short: even if everyone left accepts, {} more reviewer(s) are needed — add names", d.short_by));
    }
    s.set_problem(if problems.is_empty() { None } else { Some(problems.join("; ")) });
    Ok(format!("{tally}{}", if sent > 0 { format!("; invited {sent}") } else { String::new() }))
}

fn find_paper(session: &mut Session, id: &str) -> Result<Paper> {
    for all in [false, true] {
        let papers = session.with_relogin(|x| x.load_papers(all))?;
        if let Some(p) = papers.into_iter().find(|p| same_id(&p.id, id)) {
            return Ok(p);
        }
    }
    Err(anyhow!("{id} is no longer in your workspace"))
}

fn now() -> String {
    Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// A macOS notification, so changes are seen without the window open.
fn notify(subtitle: &str, text: &str) {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return;
    }
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"Docket\" subtitle \"{}\"", q(text), q(subtitle));
    let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_status;

    fn review(pin: &str, version: &str, status: &str) -> Review {
        let s = parse_status(status);
        Review {
            paper_id: "26-1".into(),
            paper_title: String::new(),
            reviews_cmd: String::new(),
            version: version.into(),
            reviewer_no: String::new(),
            reviewer: format!("R{pin}"),
            first_name: String::new(),
            last_name: String::new(),
            pin: pin.into(),
            review_id: format!("9{pin}"),
            details_cmd: String::new(),
            status_raw: status.into(),
            state: s.state,
            state_date: s.date,
            due: s.due,
            reminders: 0,
            thanked: false,
            overdue: false,
            recommendation: String::new(),
            from_submission: String::new(),
        }
    }

    fn sched(needed: u32, at_once: u32, pins: &[&str]) -> Schedule {
        Schedule {
            id: 1,
            site: first_site(),
            journal: "tac".into(),
            paper_id: "26-1".into(),
            paper_number: "26-1.1".into(),
            title: String::new(),
            needed,
            at_once,
            no_answer_days: None,
            queue: pins.iter().map(|p| Backup { pin: p.to_string(), name: format!("R{p}"), invited_on: None, skipped: None }).collect(),
            state: State::Active,
            problem: None,
            last_check: None,
            counts: Counts::default(),
            log: Vec::new(),
            invite: InviteOptions::default(),
        }
    }

    fn pins(d: &Decision) -> Vec<&str> {
        d.invite.iter().map(|b| b.pin.as_str()).collect()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn first_round_invites_at_once() {
        let d = decide(&sched(2, 3, &["1", "2", "3", "4", "5"]), "1", &[], day(2026, 11, 3));
        assert_eq!(pins(&d), ["1", "2", "3"]);
        assert_eq!(d.short_by, 0);
    }

    #[test]
    fn a_decline_brings_in_the_next_name() {
        let mut s = sched(2, 3, &["1", "2", "3", "4", "5"]);
        for b in &mut s.queue[..3] {
            b.invited_on = Some(day(2026, 11, 1));
        }
        let reviews = [
            review("1", "1", "Confirmed on November 2, 2026. Due on December 2, 2026"),
            review("2", "1", "Declined on November 2, 2026"),
            review("3", "1", "Requested on November 1, 2026"),
        ];
        let d = decide(&s, "1", &reviews, day(2026, 11, 3));
        assert_eq!((d.counts.accepted, d.counts.pending, d.counts.lost), (1, 1, 1));
        assert_eq!(pins(&d), ["4"]);
    }

    #[test]
    fn hand_made_requests_count_and_old_versions_do_not() {
        let s = sched(2, 2, &["1", "2", "3"]);
        let reviews = [review("77", "2", "Confirmed on November 2, 2026"), review("1", "1", "Received on May 1, 2026")];
        let d = decide(&s, "2", &reviews, day(2026, 11, 3));
        assert_eq!(d.counts.accepted, 1);
        assert_eq!(pins(&d), ["1"], "the v1 reviewer can be asked again for v2");
    }

    #[test]
    fn done_when_enough_accepted() {
        let s = sched(2, 3, &["1", "2", "3"]);
        let reviews = [review("1", "1", "Confirmed on November 2, 2026"), review("2", "1", "Received on November 20, 2026*")];
        let d = decide(&s, "1", &reviews, day(2026, 11, 3));
        assert!(d.done && d.invite.is_empty() && d.cancel.is_empty());
    }

    #[test]
    fn previous_submission_reviews_do_not_count() {
        let s = sched(1, 1, &["1", "2"]);
        let mut old = review("9", "1", "Received on April 6, 2026");
        old.from_submission = "25-5678".into();
        let d = decide(&s, "1", &[old], day(2026, 11, 3));
        assert!(!d.done, "a review of the earlier submission 25-5678 is not one of version 1 of this paper");
        assert_eq!(pins(&d), ["1"]);
    }

    #[test]
    fn unanswered_invitations_are_replaced_only_if_ours() {
        let mut s = sched(1, 1, &["1", "2"]);
        s.no_answer_days = Some(7);
        s.queue[0].invited_on = Some(day(2026, 10, 20));
        let reviews = [review("1", "1", "Requested on October 20, 2026"), review("50", "1", "Requested on October 1, 2026")];
        let d = decide(&s, "1", &reviews, day(2026, 11, 3));
        assert_eq!(d.cancel.iter().map(|r| r.pin.as_str()).collect::<Vec<_>>(), ["1"], "the hand-made request 50 is never canceled");
        assert!(d.invite.is_empty(), "50 is still pending, so the single slot stays taken");
        let d = decide(&s, "1", &reviews[..1], day(2026, 11, 3));
        assert_eq!(pins(&d), ["2"]);
        let d = decide(&s, "1", &reviews[..1], day(2026, 10, 26));
        assert!(d.cancel.is_empty() && d.invite.is_empty(), "not yet 7 days");
    }

    #[test]
    fn short_list_is_reported() {
        let mut s = sched(3, 3, &["1", "2", "3"]);
        for b in &mut s.queue {
            b.invited_on = Some(day(2026, 11, 1));
        }
        let reviews = [review("1", "1", "Declined on November 2, 2026"), review("2", "1", "Confirmed on November 2, 2026"), review("3", "1", "Requested on November 1, 2026")];
        let d = decide(&s, "1", &reviews, day(2026, 11, 3));
        assert!(d.invite.is_empty());
        assert_eq!(d.short_by, 1);
    }

    #[test]
    fn upsert_checks_and_keeps_history() {
        let paper = Paper {
            id: "26-1".into(),
            number: "26-1.1".into(),
            reviews_cmd: String::new(),
            kind: String::new(),
            status: String::new(),
            received: String::new(),
            latest_decision: String::new(),
            next_due: String::new(),
            editor: String::new(),
            handled_by: String::new(),
            authors: String::new(),
            title: "T".into(),
            download_cmd: String::new(),
        };
        let mut st = Store::default();
        let q = |v: &[&str]| v.iter().map(|p| (p.to_string(), format!("R{p}"))).collect::<Vec<_>>();
        assert!(st.upsert(None, &paper, "tac", 3, 2, None, q(&["1"])).is_err());
        assert!(st.upsert(None, &paper, "tac", 1, 1, None, q(&["1", "1"])).is_err());
        let id = st.upsert(None, &paper, "tac", 1, 2, Some(7), q(&["1", "2"])).unwrap();
        assert!(st.upsert(None, &paper, "tac", 1, 2, None, q(&["3"])).is_err(), "one schedule per paper");
        st.schedules[0].queue[0].invited_on = Some(day(2026, 11, 1));
        assert!(st.upsert(Some(id), &paper, "tac", 1, 2, None, q(&["2"])).is_err(), "invited names stay");
        st.upsert(Some(id), &paper, "tac", 2, 2, None, q(&["2", "1", "3"])).unwrap();
        let s = &st.schedules[0];
        assert_eq!(s.queue[1].invited_on, Some(day(2026, 11, 1)));
        assert_eq!((s.needed, s.no_answer_days), (2, None));
    }
}
