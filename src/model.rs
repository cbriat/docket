use chrono::NaiveDate;
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

#[derive(Clone, Debug, Serialize)]
pub struct Journal {
    pub code: String,
    pub name: String,
    pub description: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleKind {
    /// Associate Editor and similar: workspace.pl.
    Editor,
    /// Reviewer's workspace: reviewerspace.pl.
    Reviewer,
}

#[derive(Clone, Debug, Serialize)]
pub struct Role {
    pub label: String,
    pub command: String,
    pub kind: RoleKind,
}

impl Paper {
    /// Current version, from the number "26-1234.1".
    pub fn version(&self) -> &str {
        self.number.rsplit_once('.').map(|(_, v)| v).unwrap_or("")
    }
}

/// Choices on the page that sends review requests.
#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq)]
pub struct InviteOptions {
    /// None keeps the site's deadline (35 days for TAC).
    #[serde(default)]
    pub deadline: Option<NaiveDate>,
    /// Attach the manuscript files to the request e-mail.
    #[serde(default)]
    pub attach: bool,
    /// Send me a copy of each message.
    #[serde(default)]
    pub copy: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct InviteResult {
    pub pin: String,
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// A name from your Reviewers list on the Request a Review page.
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub pin: String,
    pub name: String,
    pub affiliation: String,
    pub email: String,
    pub keywords: String,
    /// The paper's keywords found in the reviewer's keywords.
    pub matches: Vec<String>,
    /// False when already approached, author of the paper, or PIN de-activated.
    pub available: bool,
    /// On your Reviewers list (false: found in the PIN database only).
    pub in_list: bool,
    /// Recommendation score 0–100 (see recommend.rs).
    pub score: u32,
    pub reasons: Vec<String>,
    /// Possible conflict of interest.
    pub conflict: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Author {
    pub name: String,
    pub affiliation: String,
    pub pin: String,
}

/// What the Request a Review page says about the paper.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PaperInfo {
    pub title: String,
    pub keywords: Vec<String>,
    pub authors: Vec<Author>,
    /// Empty until the submission details page has been captured.
    pub abstract_text: String,
}

/// Dates of a review shown on its Review Details page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ReviewDates {
    pub requested: Option<NaiveDate>,
    pub confirmed: Option<NaiveDate>,
    pub submitted: Option<NaiveDate>,
    pub canceled: Option<NaiveDate>,
    pub last_reminder: Option<NaiveDate>,
}

/// One answer in a review form ("Comments to the author", …).
#[derive(Clone, Debug, Serialize)]
pub struct Answer {
    pub question: String,
    pub answer: String,
}

/// A submitted review as the editor sees it on Review Details.
#[derive(Clone, Debug, Serialize)]
pub struct ReviewText {
    pub review_id: String,
    pub reviewer: String,
    pub form: String,
    pub answers: Vec<Answer>,
    /// Files attached to the review (PDF of the comments, …).
    pub attachments: Vec<FileLink>,
}

/// An editor listed on a paper's Contact page ("Handlers of the submission").
#[derive(Clone, Debug, Serialize)]
pub struct Handler {
    pub name: String,
    /// "Senior Editor", "Associate Editor", …
    pub role: String,
    pub pin: String,
    pub email: String,
    /// What the page puts in the To field: "Jane Doe <jane.doe@example.org>".
    pub address: String,
}

/// A file the site offers for download, opened with `command`.
#[derive(Clone, Debug, Serialize)]
pub struct FileLink {
    /// "Attachment to the Comments to the Author", "Manuscript", …
    pub label: String,
    /// "Pdf file, 573 KB", …
    pub about: String,
    pub command: String,
}

/// The reviews of a paper a reviewer may read ("Inspect the reviews").
#[derive(Clone, Debug, Default, Serialize)]
pub struct Inspection {
    pub number: String,
    pub title: String,
    pub authors: String,
    pub current_version: String,
    pub current_status: String,
    pub kind: String,
    pub versions: Vec<VersionReviews>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VersionReviews {
    pub heading: String,
    pub reviews: Vec<SharedReview>,
    pub decisions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SharedReview {
    pub review_id: String,
    pub reviewer_no: String,
    pub recommendation: String,
    pub comments: String,
    /// Your own review (shown in bold and linked on the site).
    pub mine: bool,
}

/// One row of the Reviewer's workspace: a review requested from you.
#[derive(Clone, Debug, Serialize)]
pub struct Assignment {
    pub journal: String,
    pub review_id: String,
    pub reviewer_no: String,
    pub requested_by: String,
    /// "Associate Editor, 12345"
    pub requester_role: String,
    pub requester_email: String,
    pub authors: String,
    pub title: String,
    pub submission_id: String,
    pub number: String,
    pub version: String,
    pub kind: String,
    pub status_raw: String,
    pub state: String,
    pub state_date: Option<NaiveDate>,
    pub due: Option<NaiveDate>,
    pub reminders: u8,
    pub overdue: bool,
    pub recommendation: String,
    /// Links in the row other than the title and "Inspect" (not yet supported).
    pub other_actions: Vec<String>,
    /// "Inspect the reviews" is offered (your review was submitted).
    pub can_inspect: bool,
    /// The title link: the submission data and files popup.
    pub details_cmd: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Paper {
    /// Submission ID as used in hidden fields, e.g. "26-1234".
    pub id: String,
    /// Number with version, e.g. "26-1234.1".
    pub number: String,
    pub reviews_cmd: String,
    pub kind: String,
    pub status: String,
    pub received: String,
    pub latest_decision: String,
    pub next_due: String,
    pub editor: String,
    pub handled_by: String,
    pub authors: String,
    pub title: String,
    /// The title link of the workspace: downloads the manuscript of this version.
    pub download_cmd: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Review {
    pub paper_id: String,
    pub paper_title: String,
    pub reviews_cmd: String,
    pub version: String,
    pub reviewer_no: String,
    pub reviewer: String,
    pub first_name: String,
    pub last_name: String,
    pub pin: String,
    pub review_id: String,
    pub details_cmd: String,
    pub status_raw: String,
    pub state: String,
    pub state_date: Option<NaiveDate>,
    pub due: Option<NaiveDate>,
    pub reminders: u8,
    pub thanked: bool,
    pub overdue: bool,
    pub recommendation: String,
    /// Empty for the paper's own reviews; the earlier submission's number
    /// (e.g. "25-5678") for the reviews listed under "Previous Submission".
    pub from_submission: String,
}

static NAME_PIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.*?)\s*\((\d+)\)\s*$").unwrap());
static STATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([A-Za-z][A-Za-z ]*?)\s+on\s+([A-Z][a-z]+ \d{1,2}, \d{4})(\**)(TH)?").unwrap());
static DUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Due on ([A-Z][a-z]+ \d{1,2}, \d{4})").unwrap());

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%B %d, %Y").ok()
}

/// "Doe, Jane (12345)" -> ("Doe, Jane", "12345")
pub fn split_name_pin(s: &str) -> (String, String) {
    match NAME_PIN.captures(s.trim()) {
        Some(c) => (c[1].trim().to_string(), c[2].to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}

/// "Doe, Jane" -> ("Jane", "Doe")
pub fn split_first_last(name: &str) -> (String, String) {
    match name.split_once(',') {
        Some((last, first)) => (first.trim().to_string(), last.trim().to_string()),
        None => (String::new(), name.trim().to_string()),
    }
}

pub struct StatusInfo {
    pub state: String,
    pub date: Option<NaiveDate>,
    pub due: Option<NaiveDate>,
    pub reminders: u8,
    pub thanked: bool,
}

/// Parses review status cells such as
/// "Confirmed on September 2, 2026**TH. Due on October 6, 2026".
pub fn parse_status(raw: &str) -> StatusInfo {
    let mut info = StatusInfo { state: raw.trim().to_string(), date: None, due: None, reminders: 0, thanked: false };
    if let Some(c) = STATE.captures(raw) {
        info.state = c[1].trim().to_string();
        info.date = parse_date(&c[2]);
        info.reminders = c.get(3).map_or(0, |m| m.as_str().len() as u8);
        info.thanked = c.get(4).is_some();
    }
    if let Some(c) = DUE.captures(raw) {
        info.due = parse_date(&c[1]);
    }
    info
}

/// False once a review is canceled, declined, received or submitted.
pub fn state_is_open(state: &str) -> bool {
    let s = state.to_lowercase();
    !["canceled", "cancelled", "received", "declined", "submitted"].iter().any(|p| s.starts_with(p))
}

impl Review {
    pub fn is_open(&self) -> bool {
        state_is_open(&self.state)
    }

    /// A review of `version` of the paper itself (not of a previous submission).
    pub fn of_version(&self, version: &str) -> bool {
        self.from_submission.is_empty() && self.version == version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        let s = parse_status("Confirmed on September 2, 2026**TH. Due on October 6, 2026");
        assert_eq!(s.state, "Confirmed");
        assert_eq!(s.date, NaiveDate::from_ymd_opt(2026, 9, 2));
        assert_eq!(s.due, NaiveDate::from_ymd_opt(2026, 10, 6));
        assert_eq!(s.reminders, 2);
        assert!(s.thanked);
        let s = parse_status("Received on October 19, 2026*TH");
        assert_eq!((s.state.as_str(), s.reminders, s.due), ("Received", 1, None));
        let s = parse_status("Canceled on September 1, 2026");
        assert_eq!(s.state, "Canceled");
    }

    #[test]
    fn names() {
        assert_eq!(split_name_pin("Doe, Jane (12345)"), ("Doe, Jane".into(), "12345".into()));
        assert_eq!(split_first_last("Smith, Ann B."), ("Ann B.".into(), "Smith".into()));
    }
}
