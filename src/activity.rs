//! A log of what Docket sent or changed by itself (automatic invitations and
//! cancellations, site notes), for the Activity tab of the settings.

use crate::creds;
use chrono::Local;
use serde::{Deserialize, Serialize};

const MAX: usize = 5000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub at: String,
    pub site: String,
    pub journal: String,
    pub paper: String,
    /// "invitation", "cancellation", "thanks", "confirmation", "reminder", "problem".
    pub kind: String,
    /// The reviewer concerned (may be empty).
    pub who: String,
    pub text: String,
    pub ok: bool,
}

fn path() -> std::path::PathBuf {
    creds::config_dir().join("activity.json")
}

pub fn load() -> Vec<Entry> {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Add an entry (the oldest ones go once there are MAX).
pub fn add(site: &str, journal: &str, paper: &str, kind: &str, who: &str, text: &str, ok: bool) {
    let mut all = load();
    all.push(Entry {
        at: Local::now().format("%Y-%m-%d %H:%M").to_string(),
        site: site.into(),
        journal: journal.into(),
        paper: paper.into(),
        kind: kind.into(),
        who: who.into(),
        text: text.into(),
        ok,
    });
    if all.len() > MAX {
        all.drain(..all.len() - MAX);
    }
    let _ = std::fs::create_dir_all(creds::config_dir());
    let tmp = path().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec(&all).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, path());
    }
}
