//! Drives `Session` against a local server that replays pages saved from
//! PaperPlaza, and checks what the client posts. The saved pages contain
//! confidential review data, so they are not part of the repository, and
//! neither is what the tests expect to read from them (names, numbers, texts):
//! that is in `expected.json` beside the pages, in the format of
//! `tests/replay-expected.example.json`.
//!
//!   DOCKET_CAPTURES=/path/to/saved/pages cargo test -- --include-ignored --test-threads=1

use crate::model::RoleKind;
use crate::papercept::{ContactOptions, Session};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// `$DOCKET_CAPTURES/expected.json`.
fn expected() -> &'static Value {
    static EXPECTED: OnceLock<Value> = OnceLock::new();
    EXPECTED.get_or_init(|| {
        let dir = std::env::var("DOCKET_CAPTURES").expect("DOCKET_CAPTURES is not set");
        let path = format!("{dir}/expected.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e} (see tests/replay-expected.example.json)"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
    })
}

/// The expected value at `path` ("paper.id").
pub(crate) fn xv(path: &str) -> &'static Value {
    let v = path.split('.').fold(expected(), |v, k| &v[k]);
    assert!(!v.is_null(), "expected.json has no '{path}'");
    v
}

pub(crate) fn x(path: &str) -> String {
    match xv(path) {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

pub(crate) fn xn(path: &str) -> usize {
    xv(path).as_u64().unwrap_or_else(|| panic!("'{path}' is not a number")) as usize
}

pub(crate) fn xs(path: &str) -> Vec<String> {
    xv(path).as_array().unwrap_or_else(|| panic!("'{path}' is not a list")).iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect()
}

#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub method: String,
    pub path: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub(crate) fn serve(dir: String) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (seen2, base2) = (seen.clone(), base.clone());
    // Commands naming one paper or review ("Reviews<paper>", …) and the saved page each opens.
    let named: HashMap<String, String> = xv("replay").as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    std::thread::spawn(move || {
        for mut rq in server.incoming_requests() {
            let mut body = Vec::new();
            rq.as_reader().read_to_end(&mut body).unwrap();
            let ct = rq
                .headers()
                .iter()
                .find(|h| h.field.equiv("Content-Type"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let path = rq.url().to_string();
            let s = Seen { method: rq.method().to_string(), path: path.clone(), content_type: ct, body: body.clone() };
            seen2.lock().unwrap().push(s);
            let text = String::from_utf8_lossy(&body).to_string();
            let cmd = text
                .split('&')
                .find_map(|kv| kv.strip_prefix("Command="))
                .map(str::to_string)
                .or_else(|| multipart_value(&text, "Command"))
                .unwrap_or_default();
            let file = match (path.as_str(), cmd.as_str()) {
                (_, c) if named.contains_key(c) => named[c].as_str(),
                (p, _) if p.ends_with("/conferences/scripts/start.pl") && rq.method().as_str() == "GET" => "START",
                (p, "Login.x") if p.ends_with("/conferences/scripts/start.pl") => "Log in.html",
                (p, "Login.x") if p.ends_with("/conferences/scripts/login.pl") => "Access page.html",
                (p, _) if p.contains("/journals/tac/scripts/login.pl?PIN=") => "IEEE-TAC Access Page.html",
                (_, "AccessReviewerspace") => "Reviewer's Workspace.html",
                (p, "" | "Next") if p.ends_with("/reviewerspace.pl") => {
                    if text.contains("ShowAll=ON") { "Reviewer's Workspace 2.html" } else { "Reviewer's Workspace.html" }
                }
                (_, "Access_597_4" | "Workspace") => "Workspace.html",
                // The workspace form sent back (its settings changed).
                (p, "") if p.ends_with("/workspace.pl") => "Workspace.html",
                (_, "Reviews") => "Manage Reviews.html",
                (_, "ContactReviewer") => "Contact the Reviewer.html",
                (_, "Contact") => "Contact One or Several Editorial Staff Members.html",
                (_, "Reminders") => "Manage Reminders and Thanks.html",
                (_, "SendThankYouNotesAll") => "Send Thank You Notes.html",
                // The page after sending was not saved; the list is read again from the workspace.
                (_, "SendThankYouNotes" | "SendReminders") => "Manage Reminders and Thanks.html",
                (_, "SendRemindersAll") => "Send Review Reminders.html",
                (_, "RequestConfirmationsAll") => "Request Review Confirmations.html",
                (_, "RequestReview") => "Request a Review.html",
                (_, "Details") => "Database Entry of IEEE-TAC Submission 26-2586. - Details.html",
                (_, "Reviewers") => "Reviewers List - 2.html",
                (p, _) if p.contains("/pins.pl?") && p.contains("AddReviewers=ON") => "Add PINs to Your Reviewers List.html",
                (p, _) if p.ends_with("/pins.pl") && text.contains("SubmitAddToReviewersList=") => "Add PINs to Your Reviewers List - after adding.html",
                (p, "Search") if p.ends_with("/pins.pl") && text.contains("SearchIn=PIN") => "Add PINs to Your Reviewers List-results.html",
                (p, "Search" | "Next") if p.ends_with("/pins.pl") => "Add PINs to Your Reviewers List - results .html",
                (_, "SendMessage") => "Manage Reviews.html",
                (p, _) if p.ends_with("reviews_aja.pl") => "AJAX",
                _ => "NOTFOUND",
            };
            if file == "PDF" {
                let resp = tiny_http::Response::from_data(b"%PDF-1.4 test".to_vec())
                    .with_header("Content-Type: application/pdf".parse::<tiny_http::Header>().unwrap());
                rq.respond(resp).unwrap();
                continue;
            }
            let html = match file {
                "START" => START.to_string(),
                "AJAX" => "2026-11-03 10:00:00&October 13, 2026".to_string(),
                "NOTFOUND" => "<html><title>Not found</title></html>".to_string(),
                f => {
                    let bytes = std::fs::read(format!("{dir}/{f}")).unwrap();
                    encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned()
                }
            };
            let html = html.replace("https://css.paperplaza.net", &base2).replace("http://css.paperplaza.net", &base2);
            let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(&html);
            let resp = tiny_http::Response::from_data(bytes.into_owned())
                .with_header("Content-Type: text/html; charset=windows-1252".parse::<tiny_http::Header>().unwrap());
            rq.respond(resp).unwrap();
        }
    });
    (base, seen)
}

fn multipart_value(body: &str, name: &str) -> Option<String> {
    let key = format!("name=\"{name}\"\r\n\r\n");
    let i = body.find(&key)? + key.len();
    Some(body[i..].split("\r\n--").next()?.to_string())
}

const START: &str = r#"<html><head><title>Start</title></head><body>
<form method="POST" name="start" action="https://css.paperplaza.net/conferences/scripts/start.pl">
<input type="checkbox" name="Command" value="ON" style="display:none">
<script>function command(arg){ document.forms[0].Command.checked = true; document.forms[0].Command.value = arg + '.x'; document.forms[0].submit(); }</script>
<a href="javascript:command('Login')">Log in</a>
<input type="hidden" name="Fields" value="Command,SPW,Home,Login,Refresh">
<input type="hidden" name="Hash" value="h0"><input type="hidden" name="Hidden" value="SPW"><input type="hidden" name="SPW" value="">
</form></body></html>"#;


/// Logs in and opens the Associate Editor workspace.
fn editor(base: &str, viewer: bool) -> Session {
    let mut s = Session::new(None, viewer).unwrap().with_portal(&format!("{base}/conferences/scripts/"));
    s.login("someone@example.org", "x").unwrap();
    s.enter_journal("tac").unwrap();
    s.enter_role("Access_597_4").unwrap();
    s
}

fn the_paper(s: &mut Session) -> crate::model::Paper {
    let id = x("paper.id");
    s.load_papers(false).unwrap().into_iter().find(|p| p.id == id).unwrap()
}

#[test]
#[ignore]
fn replay() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else {
        eprintln!("DOCKET_CAPTURES not set; skipping");
        return;
    };
    let (base, seen) = serve(dir);
    let mut s = Session::new(None, false).unwrap().with_portal(&format!("{base}/conferences/scripts/"));

    s.login("someone@example.org", "pa&ss word").unwrap();
    {
        let log = seen.lock().unwrap();
        let login = &log[2];
        let body = String::from_utf8_lossy(&login.body);
        assert!(login.path.ends_with("/conferences/scripts/login.pl"), "{}", login.path);
        assert!(body.contains("LoginId=someone%40example.org"), "{body}");
        assert!(body.contains("Password=pa%26ss+word"), "{body}");
        assert!(body.contains("Command=Login.x"), "{body}");
        assert!(body.contains("Hash=0645e3c622c8b73943684df133306f69"), "{body}");
    }
    assert!(s.journals.iter().any(|j| j.code == "tac"));

    s.enter_journal("tac").unwrap();
    let roles: Vec<_> = s.roles.iter().map(|r| (r.label.as_str(), r.command.as_str(), r.kind)).collect();
    assert_eq!(roles, [("Associate Editor", "Access_597_4", RoleKind::Editor), ("Reviewer", "AccessReviewerspace", RoleKind::Reviewer)]);

    // Reviewer workspace: no current items; "Show all items" lists them all.
    s.enter_role("AccessReviewerspace").unwrap();
    assert!(s.load_assignments(false).unwrap().is_empty());
    let mine = s.load_assignments(true).unwrap();
    assert_eq!(mine.len(), xn("reviewer.items"));
    {
        let log = seen.lock().unwrap();
        let show_all = log.iter().find(|x| {
            let b = String::from_utf8_lossy(&x.body);
            x.path.ends_with("/reviewerspace.pl") && b.contains("ShowAll=ON") && !b.contains("Command=")
        });
        let body = String::from_utf8_lossy(&show_all.expect("ShowAll posted like the checkbox's form.submit()").body).to_string();
        assert!(body.contains("Hash=5fd5e154244b5d56d3aafb213e850b98"), "{body}");
    }
    let a = mine.iter().find(|a| a.review_id == x("reviewer.canceled.id")).unwrap();
    assert_eq!(
        (a.requested_by.clone(), a.requester_role.clone(), a.requester_email.clone()),
        (x("reviewer.canceled.requested_by"), x("reviewer.canceled.requester_role"), x("reviewer.canceled.requester_email"))
    );
    assert_eq!((a.number.clone(), a.version.as_str(), a.state.as_str()), (x("reviewer.canceled.number"), "1", "Canceled"));
    assert!(a.title.starts_with(&x("reviewer.canceled.title_start")));
    let sub = mine.iter().find(|a| a.review_id == x("reviewer.submitted.id")).unwrap();
    assert_eq!((sub.state.as_str(), sub.recommendation.clone(), sub.overdue), ("Submitted", x("reviewer.submitted.recommendation"), false));
    assert_eq!(mine.iter().filter(|a| a.state == "Submitted").count(), xn("reviewer.submitted_count"));

    // Back to the editor role: its command is only on the journal Access page.
    s.enter_role("Access_597_4").unwrap();
    {
        let log = seen.lock().unwrap();
        let body = String::from_utf8_lossy(&log.last().unwrap().body).to_string();
        assert!(body.contains("Command=Access_597_4&") || body.ends_with("Command=Access_597_4"), "{body}");
    }

    let papers = s.load_papers(false).unwrap();
    assert_eq!(papers.len(), xn("papers"));
    let paper = papers.iter().find(|p| p.id == x("paper.id")).unwrap();
    assert_eq!(paper.status, "Under review");
    let reviews = s.load_reviews(paper).unwrap();
    assert_eq!(reviews.len(), xn("paper.reviews"));
    {
        let log = seen.lock().unwrap();
        let last = log.last().unwrap();
        assert!(last.path.ends_with("/journals/tac/scripts/workspace.pl"), "{}", last.path);
        assert!(String::from_utf8_lossy(&last.body).contains(&format!("Command={}", x("paper.reviews_cmd"))));
    }

    // Contact: the saved details and contact pages are those of one (canceled) review.
    let canceled = reviews.iter().find(|r| r.review_id == x("paper.canceled_review.id")).unwrap().clone();
    let subject = format!("IEEE-TAC {}: hello", paper.id);
    let out = s.contact_reviewer(&canceled, &subject, "", "Dear Università,\nsecond line", &ContactOptions::default(), None).unwrap();
    assert!(out.starts_with("Sent"), "{out}");
    {
        let log = seen.lock().unwrap();
        let send = log.iter().rev().find(|x| x.path.ends_with("contactreviewer.pl")).unwrap();
        assert!(send.content_type.starts_with("multipart/form-data"), "{}", send.content_type);
        let body = &send.body;
        let contains = |needle: &[u8]| body.windows(needle.len()).any(|w| w == needle);
        assert!(contains(b"name=\"Command\"\r\n\r\nSendMessage\r\n"));
        assert!(contains(format!("name=\"Review_ID\"\r\n\r\n{}\r\n", canceled.review_id).as_bytes()));
        assert!(contains(format!("name=\"Subject\"\r\n\r\n{subject}\r\n").as_bytes()));
        assert!(contains(b"Dear Universit\xe0,\r\nsecond line"), "message must be windows-1252 with CRLF");
        assert!(contains(b"name=\"Attachment\"; filename=\"\""));
        assert!(contains(b"name=\"Hash\"\r\n\r\n4934737cdcf2c388ec002e471c5cd71f\r\n"));
        assert!(!contains(b"name=\"DoNotArchive\""));
    }

    // Cancel an open review: the replay serves the details page of the
    // canceled one, so the safety check must refuse and CancelReview must
    // never be posted.
    let open = reviews.iter().find(|r| r.review_id == x("paper.open_review")).unwrap().clone();
    let err = s.cancel_review(&open).unwrap_err().to_string();
    assert!(err.contains("wrong review"), "{err}");
    assert!(!seen.lock().unwrap().iter().any(|x| String::from_utf8_lossy(&x.body).contains("Command=CancelReview")));

    // A canceled review is not cancelled again.
    let err = s.cancel_review(&canceled).unwrap_err().to_string();
    assert!(err.contains("Canceled on"), "{err}");

    // Deadline extension uses the site's AJAX call (the replay answers with a fixed date).
    let out = s.extend_deadline(&open, 7).unwrap();
    assert_eq!(out, "New due date: October 13, 2026");
    {
        let log = seen.lock().unwrap();
        let x = log.last().unwrap();
        let body = String::from_utf8_lossy(&x.body);
        assert!(x.path.ends_with("/journals/tac/scripts/reviews_aja.pl"));
        assert!(body.starts_with("Token=") && body.ends_with(&format!("&ExtendDeadline=ON&ReviewID={}&Extension=7", open.review_id)), "{body}");
    }
    assert!(s.extend_deadline(&canceled, 7).is_err());
    assert!(s.extend_deadline(&open, 5).is_err());
}

/// Viewer mode: pages can be read, but no change ever leaves the client.
#[test]
#[ignore]
fn viewer_mode() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let (base, seen) = serve(dir);
    let mut s = editor(&base, true);
    let paper = the_paper(&mut s);
    let reviews = s.load_reviews(&paper).unwrap();
    let open = reviews.iter().find(|r| r.review_id == x("paper.open_review")).unwrap().clone();
    let canceled = reviews.iter().find(|r| r.review_id == x("paper.canceled_review.id")).unwrap().clone();
    let before = seen.lock().unwrap().len();

    let blocked = |e: anyhow::Error| e.downcast_ref::<crate::http::ViewerMode>().is_some() || format!("{e:#}").contains("viewer mode");
    assert!(blocked(s.extend_deadline(&open, 7).unwrap_err()));
    assert!(blocked(s.contact_reviewer(&canceled, "s", "", "m", &ContactOptions::default(), None).unwrap_err()));
    assert!(blocked(s.request_reviews(&paper, &["1".into()], &Default::default()).unwrap_err()));
    // cancel_review reads first (allowed), then the CancelReview click is refused.
    let err = s.cancel_review(&open).unwrap_err();
    assert!(format!("{err:#}").contains("wrong review") || blocked(err), "the replay serves the wrong details page, or viewer mode stops it");
    let sent = seen.lock().unwrap()[before..].to_vec();
    for x in &sent {
        let b = String::from_utf8_lossy(&x.body);
        let cmd = b.split('&').find_map(|kv| kv.strip_prefix("Command=")).unwrap_or("");
        let changes = ["CancelReview", "UnCancelReview", "SendMessage", "ContactReviewer", "SendReviewReminder", "RegisterAsConfirmed", "SendReviewCancellationNotice", "SendThankYouNotes", "SendReminders"];
        assert!(!changes.contains(&cmd), "viewer mode sent {cmd}: {}", x.path);
        assert!(!x.path.ends_with("_aja.pl"), "viewer mode sent an AJAX change");
        assert!(!x.content_type.starts_with("multipart"), "viewer mode sent a multipart form");
    }

    // The Reviewers list on Request a Review is readable in viewer mode.
    let (info, cands) = s.load_candidates(&paper).unwrap();
    assert_eq!(info.authors.len(), xn("paper.authors.count"));
    assert_eq!((info.authors[0].name.clone(), info.authors[0].pin.clone()), (x("paper.authors.first_name"), x("paper.authors.first_pin")));
    assert!(info.authors.iter().any(|a| a.affiliation == x("paper.authors.affiliation")));
    assert_eq!(info.keywords, xs("paper.keywords"));
    assert!(cands[0].score >= cands[10].score && cands[0].available && cands[0].conflict.is_none());
    // People sharing an author's affiliation are flagged.
    let word = x("paper.authors.affiliation_word");
    let same: Vec<_> = cands.iter().filter(|c| c.affiliation.to_lowercase().contains(&word)).collect();
    assert!(!same.is_empty() && same.iter().all(|c| c.conflict.is_some()), "{} people at {word}, not all flagged", same.len());
    assert_eq!(cands.len(), xn("candidates.count"));
    // Titles as given ("Prof.", "Dr.", …); some people give none.
    let group = |g: &str| cands.iter().filter(|c| crate::model::title_group(&c.title) == g).count();
    assert!(["prof", "dr", "mr", "none"].iter().all(|g| group(g) > 0), "titles were not read");
    let c = cands.iter().find(|c| c.pin == x("candidates.keyword_match.pin")).unwrap();
    assert!(c.available && c.matches == [x("candidates.keyword_match.keyword")], "{:?}", c.matches);
    assert!(!cands.iter().find(|c| c.pin == x("candidates.unavailable_pin")).unwrap().available);

    // A schedule checked in viewer mode records what it would do.
    let _guard = crate::creds::folder_lock();
    let cfg = std::env::temp_dir().join(format!("docket-viewer-{}", std::process::id()));
    unsafe { std::env::set_var("DOCKET_CONFIG_DIR", &cfg) };
    let mut store = crate::scheduler::Store::default();
    let q: Vec<(String, String)> = xv("candidates.queue").as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into())).collect();
    let names = q.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ");
    let id = store.upsert(None, &paper, "tac", 4, 5, Some(7), q.clone()).unwrap();
    // The preview of "Run automatic invitations now" says what would be sent, and sends nothing.
    let before = seen.lock().unwrap().len();
    let (lines, any) = crate::scheduler::preview(&mut s, &store);
    let tally = format!("{} accepted, 0 awaiting an answer", xn("paper.accepted"));
    assert!(any && lines == [format!("{}: invite {names} ({tally})", paper.number)], "{lines:?}");
    for x in &seen.lock().unwrap()[before..] {
        let b = String::from_utf8_lossy(&x.body);
        assert!(!b.contains("Command=Send") && !b.contains("Command=CancelReview") && !b.contains("Command=Next"), "preview sent {b}");
    }
    let report = crate::scheduler::run(&mut s, &mut store, Some(id));
    let sch = &store.schedules[0];
    assert!(report[0].contains(&format!("{tally}, {} declined", xn("paper.lost"))), "{report:?}");
    assert!(sch.log.iter().any(|l| l.text == format!("Viewer mode — would invite {names}")));
    assert!(sch.queue.iter().all(|b| b.invited_on.is_none()));
    let _ = std::fs::remove_dir_all(cfg);
}

