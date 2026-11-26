#[cfg(target_os = "macos")]
mod app;
mod creds;
mod http;
mod model;
mod page;
mod papercept;
mod recommend;
mod scheduler;
mod templates;
mod notes;
mod letters;
mod activity;
#[cfg(test)]
mod replay_test;
mod ui;

use anyhow::{Context, Result, bail};
use papercept::Session;
use std::path::PathBuf;

const USAGE: &str = "Docket

Usage:
  docket [--trace DIR] [--no-browser]
                              Start the app in your browser (default from a terminal)
  docket app [--background] [--trace DIR]
                              Start the app in its own windows (what Docket.app does);
                              --background starts it in the menu bar only
  docket list [--trace DIR]      Log in from the terminal and print all reviews (read-only)
  docket run-schedules [--viewer] [--trace DIR]
                              Check the automatic invitations and send the site notes
                              (thanks, confirmation requests, reminders) once, using the
                              login and password saved in the Keychain (for cron/launchd).
                              Follows Settings → Automation (pause, sending hours).
                              With --viewer nothing is sent; the log says what would be.
  docket parse FILE.html...      Show what the tool reads from saved PaperPlaza pages
  docket forget                  Remove the saved passwords from the Keychain

--trace DIR saves every page fetched and every form sent (password redacted)
to DIR, for troubleshooting. The files contain confidential review data.";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    if !cfg!(test) && std::env::var_os("DOCKET_CONFIG_DIR").is_none() {
        creds::migrate_old_settings();
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let trace = take_opt(&mut args, "--trace")?.map(PathBuf::from);
    let no_browser = take_flag(&mut args, "--no-browser");
    let viewer = take_flag(&mut args, "--viewer");
    let background = take_flag(&mut args, "--background");
    match args.first().map(String::as_str) {
        None if in_app_bundle() => windows(trace, background),
        None | Some("ui") => ui::serve(trace, !no_browser),
        Some("app") => windows(trace, background),
        Some("list") => list(trace),
        Some("run-schedules") => run_schedules(trace, viewer),
        Some("parse") => parse(&args[1..]),
        Some("forget") => {
            let cfg = creds::load_config();
            for (site, _) in papercept::SITES {
                if let Some(id) = cfg.login_for(site) {
                    if creds::delete_password(&creds::account(site, &id)).is_ok() {
                        println!("Removed the saved password for {id} on {site}.");
                    }
                }
            }
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command '{other}'\n\n{USAGE}"),
    }
}

/// Started as Docket.app (the binary is in Docket.app/Contents/MacOS).
fn in_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

#[cfg(target_os = "macos")]
fn windows(trace: Option<PathBuf>, background: bool) -> Result<()> {
    app::run(trace, background)
}

#[cfg(not(target_os = "macos"))]
fn windows(trace: Option<PathBuf>, _background: bool) -> Result<()> {
    ui::serve(trace, true)
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    let found = args.iter().any(|a| a == name);
    args.retain(|a| a != name);
    found
}

fn take_opt(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == name) else { return Ok(None) };
    if i + 1 >= args.len() {
        bail!("{name} needs a value");
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Ok(Some(v))
}

