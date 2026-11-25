//! Site notes: thank-you notes, confirmation requests and review reminders
//! sent by PaperPlaza itself, from its "Reminders and thanks" pages and with
//! the site's own letters, so they are recorded there like notes sent by hand.
//! Nothing here is sent as a personal message.
//!
//! Checked with the automatic invitations (Settings → Automation: how often,
//! at which hours), in the journals where they are turned on. Each kind of
//! note is either sent right away or held until you approve it ("ask me
//! first"). Safeguards:
//! - thank-you notes: only for acceptances and reviews dated from the day the
//!   note was turned on; the site lists only reviews it has not thanked;
//! - confirmation requests and reminders: at most `max` per review, at least
//!   `after_days` apart, counting the site's own record of earlier ones;
//! - every attempt is recorded; one that cannot be confirmed afterwards on the
//!   site's list counts as sent, so it is never repeated sooner.

use crate::activity;
use crate::creds;
use crate::model::{SiteItem, Unthanked};
use crate::papercept::{Session, SiteList};
use crate::scheduler::{Notice, notify};
use anyhow::Result;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Thank-you note for agreeing to review.
    Accept,
    /// Thank-you note for submitting a review.
    Review,
    /// Request to confirm (accept or decline) a review request.
    Confirm,
    /// Reminder of an overdue review.
    Remind,
}

impl Kind {
    pub fn what(self) -> &'static str {
        match self {
            Kind::Accept => "thank-you note for agreeing to review",
            Kind::Review => "thank-you note for the review",
            Kind::Confirm => "confirmation request",
            Kind::Remind => "review reminder",
        }
    }

    fn activity(self) -> &'static str {
        match self {
            Kind::Accept | Kind::Review => "thanks",
            Kind::Confirm => "confirmation",
            Kind::Remind => "reminder",
        }
    }
}

/// One of the two thank-you notes: whether it is sent, and from when.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub on: bool,
    /// The day it was turned on: earlier events are not thanked.
    pub since: Option<NaiveDate>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThanksSettings {
    pub accept: Rule,
    pub review: Rule,
    /// The site's "Send me a copy of each thank you note".
    pub copy_me: bool,
    /// Hold the notes until approved.
    pub ask: bool,
}

/// Confirmation requests or overdue reminders.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Repeat {
    pub on: bool,
    /// Confirmations: days after the request, and between requests.
    /// Reminders: days after the due date, and between reminders.
    pub after_days: u32,
    /// At most this many per review (the site's earlier ones count).
    pub max: u32,
    pub copy_me: bool,
    /// The site's "Attach the submission".
    pub attach: bool,
    pub ask: bool,
}

impl Default for Repeat {
    fn default() -> Self {
        Repeat { on: false, after_days: 7, max: 2, copy_me: false, attach: false, ask: true }
    }
}

/// A journal where the notes are sent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Place {
    pub site: String,
    pub journal: String,
    #[serde(default)]
    pub name: String,
    pub since: NaiveDate,
}

/// A note sent, or attempted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Done {
    pub site: String,
    pub review_id: String,
    pub kind: Kind,
    /// "YYYY-MM-DD HH:MM".
    pub at: String,
    pub paper: String,
    pub reviewer: String,
    pub outcome: String,
}

/// A note held until approved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub key: String,
    pub site: String,
    pub journal: String,
    pub kind: Kind,
    pub review_id: String,
    pub paper: String,
    pub reviewer: String,
    /// Why it is due (e.g. "12 days overdue").
    pub why: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    pub journals: Vec<Place>,
    pub thanks: ThanksSettings,
    pub confirm: Repeat,
    pub remind: Repeat,
    pub done: Vec<Done>,
    pub pending: Vec<Pending>,
}

impl Default for Notes {
    fn default() -> Self {
        Notes {
            journals: Vec::new(),
            thanks: ThanksSettings::default(),
            confirm: Repeat::default(),
            remind: Repeat { max: 3, ..Repeat::default() },
            done: Vec::new(),
            pending: Vec::new(),
        }
    }
}