/// PIN database search, adding to the Reviewers list, and the abstract.
#[test]
#[ignore]
fn pins_and_details() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let (base, seen) = serve(dir);
    let mut s = editor(&base, false);
    let paper = the_paper(&mut s);

    let (abs, kws) = s.load_abstract(&paper).unwrap();
    assert!(abs.starts_with(&x("paper.abstract_start")));
    assert_eq!(kws, xs("paper.keywords"));

    let query = x("pin_search.query");
    let (found, more) = s.search_pins("Keywords", &query, 5).unwrap();
    assert_eq!((found.len(), more), (xn("pin_search.found"), Some(xn("pin_search.total") as u32)));
    assert_eq!(found.iter().filter(|c| c.available && !c.in_list).count(), xn("pin_search.addable"), "addable");
    assert!(found.iter().any(|c| crate::model::title_group(&c.title) == "prof"), "the Title column was not read");
    let c = found.iter().find(|c| c.pin == x("pin_search.person.pin")).unwrap();
    assert_eq!((c.name.clone(), c.affiliation.clone(), c.in_list), (x("pin_search.person.name"), x("pin_search.person.affiliation"), false));
    {
        let log = seen.lock().unwrap();
        let search = log.iter().find(|x| x.path.ends_with("/pins.pl") && String::from_utf8_lossy(&x.body).contains("Command=Search")).unwrap();
        let b = String::from_utf8_lossy(&search.body);
        for part in ["SearchIn=Keywords", &format!("SearchFor={query}"), "IncludeKeywords=ON", "PageviewRows=200", "AddReviewers=ON", "Hash=6f41087d0afca1353a9d00cea227535b"] {
            assert!(b.contains(part), "{part} missing: {b}");
        }
    }

    // One person can be added on the saved results page; another is already on the list.
    let add = x("pin_search.add.pin");
    assert_eq!(s.add_to_list(&add).unwrap(), format!("{} was added to your Reviewers list", x("pin_search.add.name")));
    {
        let log = seen.lock().unwrap();
        let sent = log.iter().rev().find(|x| String::from_utf8_lossy(&x.body).contains("SubmitAddToReviewersList")).unwrap();
        let b = String::from_utf8_lossy(&sent.body);
        assert!(b.contains(&format!("AddToReviewers{add}=HIT")) && b.contains("SubmitAddToReviewersList=Add+to+list"), "{b}");
        assert!(!b.contains("Command="), "the Add to list button is a real submit button: {b}");
        assert_eq!(b.matches("=HIT").count(), 1, "only the chosen person is ticked");
    }
    assert!(s.add_to_list(&x("pin_search.already_listed")).unwrap().contains("already on your Reviewers list"));

    // Viewer mode: searching works, adding is refused before anything is sent.
    let mut v = editor(&base, true);
    assert_eq!(v.search_pins("Keywords", &query, 1).unwrap().0.len(), xn("pin_search.found"));
    let n = seen.lock().unwrap().len();
    assert!(format!("{:#}", v.add_to_list(&add).unwrap_err()).contains("viewer mode"));
    assert_eq!(seen.lock().unwrap().len(), n);
}

