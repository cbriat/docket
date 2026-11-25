# Docket

Docket is a Mac app for **associate editors and reviewers** who work on
PaperCept journal sites: css.paperplaza.net (e.g. IEEE-TAC), ifac-sms.org
(e.g. Automatica) and controls.papercept.net. It shows all your papers and
review requests in one table, runs actions on many reviews at once, and keeps
papers supplied with reviewers automatically.

Docket is a personal tool and is not affiliated with PaperCept. It works with
your own account, on the same pages you use in the browser.

## Features

- **One table** of every paper in your workspace and every review request:
  status, due date (red when overdue), reminders and recommendations, grouped
  by version. Filter, search, collapse papers, export to CSV.
- **Message many reviewers at once** through PaperPlaza's *Contact the
  reviewer*, with templates, placeholders and a live preview, so every
  message is archived with its submission.
- **Message the senior editor** of a paper through its *Contact* page.
- **Cancel** many review requests, or **extend** many deadlines, in one go.
- **Find reviewers**: your Reviewers list ranked against the paper's
  keywords, title and abstract, with possible conflicts of interest flagged.
  You can also search the whole PIN database.
- **Invite** reviewers right away, or set up **automatic invitations** that
  invite the next name whenever someone declines or does not answer, until
  enough reviewers have accepted.
- **Site notes**: PaperPlaza's own thank-you notes, confirmation requests
  and overdue reminders, sent automatically or after your approval.
- **Settings** (⌘,): automation pause, check interval and sending hours,
  open at login, notifications, defaults, and an activity log of everything
  Docket sent by itself.
- **Read reviews and PDFs**: the manuscript, the reviews with their
  attachments, and, as a reviewer, the other reviews of papers you reviewed.
- **Reviewer role**: the reviews requested from you, in any journal.
- **Menu-bar icon** with a summary and the everyday commands, and native
  menus for everything else.
- **Viewer mode** for trying Docket safely: nothing is ever sent.

## Requirements

