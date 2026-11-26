//! Message templates with placeholders such as %reviewerfirstname or
//! %deadline, filled in separately for each review.

use crate::creds;
use crate::model::{Paper, Review, ReviewDates};
use anyhow::{Result, bail};
use chrono::NaiveDate;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Templates {
    /// What %signature stands for, e.g. your name and title.
    #[serde(default)]
    pub signature: String,
    pub list: Vec<Template>,
}

/// Every placeholder, with what it stands for.
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    ("%reviewerfirstname", "reviewer's first name"),
    ("%reviewerlastname", "reviewer's last name"),
    ("%reviewername", "reviewer's full name"),
    ("%reviewerpin", "reviewer's PIN"),
    ("%reviewernumber", "reviewer number on the paper"),
    ("%paper", "paper number with version, e.g. 26-1234.1"),
    ("%paperid", "submission ID, e.g. 26-1234"),
    ("%title", "paper title"),
    ("%authors", "paper authors"),
    ("%version", "version reviewed"),
    ("%papertype", "type of submission, e.g. Technical Note"),
    ("%paperstatus", "status of the paper"),
    ("%decisiondue", "date the decision is due"),
    ("%journal", "journal name"),
    ("%reviewid", "review ID"),
    ("%status", "review status, e.g. Confirmed"),
    ("%deadline", "review due date"),
    ("%daysleft", "days until the due date (0 if past)"),
    ("%daysoverdue", "days past the due date (0 if not late)"),
    ("%delay", "time past the due date, e.g. “12 days”"),
    ("%reminders", "number of reminders sent"),
    ("%requesteddate", "date the review was requested"),
    ("%accepteddate", "date the reviewer accepted"),
    ("%receiveddate", "date the review was received"),
    ("%canceleddate", "date the request was canceled"),
    ("%lastreminderdate", "date of the last reminder"),
    ("%today", "today's date"),
    ("%signature", "your signature (set in the templates window)"),
];

/// Placeholders read from each review's Review Details page.
const FROM_DETAILS: &[&str] = &["%requesteddate", "%accepteddate", "%receiveddate", "%canceleddate", "%lastreminderdate"];

/// Merge fields of earlier versions, still accepted.
const OLD_FIELDS: &[(&str, &str)] = &[
    ("{first}", "%reviewerfirstname"),
    ("{last}", "%reviewerlastname"),
    ("{name}", "%reviewername"),
    ("{paper}", "%paperid"),
    ("{title}", "%title"),
    ("{review_id}", "%reviewid"),
    ("{due}", "%deadline"),
];

pub(crate) static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%[a-z]+").unwrap());

fn path() -> std::path::PathBuf {
    creds::config_dir().join("templates.json")
}

impl Templates {
    pub fn load() -> Templates {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(Templates::defaults)
    }

    pub fn save(&self) -> Result<()> {
        for t in &self.list {
            if t.name.trim().is_empty() {
                bail!("every template needs a name");
            }
            check(&t.subject)?;
            check(&t.body)?;
        }
        let mut names: Vec<&str> = self.list.iter().map(|t| t.name.trim()).collect();
        names.sort();
        if names.windows(2).any(|w| w[0] == w[1]) {
            bail!("two templates have the same name");
        }
        std::fs::create_dir_all(creds::config_dir())?;
        let tmp = path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path())?;
        Ok(())
    }

    pub fn defaults() -> Templates {
        let t = |name: &str, subject: &str, body: &str| Template { name: name.into(), subject: subject.into(), body: body.into() };
        Templates {
            signature: String::new(),
            list: vec![
                t(
                    "Reminder: review overdue",
                    "%journal %paper: your review is overdue",
                    "Dear %reviewerfirstname,\n\nI am writing about your review of the paper %paper, “%title”, which was due on %deadline (%delay ago).\n\nI would be very grateful if you could submit your review as soon as possible, or let me know when you expect to complete it.\n\nThank you very much for your help.\n\nBest regards,\n%signature",
                ),
                t(
                    "Reminder: review due soon",
                    "%journal %paper: your review is due on %deadline",
                    "Dear %reviewerfirstname,\n\nThis is a friendly reminder that your review of the paper %paper, “%title”, is due on %deadline, in %daysleft days.\n\nIf you need more time, please let me know.\n\nThank you very much for your help.\n\nBest regards,\n%signature",
                ),
                t(
                    "Invitation not answered",
                    "%journal %paper: review invitation",
                    "Dear %reviewerfirstname,\n\nOn %requesteddate I invited you to review the paper %paper, “%title”, for %journal, and I have not yet received your answer.\n\nCould you please accept or decline the invitation using the link in the invitation e-mail? If you cannot review it, suggestions of other reviewers are very welcome.\n\nThank you very much.\n\nBest regards,\n%signature",
                ),
                t(
                    "Thank you for your review",
                    "%journal %paper: thank you for your review",
                    "Dear %reviewerfirstname,\n\nThank you very much for your review of the paper %paper, “%title”, received on %receiveddate. Your help is greatly appreciated.\n\nBest regards,\n%signature",
                ),
                t(
                    "Deadline extended",
                    "%journal %paper: review deadline extended",
                    "Dear %reviewerfirstname,\n\nAs requested, the deadline of your review of the paper %paper, “%title”, is now %deadline.\n\nThank you very much for your help.\n\nBest regards,\n%signature",
                ),
                t(
                    "Review no longer needed",
                    "%journal %paper: review no longer needed",
                    "Dear %reviewerfirstname,\n\nEnough reviews of the paper %paper, “%title”, have now been received, so your review is no longer needed and the request has been canceled.\n\nThank you very much for your availability.\n\nBest regards,\n%signature",
                ),
            ],
        }
    }
}