/// The request built from the complete Request a Review page, as next() submits it.
#[test]
#[ignore]
fn request_form() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let bytes = std::fs::read(format!("{dir}/Request a Review - List complete.html")).unwrap();
    let html = encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned();
    let page = crate::page::Page::parse("https://css.paperplaza.net/journals/tac/scripts/reviews.pl", html);
    // Two people, as the page names them (one with letters outside windows-1252).
    let people: Vec<(String, String)> = xv("request.people").as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into())).collect();
    let pins: Vec<String> = people.iter().map(|(p, _)| p.clone()).collect();
    let form = crate::papercept::prepare_request_form(&page, &pins).unwrap();
    let entries = form.entries(Some("Next"));
    let text = |n: &str| entries.iter().filter(|e| e.name == n).filter_map(|e| match &e.value { crate::page::EntryValue::Text(t) => Some(t.clone()), _ => None }).collect::<Vec<_>>();
    for (pin, label) in &people {
        assert_eq!(text(&format!("Include{pin}")), [label.clone()]);
    }
    assert_eq!(text("RequestList"), [people.iter().map(|(_, l)| format!("{l}\r\n")).collect::<String>()]);
    assert_eq!(text("Command"), ["Next"]);
    assert_eq!(text("Submission_ID"), [x("request.submission")]);
    assert_eq!(text("Hash"), ["c7512e457e75f88ec5a9c1a1530b7239"]);
    assert_eq!(entries.iter().filter(|e| e.name.starts_with("Include")).count(), 2);
    assert!(crate::papercept::prepare_request_form(&page, &["999999999".into()]).is_err());
    assert!(!crate::http::viewer_allows(&form, Some("Next")), "viewer mode never moves on to sending");
}

