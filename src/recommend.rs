//! Reviewer recommendations for a paper, computed locally from what the
//! Request a Review page shows: the paper's title, keywords and authors, and
//! each reviewer's keywords and affiliation. Nothing is sent anywhere.
//!
//! Score (0–100):
//!   70% shared PaperPlaza keywords, each weighted by how rare it is in the
//!       list (sharing "LMIs" says more than sharing "Linear systems");
//!   30% word overlap (TF-IDF cosine) between the paper's title, keywords and
//!       abstract (when known) and the reviewer's free-text keywords.
//! Possible conflicts of interest are flagged: a paper author, or the same
//! affiliation as an author.

use crate::model::{Candidate, PaperInfo};
use std::collections::{HashMap, HashSet};

const STOP: &[&str] = &[
    "and", "the", "for", "with", "from", "into", "via", "using", "based", "under", "over", "class", "systems", "system", "control",
    "theory", "analysis", "design", "methods", "method", "applications", "application", "new", "its", "their", "are", "this", "that",
    "our", "paper", "approach", "problem", "problems", "results", "study", "general", "some", "such", "these", "those", "which",
];

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOP.contains(w) && !w.chars().all(|c| c.is_ascii_digit()))
        .map(stem)
        .collect()
}

/// Crude plural folding so "observers" meets "observer".
fn stem(w: &str) -> String {
    let w = w.to_string();
    if w.len() > 4 && w.ends_with("ies") {
        format!("{}y", &w[..w.len() - 3])
    } else if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("is") {
        w[..w.len() - 1].to_string()
    } else {
        w
    }
}

fn norm_aff(s: &str) -> HashSet<String> {
    const GENERIC: &[&str] = &["university", "universita", "università", "universite", "université", "universidad", "universitat", "degli", "studi", "institute", "of", "the", "de", "di", "der", "and", "technology", "dept", "department", "school", "college", "faculty", "cnrs"];
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !GENERIC.contains(w))
        .map(String::from)
        .collect()
}

/// The paper keywords found in a reviewer's keywords, as whole words
/// ("linear systems" does not match "Nonlinear systems").
fn keyword_matches(paper_kw: &[String], keywords: &str) -> Vec<String> {
    let text = keywords.to_lowercase();
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    paper_kw
        .iter()
        .filter(|k| {
            text.match_indices(k.as_str()).any(|(i, m)| !word(text[..i].chars().next_back()) && !word(text[i + m.len()..].chars().next()))
        })
        .cloned()
        .collect()
}