- macOS 12 or later (built and tested on Apple silicon).
- To build: [Rust](https://rustup.rs) 1.90 or later and the Xcode command
  line tools (`xcode-select --install`).
- An account on one of the PaperCept sites above, with an Associate Editor
  or Reviewer role in a journal.

## Build and install

```sh
git clone https://github.com/cbriat/docket.git
cd docket
scripts/build-app.sh                          # builds target/Docket.app
ditto target/Docket.app /Applications/Docket.app
```

Or download `Docket-<version>-macOS-arm64.zip` from the
[Releases](https://github.com/cbriat/docket/releases) page (Apple silicon),
unzip it and move Docket.app to Applications.

The app is signed ad hoc (no Apple developer certificate), which is enough to
run it on the Mac it was built on. On another Mac, macOS blocks it the first
time: open it once, then go to System Settings → Privacy & Security and click
*Open Anyway* (on macOS 14 and earlier, right-click → *Open* also works).
After each rebuild or update, macOS asks once whether Docket may use the
passwords saved in the Keychain; answer *Always Allow*.

## Getting started

Docket runs in two ways, with the same features and the same settings:

- **Docket.app** (or `docket app` from a terminal): its own windows. The
  reviewer picker, the message window, the reviews you read, paper details
  and PDFs each open in a separate window that you can move, resize and keep
  next to the main list. Each kind of window reopens where you left it.
- **In a web browser**: run `docket` from a terminal (or `docket
  --no-browser` to only print the link). The same features open as dialogs
  in the page.

Choose the site, then log in with your PIN or e-mail and your password for
that site (each site has its own accounts). You can save the password in the
macOS Keychain, separately for each site. Then pick the journal and the role
(Associate Editor or Reviewer); both are remembered. Switch journal or role
at any time with the two menus at the top of the window.

**Viewer mode** (a checkbox on the login page) is for trying Docket safely.
Only requests that open pages are sent to the site; anything that would send,
cancel or change something is refused inside Docket before it reaches the
site. A *Viewer mode* badge is shown while it is on.

## Using Docket

### The table

Each paper is followed by its review requests: reviewer, review number,
status, due date, reminder marks (`*`) and recommendation. Reviews are
grouped by round, newest first: "Version 2 · current", "Version 1", then
"Previous submission 25-1234 · version 1" for the reviews PaperPlaza lists
under an earlier submission of the same paper. Older rounds are greyed out
and never count for automatic invitations.

Filter by status, search by reviewer, paper or title, and tick reviews to act
on them. The − / + button at the left of a paper hides or shows its reviews
(the one in the header does it for every paper); this is remembered per
journal. *Export CSV* saves what is shown.

Right-click a paper for: the manuscript (PDF), its details and abstract, the
submitted reviews, *Find reviewers and invite…*, *Message the senior
editor…*, selecting its reviews, and reminding, messaging or extending its
open reviews. Right-click a review for: a reminder, the review and its
attachments, its details, and message, extend or cancel (applied to the whole
selection if the row is part of it).

### Messages to reviewers

One message is sent per review, through PaperPlaza's *Contact the reviewer*
page, so it is archived with the submission. You can add Cc addresses, an
attachment (under 5 MB) and the archive options.

- **Templates** (*Edit templates…* in the message window): six to start from
  (overdue reminder, reminder before the deadline, invitation not answered,
  thank you, deadline extended, review no longer needed). Edit, add,
  duplicate or delete them, and set your signature.
- **Placeholders**, filled in for each reviewer: `%reviewerfirstname`
  `%reviewerlastname` `%reviewername` `%reviewerpin` `%reviewernumber`
  `%paper` `%paperid` `%title` `%authors` `%version` `%papertype`
  `%paperstatus` `%decisiondue` `%journal` `%reviewid` `%status` `%deadline`
  `%daysleft` `%daysoverdue` `%delay` `%reminders` `%requesteddate`
  `%accepteddate` `%receiveddate` `%canceleddate` `%lastreminderdate`
  `%today` `%signature`. Click one to insert it. Dates of requests,
  acceptances, reviews and reminders are read from each review's *Review
  Details* page, only when a message uses them. A misspelt placeholder is
  refused instead of being sent as written.
- **Live preview** beside the message shows the e-mail as one chosen
  reviewer will receive it; then every message is shown before anything is
  sent.
- **Reminders**: right-click a reviewer → *Send a reminder…*, or a paper →
  *Remind the open reviewers…*. The message window opens with the fitting
  template (overdue, due soon, or invitation not answered).
- **Remind overdue reviewers / nudge unanswered invitations** (Reviews menu
  or the menu-bar icon): selects the confirmed reviews that are overdue, or
  the invitations of the current version that nobody has answered, and opens
  the message window with the fitting template.

### Messages to the senior editor

Right-click a paper → *Message the senior editor…*. Docket opens the paper's
*Contact* page on PaperPlaza and lists its handlers (the senior editor and
you). The senior editor is ticked; the subject is the site's default, and the
message starts with a greeting and your signature. You can add Cc addresses,
an attachment and the archive options. The whole message is shown before it
is sent, and it is archived with the submission. Only the handlers listed on
that page can be chosen.

### Cancel and extend

- **Cancel review requests**: Docket reads the status back to confirm each
  cancellation. No cancellation notice is sent (see *Limitations*).
- **Extend deadlines** by 1 day to 4 weeks, using the site's own *Extend the
  deadline* function.

Canceled and received reviews are skipped.

### Finding and inviting reviewers

*Find reviewers and invite…* on a paper opens a window with:

- **Recommended reviewers**: every name in your Reviewers list gets a score
  from 0 to 100, computed on your computer (nothing is sent anywhere). 70%
  comes from the paper's keywords they share (rare keywords count more) and
  30% from words shared between the paper's title, keywords and abstract and
  their keywords. The reasons are shown. Possible conflicts of interest (an
  author, or the same affiliation as an author) are flagged and listed last.
  Filter by name, affiliation, keyword or PIN. *PIN from* keeps only people
  with that PIN or a higher one: PINs are given in order of registration, so
  a high PIN usually means a younger researcher. *Title* keeps only people
  who gave that title on PaperPlaza (Prof., Dr., Mr., Ms. or Mrs.; each
  person's title is shown next to the name).
- **The whole PIN database**: search by keywords (with AND, OR, NOT), surname,
  e-mail, affiliation or PIN. People found who are not on your Reviewers list
  are ranked with the others and can be added to it in one click (Docket runs
  PaperPlaza's own *Add to list*).
- **Invite now**: sends the invitations right away, as *Request a Review* →
  *Next* → *Send* does, with PaperPlaza's cover letter. You can set the
  deadline (default: the site's), attach the manuscript and get a copy.
  People not yet on your Reviewers list are added to it first. Each
  invitation is confirmed by finding the reviewer on the paper's *Manage
  Reviews* page.

### Automatic invitations

In the same window, give an ordered list of reviewers, the number of
reviewers needed and how many to keep invited at once. Whenever someone
declines, or (if you choose) does not answer within a set number of days and
is canceled, the next name is invited, until enough have accepted. Reviews
you requested by hand count toward the totals and are never canceled by
Docket. A name that cannot be invited is skipped, with the reason.

Schedules are checked every 30 minutes (Settings → Automation: 15, 30 or
60, at set hours only, or paused) while Docket is open (even with its
windows closed), and by `docket run-schedules` (e.g. from cron or launchd).
New schedules start from Settings → Invitations.
Each change sends a macOS notification. *Run Automatic Invitations Now…*
(Invitations menu or the menu-bar icon) checks every active schedule, shows
who would be invited or canceled, and sends only after you confirm. The
Invitations menu also pauses or resumes all schedules.

### Site notes: thanks, confirmation requests, reminders

Settings → Automation sends PaperPlaza's own notes, from its *Reminders and
thanks* pages and with the site's letters, so the site records them as if
you had sent them there. No personal message is sent. Turn them on for each
journal where you are Associate Editor (*Send site notes for my papers in
…*), then choose:

- **Thank-you notes** for agreeing to review and for submitting a review.
  Only acceptances and reviews dated from the day a note is turned on are
  thanked; the site lists only reviews it has not thanked.
- **Confirmation requests** to reviewers who have not answered an invitation:
  after N days, then every N days, at most M per review.
- **Review reminders** for overdue reviews: N days after the due date, then
  every N days, at most M per review. None is sent before the due date: the
  site's letter is written for late reviews.

Each kind can attach the submission (requests and reminders), send you a
copy, and **ask you first**: the notes then wait in *Send Pending Site
Notes…* (menu-bar icon, Reviews menu, or the button above the table), where
you untick any you do not want. Requests and reminders already recorded on
the site count toward the limits. After sending, Docket reads the site's list
again; a note it cannot confirm is shown as such and counts as sent, so it is
never repeated sooner. Everything sent is in Settings → Activity. In viewer
mode the checks only list what would be sent.

### Settings

Docket → *Settings…* ⌘, (or the menu-bar icon, or *Settings* in the browser).

- **General**: Dock icon; open Docket at login in the menu bar (Docket.app
  only; it then logs in with the saved password); which notifications to
  show (invitations, site notes, problems); how the table opens.
- **Automation**: pause everything automatic; check every 15, 30 or 60
  minutes; send only between set hours and/or on weekdays (what is held back
  goes at the next allowed check); site notes (above).
- **Invitations**: starting values of new automatic invitations.
- **Messages**: signature and templates; Cc and options new messages start
  with.
- **Accounts & data**: logins and saved passwords, automatic login, viewer
  mode at login, export or import the settings, open the settings folder.
- **Activity**: everything Docket sent or changed by itself (invitations,
  cancellations, site notes, problems), searchable, exportable to CSV.

### Reading reviews and PDFs

- **As editor**: right-click a reviewer → *Read the review* (the whole form
  from its *Review Details* page, confidential comments included), or a
  paper → *Read the submitted reviews*. Files attached to a review open from
  the review window. Right-click a paper → *Open the manuscript (PDF)* opens
  the current version.
- **As reviewer**: right-click a paper → *Read the reviews* (PaperPlaza's
  *Inspect the reviews*: the other reviewers' recommendations and comments
  to the authors for the versions you reviewed, and the decision), or *Open
  the paper (files)…*.

In the app, PDFs are shown by macOS's own PDF viewer (as in Preview). File →
*Open in Preview*, *Show in Finder* or *Save a Copy to Downloads*. Downloaded
files are kept in `~/Library/Caches/Docket` for the session only (emptied at
each start). All of this is allowed in viewer mode.

### Reviewer role

The reviews requested from you in a journal (current ones, or all past
ones), with requester, status, due date and your recommendation. Right-click
a row for its details, the paper's files, the other reviews, an e-mail to
the editor, or to copy the paper number. This view is read-only.

### Menu bar, Dock and menus (app)

- **Menu-bar icon** (a clipboard with ticks, at the top right of the
  screen). Click it for a summary (overdue reviews, unanswered invitations,
  active automatic invitations, site notes and those waiting for approval)
  and the everyday commands: Open Docket, Send Pending Site Notes…, Run
  Automatic Invitations Now…, Remind Overdue Reviewers…, Nudge Unanswered
  Invitations…, Reload, Pause All Automation, Settings…, Journal and Role,
  Quit.
- **Closing the main window does not quit**: Docket keeps running from the
  menu-bar icon, and automatic invitations keep being checked. Quit with ⌘Q
  or from the icon's menu.
- **Dock icon** (Settings → General): *While a window is open* (default),
  *Always*, or *Never*. macOS shows an app's menus only
  while it has a Dock icon, so with *Never* the menu-bar icon is the way to
  Docket's commands.
- **Menus**:
  - Docket: Settings… ⌘,.
  - File: Export the Table as CSV ⇧⌘E, Export Settings…, Import Settings…,
    and, for PDF windows, Open in Preview ⇧⌘P, Show in Finder ⇧⌘R, Save a
    Copy to Downloads ⇧⌘S.
  - View: status filter, papers with any status (or past reviews), collapse
    or expand all papers, Find ⌘F, Reload from PaperPlaza ⌘R, zoom ⌘+ ⌘− ⌘0.
  - Journal: switch journal and role, log out or change site.
  - Reviews: message ⇧⌘M, extend, cancel or read the selected reviews;
    select the overdue reviews or the unanswered invitations; remind or nudge
    them; send pending site notes; site notes settings; edit the templates.
  - Invitations: all schedules, run them now, pause or resume them all,
    pause all automation, open one.
  - Window: Main Window ⌘1.

While Docket waits for the site (sending, reading a long list), a message
with a running time covers the window: nothing else can be clicked, and that
window cannot be closed (nor Docket quit) until it is finished.

### Moving to another computer

File → *Export Settings…* saves one file in Downloads with your logins and
PINs for each site, journal and role choices, message templates and
signature, automatic invitations, preferences, and site notes (with what
was already sent). Passwords are not included: they stay
in the Keychain, and you type each once on the new computer. Keep the file
private.

File → *Import Settings…* on the other computer replaces its settings with
the file's, after copying the current ones to a `backup …` folder in the
settings folder. If Docket stays open on both computers, pause all
automation on one of them (Settings → Automation), or both will send.

## Safety

- Every action is shown in full before anything is sent, and a preview can be
  executed only once. The exceptions are what you turn on to run by itself:
  automatic invitations and site notes (which can ask you first, and can be
  paused or limited to set hours).
- Before acting, Docket checks that the page it opened belongs to the
  intended paper and review, and refuses otherwise.
- Viewer mode blocks every request that could change something, inside
  Docket, before it reaches the site.
- The local server that the windows (or your browser) talk to listens only
  on 127.0.0.1 and requires a random token.
- Reviewer recommendations are computed on your computer; no manuscript data
  is sent to other services.

## Where Docket keeps its data

| What | Where |
|---|---|
| Logins, PINs, journal and role choices, window positions | `~/Library/Application Support/Docket/config.json` |
| Message templates and signature | `~/Library/Application Support/Docket/templates.json` |
| Automatic invitations | `~/Library/Application Support/Docket/schedules.json` |
| Site notes: settings, what was sent, notes waiting for approval | `~/Library/Application Support/Docket/notes.json` |
| Activity log (what Docket sent by itself) | `~/Library/Application Support/Docket/activity.json` |
| Open at login | `~/Library/LaunchAgents/local.docket.Docket.login.plist` |
| Passwords | macOS Keychain, service "Docket" |
| Downloaded PDFs | `~/Library/Caches/Docket/files` (emptied at each start) |

## Command line

```
docket [--trace DIR] [--no-browser]   Start in your web browser
docket app [--background] [--trace DIR]
                                      Start in its own windows (what Docket.app does);
                                      --background: in the menu bar only
docket list [--trace DIR]             Log in from the terminal and print all reviews (read-only)
docket run-schedules [--viewer]       Check the automatic invitations and the site notes
                                      once, with the password saved in the Keychain (for
                                      cron or launchd); follows pause and sending hours
docket parse FILE.html...             Show what Docket reads from saved PaperPlaza pages
docket forget                         Remove the saved passwords from the Keychain
```

`--trace DIR` saves every page received and every form sent to `DIR`, with
the password redacted, for troubleshooting. These files contain confidential
review data: do not share them.

## How it talks to the site

Docket behaves like your browser on the same pages. It submits each page's
own form with the button's command name, echoing the hidden `Fields`, `Hash`,
`Token` and `RldCtl` values. It encodes text as windows-1252, as the site
expects, and waits at least 0.4 s between requests. After the site's
60-minute session timeout it logs in again automatically. Links to other
sites and e-mail addresses open in your default browser and mail app.

Every feature is built from pages saved from the site; nothing is guessed.
Features whose pages have not been seen yet are listed below.

## Limitations

Not supported yet, for lack of saved pages:

- sending the cancellation notice after a cancel;
- PaperPlaza's per-review reminder and thank-you buttons on Review Details
  (Docket uses the site's bulk pages instead);
- editing the site's letters for site notes (its default letters are sent);
- editing the invitation letter, or the "Request a rereview" letter (the
  site's default letter is sent);
- the pages shown after *Send* (success is checked on *Manage Reviews*, by
  the message form closing, or, for site notes, on the site's lists read
  again after sending);
- the decision letter behind *More...* on *Inspect the reviews*, and files
  attached to the other reviews shown there;
- the submission menu's *Files* page (other versions and supplementary
  files); the reviewer's file list has only been seen empty;
- reviewer actions (accept, decline, submit a review);
- conference workspaces, including conference reviewer roles;
- editor pages on ifac-sms.org and controls.papercept.net (only their Access
  pages and the Automatica reviewer workspace have been seen).

## Development

```
src/main.rs        command line
src/app.rs         macOS windows, menus, menu-bar icon (wry, tao, muda, tray-icon, PDFKit)
src/ui.rs          local web server and its API; serves assets/index.html
src/papercept.rs   the PaperCept site: navigation, parsing, actions
src/http.rs        HTTP client, form submission, viewer-mode guard
src/page.rs        HTML page and form model
src/model.rs       papers, reviews, reviewers
src/scheduler.rs   automatic invitations
src/recommend.rs   reviewer recommendations
src/templates.rs   message templates and placeholders
src/notes.rs       site notes: thanks, confirmation requests, reminders
src/activity.rs    log of what Docket sent by itself
src/creds.rs       settings, Keychain, settings export and import
assets/index.html  the whole user interface
scripts/build-app.sh  builds Docket.app
```

**Tests**: `cargo test`. Tests never contact the real sites and never touch
your settings or the Keychain. The replay tests run the full flows against
pages saved from the site, served by a local test server. Those pages contain
confidential review data, and so do the names, numbers and texts the tests
expect to read from them; neither is in the repository. To run them with your
own saved pages, put an `expected.json` next to them, in the format of
[`tests/replay-expected.example.json`](tests/replay-expected.example.json)
(all values there are made up), then:

```sh
DOCKET_CAPTURES=/path/to/saved/pages cargo test -- --include-ignored --test-threads=1
```

Never commit saved pages, `expected.json` or `--trace` output.

**Icons**: `assets/icon/Docket.icns` is made from `assets/icon/docket.svg`
(render each size of an `.iconset` folder from the SVG, then `iconutil -c
icns`). The menu-bar glyph `assets/icon/menubar.rgba` is drawn by `swift
assets/icon/menubar.swift assets/icon/menubar.rgba /tmp/menubar.png`.

## License

[MIT](LICENSE) © 2026 Corentin Briat