/// The page after Next, filled in and sent as send() does.
#[test]
#[ignore]
fn send_form() {
    use crate::model::InviteOptions;
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let bytes = std::fs::read(format!("{dir}/Request a Review - Reviewer selected and soon to be invited.html")).unwrap();
    let html = encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned();
    let page = crate::page::Page::parse("https://css.paperplaza.net/journals/tac/scripts/requestreview.pl", html);
    let (submission, pin) = (x("request.submission"), x("request.selected_pin"));
    let pins = [pin.clone()];
    let text = |entries: &[crate::page::Entry], n: &str| {
        entries.iter().filter(|e| e.name == n).filter_map(|e| match &e.value { crate::page::EntryValue::Text(t) => Some(t.clone()), _ => None }).collect::<Vec<_>>()
    };

    // Site defaults: deadline December 8, 2026, no attachment, no copy.
    let form = crate::papercept::prepare_send_form(&page, &submission, &pins, &InviteOptions::default()).unwrap();
    let e = form.entries(Some("Send"));
    assert_eq!(text(&e, "Command"), ["Send"]);
    assert_eq!(text(&e, &format!("Include{pin}")), ["ON"]);
    assert_eq!(text(&e, "ReviewerPINs"), [pin.clone()]);
    assert_eq!((text(&e, "MonthDue"), text(&e, "DayDue"), text(&e, "YearDue")), (vec!["12".to_string()], vec!["8".to_string()], vec!["2026".to_string()]));
    assert_eq!(text(&e, "Form_letter"), ["Request a review"]);
    assert!(text(&e, "Cover_message")[0].starts_with("Dear %YOUR_TITLE% %YOUR_GIVEN_NAME% %YOUR_SURNAME%,"));
    assert_eq!(text(&e, "Hash"), ["b16c2db1b6243f45c23a28885e560965"]);
    assert!(text(&e, "AttachSubmission").is_empty() && text(&e, "SendCopy").is_empty());
    for button in ["Back", "Reviews", "Send"] {
        assert!(text(&e, button).is_empty(), "{button} is not clicked");
    }

    // Chosen deadline and attachment.
    let opts = InviteOptions { deadline: chrono::NaiveDate::from_ymd_opt(2027, 1, 15), attach: true, copy: false };
    let e = crate::papercept::prepare_send_form(&page, &submission, &pins, &opts).unwrap().entries(Some("Send"));
    assert_eq!((text(&e, "MonthDue"), text(&e, "DayDue"), text(&e, "YearDue")), (vec!["1".to_string()], vec!["15".to_string()], vec!["2027".to_string()]));
    assert_eq!(text(&e, "AttachSubmission"), ["ON"]);

    // Anything unexpected stops before sending: someone else, another paper, a deadline too far.
    assert!(crate::papercept::prepare_send_form(&page, &submission, &[pin.clone(), "1".into()], &InviteOptions::default()).is_err());
    assert!(crate::papercept::prepare_send_form(&page, &x("paper.id"), &pins, &InviteOptions::default()).is_err());
    let bad = InviteOptions { deadline: chrono::NaiveDate::from_ymd_opt(2030, 1, 1), ..Default::default() };
    assert!(crate::papercept::prepare_send_form(&page, &submission, &pins, &bad).is_err());
    assert!(!crate::http::viewer_allows(&form, Some("Send")), "viewer mode never sends");
}