/// Score, explain and flag every candidate, in place, best first.
pub fn rank(info: &PaperInfo, cands: &mut [Candidate]) {
    let n = cands.len().max(1) as f64;
    let paper_kw: Vec<String> = info.keywords.iter().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect();
    for c in cands.iter_mut() {
        c.matches = keyword_matches(&paper_kw, &c.keywords);
    }

    // Keyword weights: idf over the list.
    let mut df: HashMap<String, usize> = HashMap::new();
    for c in cands.iter() {
        for m in c.matches.iter().cloned().collect::<HashSet<_>>() {
            *df.entry(m).or_default() += 1;
        }
    }
    let kw_idf = |k: &str| (n / (1.0 + *df.get(k).unwrap_or(&0) as f64)).ln().max(0.1) + 1.0;
    let kw_total: f64 = paper_kw.iter().map(|k| kw_idf(k)).sum::<f64>().max(1e-9);

    // Word overlap: tf-idf cosine.
    let docs: Vec<Vec<String>> = cands.iter().map(|c| words(&c.keywords)).collect();
    let mut wdf: HashMap<&str, usize> = HashMap::new();
    for d in &docs {
        for w in d.iter().map(String::as_str).collect::<HashSet<_>>() {
            *wdf.entry(w).or_default() += 1;
        }
    }
    let idf = |w: &str| (n / (1.0 + *wdf.get(w).unwrap_or(&0) as f64)).ln().max(0.0);
    let vec = |ws: &[String]| {
        let mut v: HashMap<String, f64> = HashMap::new();
        for w in ws {
            *v.entry(w.clone()).or_default() += 1.0;
        }
        for (w, x) in v.iter_mut() {
            *x *= idf(w);
        }
        v
    };
    let paper_text = format!("{} {} {}", info.title, info.keywords.join(" "), info.abstract_text);
    let pv = vec(&words(&paper_text));
    let pnorm = pv.values().map(|x| x * x).sum::<f64>().sqrt().max(1e-9);

    let author_pins: HashSet<&str> = info.authors.iter().map(|a| a.pin.as_str()).filter(|p| !p.is_empty()).collect();
    let author_affs: Vec<(String, HashSet<String>)> = info.authors.iter().map(|a| (a.name.clone(), norm_aff(&a.affiliation))).collect();

    for (c, d) in cands.iter_mut().zip(&docs) {
        let matched = c.matches.clone();
        let kw_score = matched.iter().map(|k| kw_idf(k)).sum::<f64>() / kw_total;
        let cv = vec(d);
        let cnorm = cv.values().map(|x| x * x).sum::<f64>().sqrt().max(1e-9);
        let mut shared: Vec<(&String, f64)> = cv.iter().filter_map(|(w, x)| pv.get(w).map(|y| (w, x * y))).collect();
        let cos = shared.iter().map(|(_, p)| p).sum::<f64>() / (pnorm * cnorm);
        shared.sort_by(|a, b| b.1.total_cmp(&a.1));
        c.score = ((0.7 * kw_score + 0.3 * cos.min(1.0)) * 100.0).round() as u32;
        c.reasons = Vec::new();
        if !matched.is_empty() {
            let shown: Vec<&str> = info.keywords.iter().map(|k| k.trim()).filter(|k| matched.contains(&k.to_lowercase())).collect();
            c.reasons.push(format!("keywords: {}", shown.join("; ")));
        }
        let terms: Vec<&str> = shared.iter().take(5).map(|(w, _)| w.as_str()).collect();
        if !terms.is_empty() {
            c.reasons.push(format!("shared terms: {}", terms.join(", ")));
        }
        c.conflict = if author_pins.contains(c.pin.as_str()) {
            Some("author of the paper".into())
        } else {
            let mine = norm_aff(&c.affiliation);
            author_affs
                .iter()
                .find(|(_, a)| !a.is_empty() && !mine.is_empty() && {
                    let common = a.intersection(&mine).count();
                    common * 2 >= a.len().min(mine.len()).max(1) && common >= 1
                })
                .map(|(name, _)| format!("same affiliation as {name}"))
        };
    }
    cands.sort_by(|a, b| b.available.cmp(&a.available).then(a.conflict.is_some().cmp(&b.conflict.is_some())).then(b.score.cmp(&a.score)).then(a.name.cmp(&b.name)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Author;

    fn cand(pin: &str, aff: &str, kw: &str, matches: &[&str]) -> Candidate {
        Candidate {
            pin: pin.into(),
            name: format!("N{pin}"),
            affiliation: aff.into(),
            email: String::new(),
            keywords: kw.into(),
            matches: matches.iter().map(|s| s.to_string()).collect(),
            available: true,
            in_list: true,
            score: 0,
            reasons: Vec::new(),
            conflict: None,
        }
    }

    #[test]
    fn rare_keywords_and_conflicts() {
        let info = PaperInfo {
            title: "Robust control of Lur'e systems".into(),
            keywords: vec!["Sampled-data control".into(), "LMIs".into(), "Linear systems".into()],
            authors: vec![Author { name: "Rossi, Mario".into(), affiliation: "Università di Esempio".into(), pin: "12345".into() }],
            abstract_text: String::new(),
        };
        let mut cs = vec![
            cand("1", "ETH Zurich", "Linear systems", &["Linear systems"]),
            cand("2", "KTH", "LMIs; Sampled-data control; Lur'e systems, absolute stability", &["LMIs", "Sampled-data control"]),
            cand("3", "Universita di Esempio", "LMIs; Sampled-data control", &["LMIs", "Sampled-data control"]),
            cand("4", "MIT", "Robotics", &[]),
            cand("5", "Oxford", "Linear systems", &["Linear systems"]),
        ];
        rank(&info, &mut cs);
        assert_eq!(cs[0].pin, "2");
        assert!(cs[0].reasons.iter().any(|r| r.contains("lur")), "{:?}", cs[0].reasons);
        let c3 = cs.iter().find(|c| c.pin == "3").unwrap();
        assert_eq!(c3.conflict.as_deref(), Some("same affiliation as Rossi, Mario"));
        assert_eq!(cs.last().unwrap().pin, "3", "conflicts go last");
        assert!(cs.iter().find(|c| c.pin == "1").unwrap().score > cs.iter().find(|c| c.pin == "4").unwrap().score);
    }

    #[test]
    fn whole_keyword_matches() {
        let kw = vec!["linear systems".to_string(), "lmis".to_string(), "delay systems".to_string()];
        assert_eq!(keyword_matches(&kw, "Nonlinear systems; LMIs"), ["lmis"]);
        assert_eq!(keyword_matches(&kw, "Linear systems; Time-delay systems"), ["linear systems", "delay systems"]);
    }
}