/// Refuse placeholders that do not exist (typos would be sent as written).
pub fn check(text: &str) -> Result<()> {
    let known: Vec<&str> = PLACEHOLDERS.iter().map(|(p, _)| *p).collect();
    let bad: Vec<&str> = WORD.find_iter(text).map(|m| m.as_str()).filter(|w| !known.contains(w)).collect();
    if !bad.is_empty() {
        bail!("unknown placeholder{} {}", if bad.len() == 1 { "" } else { "s" }, bad.join(", "));
    }
    Ok(())
}

/// The text uses dates only shown on Review Details.
pub fn needs_details(text: &str) -> bool {
    FROM_DETAILS.iter().any(|p| WORD.find_iter(text).any(|m| m.as_str() == *p))
}

/// Values of the placeholders for one review.
pub fn values(r: &Review, paper: Option<&Paper>, journal: &str, dates: Option<&ReviewDates>, signature: &str, today: NaiveDate) -> HashMap<&'static str, String> {
    let day = |d: Option<NaiveDate>| d.map(|d| d.format("%B %-d, %Y").to_string()).unwrap_or_default();
    let name = if r.first_name.is_empty() { r.last_name.clone() } else { format!("{} {}", r.first_name, r.last_name) };
    let late = r.due.filter(|_| r.is_open()).map(|d| (today - d).num_days()).filter(|n| *n > 0).unwrap_or(0);
    let left = r.due.map(|d| (d - today).num_days()).filter(|n| *n > 0).unwrap_or(0);
    let p = |f: fn(&Paper) -> &String| paper.map(f).cloned().unwrap_or_default();
    let mut v = HashMap::new();
    v.insert("%reviewerfirstname", if r.first_name.is_empty() { r.last_name.clone() } else { r.first_name.clone() });
    v.insert("%reviewerlastname", r.last_name.clone());
    v.insert("%reviewername", name);
    v.insert("%reviewerpin", r.pin.clone());
    v.insert("%reviewernumber", r.reviewer_no.clone());
    v.insert("%paper", paper.map(|x| x.number.clone()).unwrap_or_else(|| r.paper_id.clone()));
    v.insert("%paperid", r.paper_id.clone());
    v.insert("%title", r.paper_title.clone());
    v.insert("%authors", p(|x| &x.authors));
    v.insert("%version", r.version.clone());
    v.insert("%papertype", p(|x| &x.kind));
    v.insert("%paperstatus", p(|x| &x.status));
    v.insert("%decisiondue", p(|x| &x.next_due));
    v.insert("%journal", journal.to_string());
    v.insert("%reviewid", r.review_id.clone());
    v.insert("%status", r.state.clone());
    v.insert("%deadline", day(r.due));
    v.insert("%daysleft", left.to_string());
    v.insert("%daysoverdue", late.to_string());
    v.insert("%delay", format!("{late} day{}", if late == 1 { "" } else { "s" }));
    v.insert("%reminders", r.reminders.to_string());
    v.insert("%requesteddate", day(dates.and_then(|d| d.requested)));
    v.insert("%accepteddate", day(dates.and_then(|d| d.confirmed)));
    v.insert("%receiveddate", day(dates.and_then(|d| d.submitted)));
    v.insert("%canceleddate", day(dates.and_then(|d| d.canceled)));
    v.insert("%lastreminderdate", day(dates.and_then(|d| d.last_reminder)));
    v.insert("%today", day(Some(today)));
    v.insert("%signature", signature.to_string());
    v
}

/// Fill the placeholders of `text`.
pub fn fill(text: &str, values: &HashMap<&'static str, String>) -> String {
    let mut text = text.to_string();
    for (old, new) in OLD_FIELDS {
        text = text.replace(old, new);
    }
    WORD.replace_all(&text, |m: &regex::Captures| values.get(&m[0]).cloned().unwrap_or_else(|| m[0].to_string())).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> Review {
        Review {
            paper_id: "26-1234".into(),
            paper_title: "A Study of Something".into(),
            reviews_cmd: String::new(),
            version: "1".into(),
            reviewer_no: "4".into(),
            reviewer: "Doe, Jane".into(),
            first_name: "Jane".into(),
            last_name: "Doe".into(),
            pin: "12345".into(),
            review_id: "400001".into(),
            details_cmd: String::new(),
            status_raw: String::new(),
            state: "Confirmed".into(),
            state_date: None,
            due: NaiveDate::from_ymd_opt(2026, 10, 6),
            reminders: 2,
            thanked: true,
            overdue: true,
            recommendation: String::new(),
            from_submission: String::new(),
        }
    }

    #[test]
    fn fills_and_checks() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let dates = ReviewDates { requested: NaiveDate::from_ymd_opt(2026, 9, 1), ..Default::default() };
        let v = values(&review(), None, "IEEE-TAC", Some(&dates), "John Smith", today);
        let t = &Templates::defaults().list[0];
        assert_eq!(fill(&t.subject, &v), "IEEE-TAC 26-1234: your review is overdue");
        let body = fill(&t.body, &v);
        assert!(body.starts_with("Dear Jane,\n\nI am writing about your review of the paper 26-1234, “A Study of Something”, which was due on October 6, 2026 (29 days ago)."), "{body}");
        assert!(body.ends_with("Best regards,\nJohn Smith"));
        assert_eq!(fill("{first} %requesteddate, 50% done, %reviewername", &v), "Jane September 1, 2026, 50% done, Jane Doe");
        assert!(check("Dear %reviwerfirstname").is_err(), "typos are refused");
        assert!(check("50% of the paper, %title").is_ok());
        assert!(needs_details("since %requesteddate") && !needs_details("by %deadline"));
        for t in Templates::defaults().list {
            check(&t.subject).unwrap();
            check(&t.body).unwrap();
        }
    }
}