/// Replay server for manual interface checks:
///   DOCKET_CAPTURES=... cargo test replay_server -- --ignored --nocapture
/// then run a debug build with DOCKET_PORTAL=<printed URL>.
#[test]
#[ignore]
fn replay_server() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let (base, _seen) = serve(dir);
    println!("DOCKET_PORTAL={base}/conferences/scripts/");
    let secs = std::env::var("DOCKET_REPLAY_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    std::thread::sleep(std::time::Duration::from_secs(secs));
}

/// IFAC (ifac-sms.org) and controls.papercept.net pages read like PaperPlaza's.
#[test]
#[ignore]
fn other_sites() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let load = |f: &str, url: &str| {
        let bytes = std::fs::read(format!("{dir}/{f}")).unwrap();
        crate::page::Page::parse(url, encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned())
    };
    let access = load("IFAC - Access page.html", "https://ifac-sms.org/conferences/scripts/login.pl");
    let js = crate::papercept::parse_journals(&access);
    assert_eq!(js.iter().map(|j| j.code.as_str()).collect::<Vec<_>>(), ["automatica"]);
    assert!(js[0].url.starts_with(&format!("https://ifac-sms.org/journals/automatica/scripts/login.pl?PIN={}", x("ifac.pin"))), "{}", js[0].url);

    let journal = load("IFAC - Automatica Access Page.html", "https://ifac-sms.org/journals/automatica/scripts/login.pl");
    assert!(journal.commands().iter().any(|(_, c)| c == "AccessReviewerspace"));

    let ws = load("IFAC - Reviewer's Workspace - 2.html", "https://ifac-sms.org/journals/automatica/scripts/reviewerspace.pl");
    let a = crate::papercept::parse_assignments(&ws, "automatica");
    assert_eq!(a.len(), xn("ifac.items"));
    let r = a.iter().find(|r| r.review_id == x("ifac.review.id")).unwrap();
    assert_eq!(
        (r.number.clone(), r.recommendation.clone(), r.requested_by.clone(), r.requester_email.clone()),
        (x("ifac.review.number"), x("ifac.review.recommendation"), x("ifac.review.requested_by"), x("ifac.review.requester_email"))
    );

    let pc = load("Papercept - Access page.html", "https://controls.papercept.net/conferences/scripts/login.pl");
    let js = crate::papercept::parse_journals(&pc);
    assert_eq!(js.iter().map(|j| j.code.as_str()).collect::<Vec<_>>(), ["uam", "joe"]);
    assert!(js.iter().all(|j| j.url.starts_with("https://controls.papercept.net/journals/")));
}

/// Reading a submitted review (editor) and the reviews of a paper (reviewer),
/// both allowed in viewer mode.
#[test]
#[ignore]
fn read_reviews() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let (base, _seen) = serve(dir);
    let mut s = editor(&base, true);
    let paper = the_paper(&mut s);
    let reviews = s.load_reviews(&paper).unwrap();
    let r = reviews.iter().find(|r| r.review_id == x("paper.received_review.id")).unwrap();
    let t = s.review_text(r).unwrap();
    assert_eq!(t.reviewer, x("paper.received_review.reviewer"));
    let q = |name: &str| t.answers.iter().find(|a| a.question == name).map(|a| a.answer.clone()).unwrap_or_default();
    assert_eq!(q("Overall recommendation"), x("paper.received_review.recommendation"));
    assert_eq!(q("Confidential comments to the Editorial Staff"), x("paper.received_review.confidential"));
    assert!(q("Comments to the author").starts_with(&x("paper.received_review.comments_start")));
    assert!(t.answers.iter().all(|a| !a.answer.is_empty()), "empty answers are left out");
    let canceled = reviews.iter().find(|r| r.review_id == x("paper.canceled_review.id")).unwrap();
    assert!(s.review_text(canceled).is_err(), "a canceled review has no text");

    s.enter_role("AccessReviewerspace").unwrap();
    let mine = x("reviewer.submitted.id");
    let i = s.inspect_reviews(&mine).unwrap();
    assert_eq!((i.number.clone(), i.current_version.clone(), i.current_status.clone()), (x("reviewer.submitted.number"), x("reviewer.submitted.version"), x("reviewer.submitted.status")));
    assert_eq!(i.versions.len(), 1);
    let v = &i.versions[0];
    let rec = x("reviewer.submitted.recommendation");
    assert_eq!(v.reviews.iter().map(|r| (r.review_id.clone(), r.mine, r.recommendation.clone())).collect::<Vec<_>>(), [(mine, true, rec.clone()), (x("reviewer.submitted.other_review"), false, rec)]);
    assert_eq!(v.reviews[1].comments, x("reviewer.submitted.other_comments"));
    assert_eq!(v.decisions, [x("reviewer.submitted.decision")]);
}

