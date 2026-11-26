//! Letters of PaperPlaza's own notes (thank-you notes, confirmation requests,
//! review reminders): the site's letter, or one of your templates put in its
//! place, and the site's letters offered as templates for messages.
//!
//! The site writes the reviewer's title and name itself (%YOUR_TITLE%
//! %YOUR_GIVEN_NAME% %YOUR_SURNAME%) and adds the paper's details below the
//! letter. A template's reviewer-name placeholders become the site's; a
//! letter with values of the paper or the review is filled in for each review
//! and sent on its own.

use crate::templates::{self, WORD};
use anyhow::Result;
use chrono::NaiveDate;
use std::collections::HashMap;

/// The reviewer's name: written by the site for each reviewer.
const NAMES: &[(&str, &str)] = &[
    ("%reviewername", "%YOUR_GIVEN_NAME% %YOUR_SURNAME%"),
    ("%reviewerfirstname", "%YOUR_GIVEN_NAME%"),
    ("%reviewerlastname", "%YOUR_SURNAME%"),
];
/// The same for every reviewer.
const SHARED: &[&str] = &["%journal", "%signature", "%today"];

/// Refuse placeholders that do not exist.
pub fn check(text: &str) -> Result<()> {
    templates::check(text)
}

/// The letter differs from one review to the other (so each is sent alone,
/// filled with that review's values).
pub fn per_review(text: &str) -> bool {
    WORD.find_iter(text).map(|m| m.as_str()).any(|w| !NAMES.iter().any(|(n, _)| *n == w) && !SHARED.contains(&w))
}

/// The letter to put in the site's form: the reviewer's name as the site's
/// placeholders, the rest filled in (`review`: the review's values, for a
/// letter that is per review).
pub fn to_site(text: &str, journal: &str, signature: &str, today: NaiveDate, review: Option<HashMap<&'static str, String>>) -> String {
    let mut v = review.unwrap_or_default();
    for (n, site) in NAMES {
        v.insert(n, site.to_string());
    }
    v.insert("%journal", journal.into());
    v.insert("%signature", signature.into());
    v.insert("%today", today.format("%B %-d, %Y").to_string());
    templates::fill(text, &v)
}

/// The kinds of the site's letters.
pub const KINDS: &[(&str, &str, &str)] = &[
    ("accept", "PaperPlaza: thanks for agreeing to review", "%journal %paper: thank you for agreeing to review"),
    ("review", "PaperPlaza: thanks for the review", "%journal %paper: thank you for your review"),
    ("confirm", "PaperPlaza: confirmation request", "%journal %paper: please accept or decline the review request"),
    ("remind", "PaperPlaza: review reminder", "%journal %paper: review reminder"),
];

/// One of the site's letters as a message template: its name placeholders
/// become Docket's, and the paper's details (which the site adds below its
/// letter) are added at the end.
pub fn from_site(kind: &str, letter: &str) -> templates::Template {
    let (_, name, subject) = KINDS.iter().find(|(k, ..)| *k == kind).copied().unwrap_or(KINDS[0]);
    let mut body = letter.replace('\r', "");
    for (site, ours) in [
        ("%YOUR_TITLE% %YOUR_GIVEN_NAME% %YOUR_SURNAME%", "%reviewername"),
        ("%YOUR_GIVEN_NAME% %YOUR_SURNAME%", "%reviewername"),
        ("%YOUR_GIVEN_NAME%", "%reviewerfirstname"),
        ("%YOUR_SURNAME%", "%reviewerlastname"),
        ("%YOUR_TITLE% ", ""),
        ("%YOUR_TITLE%", ""),
    ] {
        body = body.replace(site, ours);
    }
    let details = if kind == "review" { "Paper: %paper, “%title”" } else { "Paper: %paper, “%title”\nAuthors: %authors\nReview due: %deadline" };
    templates::Template { name: name.into(), subject: subject.into(), body: format!("{}\n\n{details}", body.trim_end()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_through_the_site() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 26).unwrap();
        let t = "Dear %reviewerfirstname,\n\nthank you. %journal\n%signature";
        check(t).unwrap();
        assert!(!per_review(t));
        assert_eq!(to_site(t, "IEEE-TAC", "J. Smith", today, None), "Dear %YOUR_GIVEN_NAME%,\n\nthank you. IEEE-TAC\nJ. Smith");
        let r = "Dear %reviewername, your review of %paper (“%title”) is %delay late.";
        assert!(per_review(r));
        let v: HashMap<&'static str, String> = [("%paper", "26-1234.1"), ("%title", "A Study"), ("%delay", "6 days"), ("%reviewername", "Jane Doe")].into_iter().map(|(k, v)| (k, v.to_string())).collect();
        assert_eq!(to_site(r, "", "", today, Some(v)), "Dear %YOUR_GIVEN_NAME% %YOUR_SURNAME%, your review of 26-1234.1 (“A Study”) is 6 days late.");
        assert!(check("Dear %reviwername").is_err(), "typos");
    }

    #[test]
    fn site_letters_as_templates() {
        let t = from_site("remind", "Dear %YOUR_TITLE% %YOUR_GIVEN_NAME% %YOUR_SURNAME%,\r\n\r\nThe deadline is shown below.\r\n\r\nSincerely,\r\nJ. Smith\r\n");
        assert_eq!(t.name, "PaperPlaza: review reminder");
        assert!(t.body.starts_with("Dear %reviewername,\n\nThe deadline is shown below."), "{}", t.body);
        assert!(t.body.ends_with("J. Smith\n\nPaper: %paper, “%title”\nAuthors: %authors\nReview due: %deadline"));
        templates::check(&t.body).unwrap();
        templates::check(&t.subject).unwrap();
    }
}