/// A note that is due: `key` identifies it for approval, `field` is its box
/// on the site's page (thanks) or the review ID (reminder lists).
#[derive(Clone, Debug)]
pub struct Due {
    pub key: String,
    pub kind: Kind,
    pub review_id: String,
    pub paper: String,
    pub reviewer: String,
    pub field: String,
    pub why: String,
}

fn key(site: &str, kind: Kind, review_id: &str) -> String {
    format!("{site}|{kind:?}|{review_id}")
}

fn path() -> std::path::PathBuf {
    creds::config_dir().join("notes.json")
}

impl Notes {
    pub fn load() -> Notes {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(creds::config_dir())?;
        let tmp = path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path())?;
        Ok(())
    }

    fn any_on(&self) -> bool {
        self.thanks.accept.on || self.thanks.review.on || self.confirm.on || self.remind.on
    }

    /// Some note is sent for some journal of `site`.
    pub fn is_on(&self, site: &str) -> bool {
        self.any_on() && self.journals.iter().any(|p| p.site == site)
    }

    pub fn place(&self, site: &str, journal: &str) -> Option<&Place> {
        self.journals.iter().find(|p| p.site == site && p.journal.eq_ignore_ascii_case(journal))
    }

    /// Change the settings. Turning a thank-you note or a journal on starts it
    /// `today`, so earlier acceptances and reviews are not thanked.
    pub fn set(&mut self, thanks: ThanksSettings, confirm: Repeat, remind: Repeat, here: Option<(&str, &str, &str, bool)>, today: NaiveDate) {
        for (rule, new) in [(&mut self.thanks.accept, &thanks.accept), (&mut self.thanks.review, &thanks.review)] {
            if new.on && !rule.on {
                rule.since = Some(today);
            }
            rule.on = new.on;
        }
        self.thanks.copy_me = thanks.copy_me;
        self.thanks.ask = thanks.ask;
        let clamp = |mut r: Repeat| {
            r.after_days = r.after_days.clamp(1, 90);
            r.max = r.max.clamp(1, 10);
            r
        };
        self.confirm = clamp(confirm);
        self.remind = clamp(remind);
        if let Some((site, journal, name, on)) = here {
            let i = self.journals.iter().position(|p| p.site == site && p.journal.eq_ignore_ascii_case(journal));
            match (i, on) {
                (None, true) => self.journals.push(Place { site: site.into(), journal: journal.into(), name: name.into(), since: today }),
                (Some(i), false) => {
                    self.journals.remove(i);
                }
                _ => {}
            }
        }
        // Held notes of what is now off, or of a journal now off, go.
        let keep: Vec<bool> = self.pending.iter().map(|p| self.kind_on(p.kind) && self.place(&p.site, &p.journal).is_some()).collect();
        let mut k = keep.iter();
        self.pending.retain(|_| *k.next().unwrap_or(&false));
    }

    /// Remove a journal (from the list of others in the settings).
    pub fn remove_place(&mut self, site: &str, journal: &str) {
        self.journals.retain(|p| !(p.site == site && p.journal.eq_ignore_ascii_case(journal)));
        self.pending.retain(|p| !(p.site == site && p.journal.eq_ignore_ascii_case(journal)));
    }

    fn kind_on(&self, kind: Kind) -> bool {
        match kind {
            Kind::Accept => self.thanks.accept.on,
            Kind::Review => self.thanks.review.on,
            Kind::Confirm => self.confirm.on,
            Kind::Remind => self.remind.on,
        }
    }

    fn ask(&self, kind: Kind) -> bool {
        match kind {
            Kind::Accept | Kind::Review => self.thanks.ask,
            Kind::Confirm => self.confirm.ask,
            Kind::Remind => self.remind.ask,
        }
    }

    fn attempts(&self, site: &str, review_id: &str, kind: Kind) -> Vec<NaiveDate> {
        self.done
            .iter()
            .filter(|d| d.site == site && d.review_id == review_id && d.kind == kind)
            .filter_map(|d| NaiveDate::parse_from_str(d.at.get(..10)?, "%Y-%m-%d").ok())
            .collect()
    }

    /// The thank-you notes of `place` to send now, from the site's list of
    /// reviews not yet thanked.
    pub fn due_thanks(&self, place: &Place, list: &[Unthanked]) -> Vec<Due> {
        let mut out = Vec::new();
        for u in list {
            let s = u.state.to_lowercase();
            let (kind, field, rule) = if s.starts_with("confirm") {
                (Kind::Accept, &u.accept_box, &self.thanks.accept)
            } else if s.starts_with("receiv") || s.starts_with("submit") {
                (Kind::Review, &u.review_box, &self.thanks.review)
            } else {
                continue;
            };
            let (Some(field), Some(on), Some(since)) = (field, u.date, rule.since) else { continue };
            if rule.on && on >= since && on >= place.since && self.attempts(&place.site, &u.review_id, kind).is_empty() {
                out.push(Due {
                    key: key(&place.site, kind, &u.review_id),
                    kind,
                    review_id: u.review_id.clone(),
                    paper: u.paper.clone(),
                    reviewer: u.reviewer.clone(),
                    field: field.clone(),
                    why: u.status_raw.clone(),
                });
            }
        }
        out
    }

    /// The confirmation requests or reminders of `place` to send now.
    pub fn due_repeat(&self, place: &Place, kind: Kind, list: &[SiteItem], today: NaiveDate) -> Vec<Due> {
        let r = if kind == Kind::Confirm { &self.confirm } else { &self.remind };
        let mut out = Vec::new();
        if !r.on {
            return out;
        }
        for it in list {
            let Some(days) = it.days else { continue };
            if days < r.after_days as i64 {
                continue;
            }
            let ours = self.attempts(&place.site, &it.review_id, kind);
            // Reminders sent before the due date do not count toward the overdue ones.
            let site_count = it.previous.iter().filter(|d| kind == Kind::Confirm || it.due.is_none_or(|due| **d > due)).count();
            if site_count.max(ours.len()) >= r.max as usize {
                continue;
            }
            let last = it.previous.iter().chain(ours.iter()).max();
            if last.is_some_and(|l| (today - *l).num_days() < r.after_days as i64) {
                continue;
            }
            let why = if kind == Kind::Confirm { format!("no answer after {days} days") } else { format!("{days} days overdue") };
            let n = site_count.max(ours.len());
            out.push(Due {
                key: key(&place.site, kind, &it.review_id),
                kind,
                review_id: it.review_id.clone(),
                paper: it.paper.clone(),
                reviewer: it.reviewer.clone(),
                field: it.review_id.clone(),
                why: if n > 0 { format!("{why}; {n} sent before") } else { why },
            });
        }
        out
    }

    fn record(&mut self, site: &str, journal: &str, d: &Due, outcome: &str, ok: bool) {
        self.done.push(Done {
            site: site.into(),
            review_id: d.review_id.clone(),
            kind: d.kind,
            at: Local::now().format("%Y-%m-%d %H:%M").to_string(),
            paper: d.paper.clone(),
            reviewer: d.reviewer.clone(),
            outcome: outcome.into(),
        });
        activity::add(site, journal, &d.paper, d.kind.activity(), &d.reviewer, &format!("{}: {outcome}", d.kind.what()), ok);
    }
}