#[test]
#[ignore]
fn files() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let att = std::fs::read(format!("{dir}/Review Details - Attachment.html")).unwrap();
    let page = crate::page::Page::parse("https://css.paperplaza.net/journals/tac/scripts/reviews.pl", encoding_rs::WINDOWS_1252.decode(&att).0.into_owned());
    let t = crate::papercept::parse_review_text(&page).unwrap();
    assert_eq!(t.attachments.len(), 1);
    let a = &t.attachments[0];
    assert_eq!((a.label.as_str(), a.about.as_str()), ("Attachment to the Comments to the Author", "Pdf file, 573 KB"));
    assert!(a.command.starts_with("Download_Review") && a.command.ends_with("__pdf"), "{}", a.command);
    assert!(t.answers.iter().all(|x| !x.question.starts_with("Attachment")), "the attachment is not an answer");
    assert!(t.answers.iter().any(|x| x.question == "Comments to the author" && x.answer == "See the attachment."));

    let (base, seen) = serve(dir);
    // Downloading is allowed in viewer mode.
    let mut s = editor(&base, true);
    let papers = s.load_papers(false).unwrap();
    let paper = papers.iter().find(|p| p.id == x("paper.id")).unwrap();
    assert_eq!(paper.download_cmd, x("files.manuscript_cmd"));
    let other = papers.iter().find(|p| p.id == x("files.version2.id")).unwrap();
    assert_eq!(other.download_cmd, x("files.version2.cmd"));
    let d = s.manuscript(paper).unwrap();
    assert!(d.bytes.starts_with(b"%PDF") && d.mime == "application/pdf");
    assert!(seen.lock().unwrap().iter().any(|y| String::from_utf8_lossy(&y.body).contains(&format!("Command={}", x("files.manuscript_cmd")))));
    // The site answering with a page instead of the file is reported.
    let no_file = papers.iter().find(|p| p.id == x("files.page_instead")).unwrap();
    let e = s.manuscript(no_file).unwrap_err().to_string();
    assert!(e.contains("instead of the file"), "{e}");

    let reviews = s.load_reviews(paper).unwrap();
    let r = reviews.iter().find(|r| r.review_id == x("paper.received_review.id")).unwrap();
    assert!(s.review_attachment(r, &format!("Download_Review{}__pdf", r.review_id)).is_err(), "that review has no attachment link");

    // Reviewer: the submission popup (this one lists no files).
    s.enter_role("AccessReviewerspace").unwrap();
    let mine = s.load_assignments(true).unwrap();
    let cmd = x("files.reviewer_details_cmd");
    assert_eq!(mine.iter().find(|a| a.number.starts_with(&x("files.reviewer_paper"))).map(|a| a.details_cmd.clone()), Some(cmd.clone()));
    assert_eq!(s.submission_files(&cmd).unwrap().len(), 0);
}

/// Reviews of an earlier submission, listed under "Previous Submission …".
#[test]
#[ignore]
fn previous_submission() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let bytes = std::fs::read(format!("{dir}/Manage Reviews - Versions.html")).unwrap();
    let page = crate::page::Page::parse("https://css.paperplaza.net/journals/tac/scripts/reviews.pl", encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned());
    let rs = crate::papercept::parse_reviews(&page, &x("versions.paper"), "", &x("versions.reviews_cmd"));
    assert_eq!(rs.len(), xn("versions.count"));
    let own: Vec<String> = rs.iter().filter(|r| r.of_version("1")).map(|r| r.review_id.clone()).collect();
    assert_eq!(own, xs("versions.own"));
    let previous = x("versions.previous");
    assert!(rs[own.len()..].iter().all(|r| r.from_submission == previous && r.version == "1"));
}

/// A message to the paper's senior editor through its Contact page.
#[test]
#[ignore]
fn contact_staff() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let (base, seen) = serve(dir);
    let mut s = editor(&base, false);
    let paper = the_paper(&mut s);

    let (handlers, subject) = s.staff_contacts(&paper).unwrap();
    assert_eq!(subject, format!("IEEE-TAC {}", paper.id));
    assert_eq!(handlers.len(), 2);
    let se = &handlers[0];
    assert!(se.role == "Senior Editor" && se.pin.chars().all(|c| c.is_ascii_digit()) && !se.pin.is_empty(), "{se:?}");
    assert!(se.address.starts_with(&format!("{} <", se.name)) && se.address.ends_with(&format!("<{}>", se.email)), "{se:?}");
    assert_eq!(handlers[1].role, "Associate Editor");

    let opts = ContactOptions { do_not_archive: false, my_eyes_only: false, do_not_copy_me: true };
    assert!(s.contact_staff(&paper, &["someone@else.org".into()], "", "", "Hello", &opts, None).is_err(), "only listed handlers");
    let before = seen.lock().unwrap().len();
    let subject = format!("IEEE-TAC {}: question", paper.id);
    let sent = s.contact_staff(&paper, std::slice::from_ref(&se.address), &subject, "", "Dear colleague,\n\nA question.", &opts, None).unwrap();
    assert!(sent.starts_with("Sent to ") && sent.contains("(Senior Editor)"), "{sent}");
    let log = seen.lock().unwrap();
    let sends: Vec<_> = log[before..].iter().filter(|x| multipart_value(&String::from_utf8_lossy(&x.body), "Command").as_deref() == Some("SendMessage")).collect();
    assert_eq!(sends.len(), 1, "one message sent");
    let post = sends[0];
    assert!(post.path.ends_with("/contact.pl"));
    let body = String::from_utf8_lossy(&post.body);
    assert!(post.content_type.starts_with("multipart/form-data"));
    for (name, value) in [("Command", "SendMessage"), ("Subject", subject.as_str()), ("Submission_ID", paper.id.as_str()), ("DoNotCopyMe", "ON")] {
        assert_eq!(multipart_value(&body, name).as_deref(), Some(value), "{name}");
    }
    assert_eq!(multipart_value(&body, "To").as_deref(), Some(se.address.as_str()));
    assert!(multipart_value(&body, "MyEyesOnly").is_none());
}