fn prompt(label: &str) -> Result<String> {
    use std::io::Write;
    print!("{label}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

fn list(trace: Option<PathBuf>) -> Result<()> {
    let cfg = creds::load_config();
    let site = cfg.site.clone().unwrap_or_else(|| papercept::SITES[0].0.to_string());
    let id = match cfg.login_for(&site) {
        Some(id) => id,
        None => prompt(&format!("PIN, login alias or e-mail on {site}: "))?,
    };
    let pw = match creds::get_password(&creds::account(&site, &id)) {
        Some(p) => p,
        None => rpassword::prompt_password("Password: ")?,
    };
    let mut s = Session::new(trace, true)?.for_site(&site)?;
    s.login(&id, &pw)?;
    let code = cfg.journal.clone().unwrap_or_else(|| "tac".into());
    s.enter_journal(&code)?;
    let role = s
        .roles
        .iter()
        .find(|r| r.label.contains("Associate Editor"))
        .or(s.roles.first())
        .cloned()
        .context("no editorial role on this journal")?;
    s.enter_role(&role.command)?;
    let papers = s.load_papers(false)?;
    for p in &papers {
        println!("\n{}  {}  [{}]", p.number, p.title, p.status);
        for r in s.load_reviews(p)? {
            println!("   {:>7}  {:<32} {}", r.review_id, format!("{} ({})", r.reviewer, r.pin), r.status_raw);
        }
    }
    Ok(())
}

fn run_schedules(trace: Option<PathBuf>, viewer: bool) -> Result<()> {
    let cfg = creds::load_config();
    if let Err(why) = cfg.automation.may_send(chrono::Local::now().naive_local()) {
        println!("Nothing checked: {why}.");
        return Ok(());
    }
    let mut store = scheduler::Store::load();
    let mut notes = notes::Notes::load();
    let on = |store: &scheduler::Store, notes: &notes::Notes, site: &str| {
        store.schedules.iter().any(|x| x.site == site && x.state == scheduler::State::Active) || notes.is_on(site)
    };
    if !papercept::SITES.iter().any(|(site, _)| on(&store, &notes, site)) {
        println!("No active schedules and no site notes turned on.");
        return Ok(());
    }
    for (site, _) in papercept::SITES {
        if !on(&store, &notes, site) {
            continue;
        }
        let Some(id) = cfg.login_for(site) else {
            println!("{site}: log in once in the app first");
            continue;
        };
        let Some(pw) = creds::get_password(&creds::account(site, &id)) else {
            println!("{site}: no password saved in the Keychain; log in once in the app with “Save the password”");
            continue;
        };
        let mut s = Session::new(trace.clone(), viewer)?.for_site(site)?;
        s.login(&id, &pw)?;
        for line in scheduler::run(&mut s, &mut store, None) {
            println!("{line}");
        }
        for line in notes::run(&mut s, &mut notes, None) {
            println!("{line}");
        }
        if !notes.pending.is_empty() {
            println!("{} site note(s) wait for your approval in the app.", notes.pending.len());
        }
        s.logout();
    }
    Ok(())
}

fn parse(files: &[String]) -> Result<()> {
    if files.is_empty() {
        bail!("give one or more saved .html pages");
    }
    for f in files {
        let bytes = std::fs::read(f).with_context(|| f.clone())?;
        let (html, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
        let html = html.into_owned();
        let url = saved_from(&html).unwrap_or_else(|| "https://css.paperplaza.net/journals/tac/scripts/unknown.pl".into());
        let page = page::Page::parse(&url, html);
        println!("== {f}\n   title: {}\n   url: {url}", page.title);
        if let Some(form) = page.form() {
            println!("   form: {} {} (multipart: {}) — {} controls, {} allowed names", form.method, form.action, form.multipart, form.controls.len(), form.allowed().len());
        }
        let journals = papercept::parse_journals(&page);
        if !journals.is_empty() {
            println!("   journals: {}", journals.iter().map(|j| format!("{} ({})", j.name, j.code)).collect::<Vec<_>>().join(", "));
        }
        let roles: Vec<_> = page.commands().into_iter().filter(|(_, c)| c.starts_with("Access_") || c == "AccessReviewerspace").collect();
        if !roles.is_empty() {
            println!("   roles: {roles:?}");
        }
        for p in papercept::parse_workspace(&page) {
            println!("   paper {} id={} cmd={} [{}] due {} — {}", p.number, p.id, p.reviews_cmd, p.status, p.next_due, p.title);
        }
        let sid = page.hidden("Submission_ID").unwrap_or_default();
        for r in papercept::parse_reviews(&page, &sid, "", "") {
            println!(
                "   review {} {} ({}) v{}{} state={} date={:?} due={:?} reminders={} thanked={} overdue={} rec={:?} cmd={}",
                r.review_id, r.reviewer, r.pin, r.version, if r.from_submission.is_empty() { String::new() } else { format!(" of previous {}", r.from_submission) }, r.state, r.state_date, r.due, r.reminders, r.thanked, r.overdue, r.recommendation, r.details_cmd
            );
        }
        let mut cands = papercept::parse_candidates(&page);
        if !cands.is_empty() {
            let t = std::time::Instant::now();
            let info = papercept::parse_paper_info(&page);
            recommend::rank(&info, &mut cands);
            println!("   paper: {:?} keywords {:?}, {} authors; {} reviewers in your list ({} available), ranked in {:?}",
                info.title, info.keywords, info.authors.len(), cands.len(), cands.iter().filter(|c| c.available).count(), t.elapsed());
            for c in cands.iter().take(5) {
                println!("   {:>3} {} ({}) [{}] {:?} {:?}", c.score, c.name, c.pin, c.affiliation, c.reasons, c.conflict);
            }
        }
        let hits = papercept::parse_pin_results(&page);
        if !hits.is_empty() {
            println!("   PIN search: {} people, {} can be added, {} already on your list", hits.len(), hits.iter().filter(|c| c.available && !c.in_list).count(), hits.iter().filter(|c| c.in_list).count());
            for c in hits.iter().take(3) {
                println!("   pin {} {} [{}] in_list={} available={}", c.pin, c.name, c.affiliation, c.in_list, c.available);
            }
        }
        let (abs, kws) = papercept::parse_abstract(&page);
        if !abs.is_empty() {
            println!("   abstract: {}… keywords {kws:?}", abs.chars().take(80).collect::<String>());
        }
        let mine = papercept::parse_assignments(&page, "");
        if !mine.is_empty() {
            println!("   {} reviews requested from you", mine.len());
            for a in mine.iter().take(5) {
                println!("   assignment {} {} v{} [{}] due {:?} rec={:?} by {} ({}) <{}> — {}", a.review_id, a.number, a.version, a.status_raw, a.due, a.recommendation, a.requested_by, a.requester_role, a.requester_email, a.title);
            }
        }
        if let Some(rt) = papercept::parse_review_text(&page) {
            println!("   review {} by {} [{}], {} answers", rt.review_id, rt.reviewer, rt.form, rt.answers.len());
            for a in &rt.answers {
                println!("   · {}: {}", a.question, a.answer.chars().take(70).collect::<String>().replace('\n', " "));
            }
        }
        let ins = papercept::parse_inspection(&page);
        if !ins.versions.is_empty() {
            println!("   inspect {} — {} [v{}, {}, {}]", ins.number, ins.title, ins.current_version, ins.current_status, ins.kind);
            for v in &ins.versions {
                println!("   {}", v.heading);
                for r in &v.reviews {
                    println!("     {}{} #{} rec={} — {}", r.review_id, if r.mine { " (mine)" } else { "" }, r.reviewer_no, r.recommendation, r.comments.chars().take(60).collect::<String>().replace('\n', " "));
                }
                for d in &v.decisions {
                    println!("     decision: {d}");
                }
            }
        }
        if let Some(to) = papercept::contact_recipient(&page) {
            println!("   contact form recipient: {to}; Review_ID={:?}", page.hidden("Review_ID"));
        }
    }
    Ok(())
}

fn saved_from(html: &str) -> Option<String> {
    let i = html.find("saved from url=(")?;
    let rest = &html[i..];
    let start = rest.find(')')? + 1;
    let url: String = rest[start..].chars().take_while(|c| !c.is_whitespace() && *c != '-').collect();
    Some(url)
}