/// Check every journal of the session's site where notes are on: send the
/// notes that are due (or, with `approved`, only those approved), and hold
/// the others that ask first. Returns one line per note or problem. In viewer
/// mode nothing is sent or recorded.
pub fn run(session: &mut Session, n: &mut Notes, approved: Option<&[String]>) -> Vec<String> {
    let site = session.site().to_string();
    let mut report = Vec::new();
    let places: Vec<Place> = n.journals.iter().filter(|p| p.site == site).cloned().collect();
    if !n.any_on() {
        return report;
    }
    let before: Vec<String> = n.pending.iter().map(|p| p.key.clone()).collect();
    let today = Local::now().date_naive();
    for place in places {
        let label = if place.name.is_empty() { place.journal.clone() } else { place.name.clone() };
        if let Err(e) = session.with_relogin(|x| x.ensure_editor(&place.journal)) {
            report.push(format!("{label}: cannot open the Associate Editor workspace ({e:#})"));
            continue;
        }
        let journal = session.journal.as_ref().map(|j| j.name.clone()).unwrap_or(label.clone());
        let mut held: Vec<Due> = Vec::new();
        let mut checked: Vec<Kind> = Vec::new();

        if n.thanks.accept.on || n.thanks.review.on {
            match session.with_relogin(|x| x.unthanked()) {
                Err(e) => report.push(format!("{journal}: cannot read the site's Send Thank You Notes page ({e:#})")),
                Ok(list) => {
                    checked.extend([Kind::Accept, Kind::Review]);
                    let due = n.due_thanks(&place, &list);
                    let send = pick(n, due, approved, &mut held);
                    if !send.is_empty() {
                        send_thanks(session, n, &site, &journal, &send, &mut report);
                    }
                }
            }
        }
        for (which, kind) in [(SiteList::Confirmations, Kind::Confirm), (SiteList::Reminders, Kind::Remind)] {
            if !n.kind_on(kind) {
                continue;
            }
            match session.with_relogin(|x| x.site_list(which)) {
                Err(e) => report.push(format!("{journal}: cannot read the site's {} list ({e:#})", kind.what())),
                Ok(list) => {
                    checked.push(kind);
                    let due = n.due_repeat(&place, kind, &list, today);
                    let send = pick(n, due, approved, &mut held);
                    if !send.is_empty() {
                        send_repeat(session, n, &site, &journal, which, &list, &send, &mut report);
                    }
                }
            }
        }
        // A regular check replaces this journal's held notes (for the kinds read
        // now) with those just found; an approval only removes what was sent.
        let sent_keys: Vec<String> = n.done.iter().map(|d| key(&d.site, d.kind, &d.review_id)).collect();
        if approved.is_none() {
            n.pending.retain(|p| !(p.site == site && p.journal.eq_ignore_ascii_case(&place.journal) && checked.contains(&p.kind)));
            for d in held {
                n.pending.push(Pending {
                    key: d.key,
                    site: site.clone(),
                    journal: place.journal.clone(),
                    kind: d.kind,
                    review_id: d.review_id,
                    paper: d.paper,
                    reviewer: d.reviewer,
                    why: d.why,
                });
            }
        } else {
            n.pending.retain(|p| !sent_keys.contains(&p.key));
        }
    }
    let new = n.pending.iter().filter(|p| !before.contains(&p.key)).count();
    if new > 0 && !session.viewer() {
        let total = n.pending.len();
        notify(Notice::Notes, "Notes waiting for your approval", &format!("{total} site note{} to approve: menu-bar icon → Send Pending Notes…", if total == 1 { "" } else { "s" }));
    }
    if let Err(e) = n.save() {
        report.push(format!("could not save the site notes: {e:#}"));
    }
    report
}