#[test]
#[ignore]
fn site_notes() {
    use crate::notes::{Kind, Notes, Repeat, Rule, ThanksSettings};
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let _guard = crate::creds::folder_lock();
    let read = |file: &str, script: &str| {
        let html = encoding_rs::WINDOWS_1252.decode(&std::fs::read(format!("{dir}/{file}")).unwrap()).0.into_owned();
        crate::page::Page::parse(&format!("https://css.paperplaza.net/journals/tac/scripts/{script}"), html)
    };
    // The site's three lists.
    let id = x("thanks.review_id");
    let accept_box = format!("ThankYouForAccepting{id}");
    let list = crate::papercept::parse_unthanked(&read("Send Thank You Notes.html", "sendthankyounotes.pl"));
    assert_eq!(list.len(), xn("thanks.rows"));
    let u = list.iter().find(|u| u.review_id == id).unwrap();
    assert_eq!((u.state.as_str(), u.paper.clone(), u.reviewer.clone()), ("Confirmed", x("thanks.paper"), x("thanks.reviewer")));
    assert_eq!((u.accept_box.clone(), u.review_box.clone()), (Some(accept_box.clone()), None));
    let rem = crate::papercept::parse_site_list(&read("Send Review Reminders.html", "sendreminders.pl"));
    assert_eq!(rem.len(), xn("site_lists.reminders.rows"));
    let early = rem.iter().find(|i| i.review_id == x("site_lists.reminders.early_id")).unwrap();
    assert_eq!(early.days, Some(xv("site_lists.reminders.early_days").as_i64().unwrap()), "negative days are read (written with a minus sign)");
    assert!(early.due.is_some() && early.previous.is_empty());
    let twice = rem.iter().find(|i| i.review_id == x("site_lists.reminders.twice_id")).unwrap();
    assert_eq!((twice.days, twice.previous.len()), (Some(xv("site_lists.reminders.twice_days").as_i64().unwrap()), 2));
    assert!(twice.previous[0] < twice.previous[1] && rem.iter().all(|i| i.checkbox.starts_with("Include") && !i.paper.is_empty() && !i.reviewer.is_empty()));
    let conf = crate::papercept::parse_site_list(&read("Request Review Confirmations.html", "requestconfirmations.pl"));
    assert_eq!(conf.len(), xn("site_lists.confirmations.rows"));
    assert_eq!((conf[0].review_id.clone(), conf[0].days), (x("site_lists.confirmations.first_id"), Some(xv("site_lists.confirmations.first_days").as_i64().unwrap())));

    let (base, seen) = serve(dir.clone());
    let long_ago = chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
    let on = |r: bool| Rule { on: r, since: None };
    let fresh = |accept: bool, confirm: bool, remind: bool, ask: bool| {
        let mut n = Notes::default();
        let rep = |on: bool, max: u32| Repeat { on, after_days: 7, max, ask, copy_me: true, attach: true, letter: String::new() };
        n.set(ThanksSettings { accept: on(accept), review: on(false), copy_me: false, ask, ..Default::default() }, rep(confirm, 2), rep(remind, 3), Some((crate::papercept::SITES[0].0, "tac", "IEEE-TAC", true)), long_ago);
        n
    };
    let sends = |cmd: &str| {
        let want = format!("Command={cmd}");
        seen.lock().unwrap().iter().filter(|q| String::from_utf8_lossy(&q.body).split('&').any(|kv| kv == want)).map(|q| (q.path.clone(), String::from_utf8_lossy(&q.body).into_owned())).collect::<Vec<_>>()
    };
    let checked = |body: &str, prefix: &str| body.split('&').filter(|kv| kv.starts_with(prefix)).map(String::from).collect::<Vec<_>>();

    // Viewer mode reads the lists but sends and records nothing.
    let mut n = fresh(true, true, true, false);
    let report = crate::notes::run(&mut editor(&base, true), &mut n, None);
    assert!(report.iter().any(|l| l.contains("viewer mode — would send a thank-you note")), "{report:#?}");
    assert!(report.iter().any(|l| l.contains("viewer mode — would send a review reminder")), "{report:#?}");
    assert!(sends("SendThankYouNotes").is_empty() && sends("SendReminders").is_empty() && n.done.is_empty());

    // Ask me first: everything due is held, nothing sent.
    let mut s = editor(&base, false);
    let mut n = fresh(true, true, true, true);
    crate::notes::run(&mut s, &mut n, None);
    assert!(sends("SendThankYouNotes").is_empty() && sends("SendReminders").is_empty() && n.done.is_empty());
    let kinds = |n: &Notes, k: Kind| n.pending.iter().filter(|p| p.kind == k).count();
    let (held_r, held_c) = (kinds(&n, Kind::Remind), kinds(&n, Kind::Confirm));
    assert!(kinds(&n, Kind::Accept) == 1 && held_r > 0 && held_c > 0, "{:#?}", n.pending);
    assert!(!n.pending.iter().any(|p| p.review_id == x("site_lists.reminders.early_id") && p.kind == Kind::Remind), "not overdue");
    assert!(n.pending.iter().any(|p| p.review_id == x("site_lists.reminders.twice_id") && p.kind == Kind::Remind), "reminded twice, at most 3");
    // Approving one reminder sends only that one, with the site's options; the others stay held.
    let one = n.pending.iter().find(|p| p.kind == Kind::Remind).unwrap().clone();
    crate::notes::run(&mut s, &mut n, Some(&[one.key.clone()]));
    let sent = sends("SendReminders");
    assert_eq!(sent.len(), 1);
    let (path, body) = &sent[0];
    assert!(path.ends_with("/journals/tac/scripts/sendreminders.pl"), "{path}");
    let boxes = checked(body, "Include");
    assert_eq!(boxes.len(), 1, "{boxes:?}");
    assert!(boxes[0].ends_with(&format!("={}", one.review_id)));
    assert!(body.split('&').any(|kv| kv == "SendCopies=ON") && body.split('&').any(|kv| kv == "AttachSubmission=ON") && !body.split('&').any(|kv| kv.starts_with("CheckAll=")));
    assert!(body.contains("Cover_message=Dear+%25YOUR_TITLE%25"), "the site's letter, unchanged");
    assert_eq!(n.done.len(), 1);
    assert!(n.done[0].outcome.starts_with("not confirmed"), "the saved list does not change after sending: {}", n.done[0].outcome);
    assert_eq!((kinds(&n, Kind::Remind), kinds(&n, Kind::Confirm)), (held_r - 1, held_c));
    assert!(crate::activity::load().iter().any(|e| e.kind == "reminder" && e.paper == one.paper));

    // Sent right away: thank-you note and confirmation requests (at most 2, the site's earlier ones count).
    let mut n = fresh(true, true, false, false);
    let report = crate::notes::run(&mut s, &mut n, None);
    let thanks = sends("SendThankYouNotes");
    assert_eq!(thanks.len(), 1, "{report:#?}");
    assert_eq!(checked(&thanks[0].1, "ThankYouFor"), [format!("{accept_box}=ON")]);
    assert!(thanks[0].1.contains("Cover_message_1=Dear+%25YOUR_TITLE%25"));
    let conf_sent = sends("SendReminders");
    assert_eq!(conf_sent.len(), 2);
    assert!(conf_sent[1].0.ends_with("/requestconfirmations.pl"));
    let due_conf = conf.iter().filter(|i| i.days.unwrap() >= 7 && i.previous.len() < 2 && i.previous.last().is_none_or(|d| (chrono::Local::now().date_naive() - *d).num_days() >= 7)).count();
    assert_eq!(checked(&conf_sent[1].1, "Include").len(), due_conf);
    assert!(n.pending.is_empty());
    // Checked again: nothing is sent twice.
    let again = crate::notes::run(&mut s, &mut n, None);
    assert_eq!((sends("SendThankYouNotes").len(), sends("SendReminders").len()), (1, 2), "{again:#?}");

    // By hand, with one of the templates as the letter: the reviewer's name
    // becomes the site's placeholders, the rest is filled in.
    let tpl = |name: &str, body: &str| crate::templates::Template { name: name.into(), subject: String::new(), body: body.into() };
    crate::templates::Templates { signature: "J. Smith".into(), list: vec![tpl("Thanks", "Dear %reviewerfirstname,\n\nthank you.\n%signature"), tpl("Late", "Dear %reviewername, review %reviewid is late.")] }
        .save()
        .unwrap();
    let (rows, letter) = crate::notes::offered(&mut s, Kind::Accept).unwrap();
    assert!(rows.iter().any(|d| d.review_id == id) && letter.starts_with("Dear %YOUR_TITLE% %YOUR_GIVEN_NAME%"));
    let report = crate::notes::send_by_hand(&mut s, Kind::Accept, &[id.clone(), "999999999".into()], Some("Dear %reviewerfirstname,\n\nthank you.\n%signature".into()), true, false).unwrap();
    assert!(report.iter().any(|l| l.contains("999999999") && l.contains("not on PaperPlaza's list")), "{report:#?}");
    let thanks = sends("SendThankYouNotes");
    assert_eq!(thanks.len(), 2);
    let body = &thanks[1].1;
    assert_eq!(checked(body, "ThankYouFor"), [format!("{accept_box}=ON")]);
    assert!(body.contains("Cover_message_1=Dear+%25YOUR_GIVEN_NAME%25%2C") && body.contains("thank+you.") && body.contains("J.+Smith"), "{body}");
    assert!(body.split('&').any(|kv| kv == "SendCopies=ON"));
    // A letter with values of the review: each review is sent on its own, filled
    // from its paper's Manage Reviews page (the replay has two papers' pages).
    let (rows, _) = crate::notes::offered(&mut s, Kind::Remind).unwrap();
    assert_eq!(rows.len(), rem.len());
    let mut known: Vec<crate::model::Review> = Vec::new();
    for p in s.load_papers(false).unwrap() {
        known.extend(s.load_reviews(&p).unwrap_or_default());
    }
    let ids: Vec<String> = rows.iter().filter(|d| known.iter().any(|r| r.review_id == d.review_id)).take(2).map(|d| d.review_id.clone()).collect();
    assert!(!ids.is_empty(), "a reminder row of a replayed paper");
    let before = sends("SendReminders").len();
    let letter = "Dear %reviewername, review %reviewid of “%title” is late.";
    let report = crate::notes::send_by_hand(&mut s, Kind::Remind, &ids, Some(letter.into()), false, false).unwrap();
    let sent = sends("SendReminders");
    assert_eq!(sent.len(), before + ids.len(), "{report:#?}");
    // The form is sent in the site's encoding (Windows-1252).
    let field = |body: &str, name: &str| {
        let v = body.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).unwrap_or_default().replace('+', " ");
        let bytes: Vec<u8> = percent_encoding::percent_decode_str(&v).collect();
        encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned()
    };
    for (q, rid) in sent[before..].iter().zip(&ids) {
        let r = known.iter().find(|r| &r.review_id == rid).unwrap();
        assert_eq!(checked(&q.1, "Include").len(), 1);
        let want = format!("Dear %YOUR_GIVEN_NAME% %YOUR_SURNAME%, review {rid} of “{}” is late.", r.paper_title);
        assert_eq!(field(&q.1, "Cover_message"), want);
        assert!(!q.1.split('&').any(|kv| kv == "SendCopies=ON" || kv == "AttachSubmission=ON"));
    }
    // A review whose paper's page cannot be read is reported, not sent with an unfilled letter.
    let other: Vec<String> = rows.iter().filter(|d| !known.iter().any(|r| r.review_id == d.review_id)).take(1).map(|d| d.review_id.clone()).collect();
    if !other.is_empty() {
        let report = crate::notes::send_by_hand(&mut s, Kind::Remind, &other, Some(letter.into()), false, false).unwrap();
        assert!(report.iter().any(|l| l.contains("not sent") && l.contains("reading the review for the letter")), "{report:#?}");
        assert_eq!(sends("SendReminders").len(), before + ids.len());
    }
}

/// The workspace's settings kept by the site ("Show tagged submissions only",
/// a search, another page) are reset before the papers are read.
#[test]
#[ignore]
fn workspace_settings_are_reset() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let tmp = std::env::temp_dir().join(format!("docket-sticky-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.file_name() != "Workspace.html" {
            std::os::unix::fs::symlink(e.path(), tmp.join(e.file_name())).unwrap();
        }
    }
    let html = encoding_rs::WINDOWS_1252.decode(&std::fs::read(format!("{dir}/Workspace.html")).unwrap()).0.into_owned();
    let sticky = html
        .replace(r#"name="ShowTaggedOnly" value="ON""#, r#"name="ShowTaggedOnly" value="ON" checked"#)
        .replace(r#"name="SearchFor" value="""#, r#"name="SearchFor" value="Doe""#)
        .replace(r#"name="Page" value="1""#, r#"name="Page" value="2""#);
    assert!(sticky.contains("ShowTaggedOnly\" value=\"ON\" checked") && sticky.contains("value=\"Doe\"") && sticky.contains("name=\"Page\" value=\"2\""));
    std::fs::write(tmp.join("Workspace.html"), encoding_rs::WINDOWS_1252.encode(&sticky).0).unwrap();
    let (base, seen) = serve(tmp.to_string_lossy().into_owned());
    let mut s = editor(&base, false);
    let papers = s.load_papers(false).unwrap();
    assert_eq!(papers.len(), xn("papers"));
    let resets: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.path.ends_with("/workspace.pl"))
        .map(|q| String::from_utf8_lossy(&q.body).into_owned())
        .filter(|b| !b.split('&').any(|kv| kv.starts_with("Command=")))
        .collect();
    assert_eq!(resets.len(), 1, "one submit resets the settings");
    let kv: Vec<&str> = resets[0].split('&').collect();
    assert!(kv.contains(&"SearchFor=") && kv.contains(&"Page=1"), "{}", resets[0]);
    assert!(!kv.iter().any(|x| x.starts_with("ShowTaggedOnly=") || x.starts_with("ShowAll=")), "{}", resets[0]);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A mark the site may put after a reviewer's PIN ("Doe, Jane (12345)§") is
/// not part of the name, and the PIN is still read.
#[test]
#[ignore]
fn marks_after_the_pin() {
    let Ok(dir) = std::env::var("DOCKET_CAPTURES") else { return };
    let html = encoding_rs::WINDOWS_1252.decode(&std::fs::read(format!("{dir}/Manage Reviews.html")).unwrap()).0.into_owned();
    let url = "https://css.paperplaza.net/journals/tac/scripts/workspace.pl";
    let (id, cmd) = (x("paper.id"), x("paper.reviews_cmd"));
    let plain = crate::papercept::parse_reviews(&crate::page::Page::parse(url, html.clone()), &id, "", &cmd);
    assert!(!plain.is_empty() && plain.iter().all(|r| !r.pin.is_empty()));
    let mut marked = html;
    for r in &plain {
        marked = marked.replace(&format!("({})", r.pin), &format!("({})§", r.pin));
    }
    let again = crate::papercept::parse_reviews(&crate::page::Page::parse(url, marked), &id, "", &cmd);
    assert_eq!(again.len(), plain.len());
    for (a, b) in again.iter().zip(&plain) {
        assert_eq!((&a.reviewer, &a.pin, &a.first_name, &a.last_name), (&b.reviewer, &b.pin, &b.first_name, &b.last_name));
        assert!(!a.reviewer.contains('§') && !a.first_name.contains('('));
    }
}