/// The notes of `due` to send now; the others that ask first go to `held`.
/// With `approved`, only those approved are sent, and nothing new is held.
fn pick(n: &Notes, due: Vec<Due>, approved: Option<&[String]>, held: &mut Vec<Due>) -> Vec<Due> {
    let mut send = Vec::new();
    for d in due {
        match approved {
            Some(keys) if keys.contains(&d.key) => send.push(d),
            Some(_) => {}
            None if n.ask(d.kind) => held.push(d),
            None => send.push(d),
        }
    }
    send
}

fn send_thanks(session: &mut Session, n: &mut Notes, site: &str, journal: &str, send: &[Due], report: &mut Vec<String>) {
    if session.viewer() {
        report.extend(send.iter().map(|d| format!("{}: viewer mode — would send a {} to {}", d.paper, d.kind.what(), d.reviewer)));
        return;
    }
    let boxes: Vec<String> = send.iter().map(|d| d.field.clone()).collect();
    let copy = n.thanks.copy_me;
    match session.with_relogin(|x| x.send_thank_you_notes(&boxes, copy)) {
        // Nothing was sent: tried again at the next check.
        Err(e) => report.push(format!("{journal}: thank-you notes not sent ({e:#})")),
        Ok(after) => {
            let still = |d: &Due| {
                after.remaining.as_ref().map(|l| l.iter().any(|u| u.accept_box.as_deref() == Some(d.field.as_str()) || u.review_box.as_deref() == Some(d.field.as_str())))
            };
            let results: Vec<(Due, Option<bool>)> = send.iter().map(|d| (d.clone(), still(d))).collect();
            finish(n, site, journal, results, after.problem, "the site still lists it as not thanked", report);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn send_repeat(session: &mut Session, n: &mut Notes, site: &str, journal: &str, which: SiteList, list: &[SiteItem], send: &[Due], report: &mut Vec<String>) {
    if session.viewer() {
        report.extend(send.iter().map(|d| format!("{}: viewer mode — would send a {} to {} ({})", d.paper, d.kind.what(), d.reviewer, d.why)));
        return;
    }
    let r = if which == SiteList::Confirmations { &n.confirm } else { &n.remind };
    let (copy, attach) = (r.copy_me, r.attach);
    let ids: Vec<String> = send.iter().map(|d| d.review_id.clone()).collect();
    let earlier: HashMap<&str, usize> = list.iter().map(|i| (i.review_id.as_str(), i.previous.len())).collect();
    match session.with_relogin(|x| x.send_site_list(which, &ids, copy, attach)) {
        Err(e) => report.push(format!("{journal}: {}s not sent ({e:#})", send[0].kind.what())),
        Ok(after) => {
            // Sent when the site's list shows one more earlier reminder (or request) for the review.
            let still = |d: &Due| {
                after.remaining.as_ref().map(|l| match l.iter().find(|i| i.review_id == d.review_id) {
                    Some(i) => i.previous.len() <= earlier.get(d.review_id.as_str()).copied().unwrap_or(0),
                    None => true,
                })
            };
            let results: Vec<(Due, Option<bool>)> = send.iter().map(|d| (d.clone(), still(d))).collect();
            finish(n, site, journal, results, after.problem, "the site does not show it as sent", report);
        }
    }
}

/// Record what was sent (`Some(false)`: no longer pending on the site's list)
/// and what could not be confirmed.
fn finish(n: &mut Notes, site: &str, journal: &str, results: Vec<(Due, Option<bool>)>, problem: Option<String>, not_shown: &str, report: &mut Vec<String>) {
    let mut ok = 0;
    for (d, still) in &results {
        let outcome = match (still, &problem) {
            (Some(false), _) => "sent".to_string(),
            (Some(true), _) => format!("not confirmed: {not_shown}"),
            (None, p) => format!("not confirmed: {}", p.as_deref().unwrap_or("unknown")),
        };
        let sent = outcome == "sent";
        n.record(site, journal, d, &outcome, sent);
        if sent {
            ok += 1;
            report.push(format!("{}: sent a {} to {}", d.paper, d.kind.what(), d.reviewer));
        } else {
            report.push(format!("{}: {} to {} — {outcome}; it counts as sent, check on PaperPlaza", d.paper, d.kind.what(), d.reviewer));
            notify(Notice::Problems, &d.paper, &format!("{} to {}: {outcome}", d.kind.what(), d.reviewer));
        }
    }
    if ok > 0 {
        let kind = results[0].0.kind.what();
        notify(Notice::Notes, journal, &format!("Sent {ok} {kind}{}", if ok == 1 { "" } else { "s" }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_status;

    fn row(id: &str, status: &str, accept: bool, review: bool) -> Unthanked {
        let s = parse_status(status);
        Unthanked {
            review_id: id.into(),
            status_raw: status.into(),
            state: s.state,
            date: s.date,
            reviewer: format!("Doe, Jane ({id})"),
            paper: "26-1234.1".into(),
            accept_box: accept.then(|| format!("ThankYouForAccepting{id}")),
            review_box: review.then(|| format!("ThankYouForSubmitting{id}")),
        }
    }

    fn item(id: &str, days: i64, due: Option<NaiveDate>, previous: &[NaiveDate]) -> SiteItem {
        SiteItem {
            review_id: id.into(),
            checkbox: format!("Include{id}"),
            reviewer: format!("Doe, Jane ({id})"),
            paper: "26-1234.1".into(),
            status_raw: String::new(),
            due,
            days: Some(days),
            previous: previous.to_vec(),
        }
    }

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn fields(v: &[Due]) -> Vec<String> {
        v.iter().map(|d| d.field.clone()).collect()
    }

    fn thanks(accept: bool, review: bool) -> ThanksSettings {
        ThanksSettings { accept: Rule { on: accept, since: None }, review: Rule { on: review, since: None }, ..Default::default() }
    }

    fn here(on: bool) -> Option<(&'static str, &'static str, &'static str, bool)> {
        Some(("css.paperplaza.net", "tac", "IEEE-TAC", on))
    }

    fn done(site: &str, kind: Kind, id: &str, on: NaiveDate) -> Done {
        Done { site: site.into(), review_id: id.into(), kind, at: format!("{on} 10:00"), paper: String::new(), reviewer: String::new(), outcome: "sent".into() }
    }

    #[test]
    fn thanks_only_for_new_events_and_once() {
        let mut n = Notes::default();
        n.set(thanks(true, true), Repeat::default(), Repeat::default(), here(true), day(11, 10));
        assert!(n.is_on("css.paperplaza.net") && !n.is_on("ifac-sms.org"));
        let place = n.place("css.paperplaza.net", "TAC").unwrap().clone();
        let list = [
            row("1", "Confirmed on November 12, 2026. Due on December 12, 2026", true, false),
            row("2", "Confirmed on November 2, 2026. Due on December 2, 2026", true, false),
            row("3", "Received on November 20, 2026", true, true),
            row("4", "Confirmed on November 12, 2026", false, false),
        ];
        assert_eq!(fields(&n.due_thanks(&place, &list)), ["ThankYouForAccepting1", "ThankYouForSubmitting3"], "2 accepted before; 3 only for the review");
        n.done.push(done("css.paperplaza.net", Kind::Accept, "1", day(11, 12)));
        assert_eq!(fields(&n.due_thanks(&place, &list)), ["ThankYouForSubmitting3"], "never twice");
        let later = [row("1", "Received on November 25, 2026", false, true)];
        assert_eq!(fields(&n.due_thanks(&place, &later)), ["ThankYouForSubmitting1"], "the review is thanked separately");
    }

    #[test]
    fn turning_on_starts_from_today() {
        let mut n = Notes::default();
        n.set(thanks(false, true), Repeat::default(), Repeat::default(), here(true), day(11, 1));
        n.set(thanks(true, true), Repeat::default(), Repeat::default(), None, day(11, 15));
        assert_eq!((n.thanks.accept.since, n.thanks.review.since), (Some(day(11, 15)), Some(day(11, 1))), "turning one on does not move the other");
        let place = n.place("css.paperplaza.net", "tac").unwrap().clone();
        let list = [row("1", "Confirmed on November 12, 2026", true, false), row("2", "Received on November 12, 2026", false, true)];
        assert_eq!(fields(&n.due_thanks(&place, &list)), ["ThankYouForSubmitting2"]);
        n.set(thanks(true, true), Repeat::default(), Repeat::default(), here(false), day(11, 16));
        assert!(!n.is_on("css.paperplaza.net"));
        n.set(thanks(true, true), Repeat::default(), Repeat::default(), here(true), day(11, 20));
        let place = n.place("css.paperplaza.net", "tac").unwrap().clone();
        assert!(n.due_thanks(&place, &list).is_empty(), "the journal turned on again starts again from that day");
    }

    #[test]
    fn confirmation_requests_spaced_and_limited() {
        let mut n = Notes::default();
        let on = Repeat { on: true, after_days: 7, max: 2, ..Repeat::default() };
        n.set(ThanksSettings::default(), on, Repeat::default(), here(true), day(11, 1));
        let place = n.place("css.paperplaza.net", "tac").unwrap().clone();
        let today = day(11, 25);
        let list = [
            item("1", 6, None, &[]),
            item("2", 10, None, &[]),
            item("3", 20, None, &[day(11, 20)]),
            item("4", 20, None, &[day(11, 10)]),
            item("5", 30, None, &[day(11, 1), day(11, 10)]),
        ];
        assert_eq!(fields(&n.due_repeat(&place, Kind::Confirm, &list, today)), ["2", "4"], "1 too recent; 3 asked 5 days ago; 5 asked twice already");
        n.done.push(done("css.paperplaza.net", Kind::Confirm, "2", day(11, 24)));
        assert_eq!(fields(&n.due_repeat(&place, Kind::Confirm, &list, today)), ["4"], "Docket's own request counts too");
        assert!(n.due_repeat(&place, Kind::Remind, &list, today).is_empty(), "reminders are off");
    }

    #[test]
    fn reminders_after_the_due_date() {
        let mut n = Notes::default();
        let on = Repeat { on: true, after_days: 7, max: 2, ..Repeat::default() };
        n.set(ThanksSettings::default(), Repeat::default(), on, here(true), day(11, 1));
        let place = n.place("css.paperplaza.net", "tac").unwrap().clone();
        let today = day(11, 25);
        let due = Some(day(11, 10));
        let list = [
            item("1", -3, due, &[]),
            item("2", 5, due, &[]),
            item("3", 15, due, &[day(11, 5)]),
            item("4", 15, due, &[day(11, 20)]),
            item("5", 15, due, &[day(11, 12), day(11, 17)]),
        ];
        assert_eq!(fields(&n.due_repeat(&place, Kind::Remind, &list, today)), ["3"], "1 not due; 2 only 5 days late; 4 reminded 5 days ago; 5 reminded twice since the due date");
        let d = &n.due_repeat(&place, Kind::Remind, &list, today)[0];
        assert_eq!(d.why, "15 days overdue", "the reminder before the due date does not count");
    }

    #[test]
    fn asking_first_holds_the_notes() {
        let mut n = Notes::default();
        n.thanks.ask = true;
        n.remind.ask = false;
        let d = |k: Kind, id: &str| Due { key: key("s", k, id), kind: k, review_id: id.into(), paper: String::new(), reviewer: String::new(), field: id.into(), why: String::new() };
        let mut held = Vec::new();
        let send = pick(&n, vec![d(Kind::Accept, "1"), d(Kind::Remind, "2")], None, &mut held);
        assert_eq!((fields(&send), fields(&held)), (vec!["2".to_string()], vec!["1".to_string()]));
        let approved = [key("s", Kind::Accept, "1")];
        let mut held = Vec::new();
        let send = pick(&n, vec![d(Kind::Accept, "1"), d(Kind::Accept, "3")], Some(&approved), &mut held);
        assert_eq!(fields(&send), ["1"]);
        assert!(held.is_empty(), "approving sends only what was approved");
    }
}
