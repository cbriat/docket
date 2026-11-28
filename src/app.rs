//! Docket in its own macOS windows: the page of the browser version, shown in
//! WebKit windows (wry/tao) with a native menu bar and an icon in the menu bar
//! at the top of the screen. The page asks for more windows through
//! `window.ipc.postMessage`; the local server stays the same.
//!
//! The main window reports what it shows ("menus"), so the Journal, Reviews,
//! Invitations and View menus and the menu-bar icon's menu follow it; their
//! items are sent back to that page as `docketEvent("menu", …)`.
//!
//! The Dock icon (Settings → General) is shown while a window is open (by default), always, or
//! never. macOS shows an app's menus only while it has a Dock icon, so with
//! "never" the menu-bar icon is the way to Docket's commands.

use crate::creds;
use anyhow::{Context, Result};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::platform::macos::{ActivationPolicy, EventLoopWindowTargetExtMacOS};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
use objc2_foundation::{NSString, NSURL};
use objc2_pdf_kit::{PDFDocument, PDFView};
use tao::platform::macos::WindowExtMacOS;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use wry::{NewWindowResponse, WebView, WebViewBuilder};

enum UserEvent {
    Ipc(WindowId, String),
    Menu(MenuEvent),
}

/// A message from a page (see `native()` in assets/index.html).
#[derive(Deserialize, Default)]
#[serde(default)]
struct Msg {
    cmd: String,
    /// open: the kind of window ("picker", "message", "pdf", …).
    view: String,
    /// open: extra query parameters for the page, or the file URL for a PDF.
    query: String,
    url: String,
    title: String,
    width: f64,
    height: f64,
    /// pdf: the downloaded file.
    path: String,
    /// broadcast: event name and data passed to the other windows.
    event: String,
    data: Value,
}

/// What the main window shows, for the menus (see syncMenus() in assets/index.html).
#[derive(Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
struct MenuState {
    logged_in: bool,
    viewer: bool,
    /// "editor" or "reviewer" once a workspace is open, else "".
    mode: String,
    journal: String,
    journals: Vec<Choice>,
    roles: Vec<Choice>,
    filters: Vec<Choice>,
    include_all: Choice,
    selected: usize,
    /// Selected reviews that were submitted (and can be read).
    done_selected: usize,
    overdue: usize,
    unanswered: usize,
    schedules: Vec<Choice>,
    active: usize,
    paused: usize,
    /// Site notes are on for some journal of this site.
    notes_on: bool,
    /// Site notes are on, but for no journal.
    #[serde(default)]
    notes_no_journal: bool,
    /// Thank-you notes on the site's list (for agreeing, for the review);
    /// None when not known.
    #[serde(default)]
    unthanked: Option<[usize; 2]>,
    /// Site notes waiting for approval.
    pending: usize,
    /// All automation is paused (Settings → Automation).
    paused_all: bool,
}

/// One entry of a menu the page fills: `on` is shown with a check mark.
#[derive(Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
struct Choice {
    id: String,
    label: String,
    on: bool,
}

/// The menus whose items follow the main window.
struct Menus {
    file_items: Vec<MenuItem>,
    journal: Submenu,
    reviews: Submenu,
    invitations: Submenu,
    view: Submenu,
}

struct Win {
    window: Window,
    view: View,
    kind: String,
    file: Option<PathBuf>,
    zoom: f64,
    /// The page is waiting for PaperPlaza (sending, reading): closing it, or
    /// quitting, could stop that halfway.
    busy: bool,
}

/// What a window shows: Docket's page, or a PDF in PDFKit's viewer (as in
/// Preview; WebKit in these windows showed PDFs as a blank page).
enum View {
    Web(WebView),
    Pdf(Retained<PDFView>),
}

struct Shell {
    /// http://127.0.0.1:port/?t=token
    base: String,
    /// http://127.0.0.1:port
    origin: String,
    proxy: EventLoopProxy<UserEvent>,
    wins: HashMap<WindowId, Win>,
    main: Option<WindowId>,
    focused: Option<WindowId>,
    frames: BTreeMap<String, [f64; 4]>,
    menus: Menus,
    state: MenuState,
    tray: Option<TrayIcon>,
    /// See Config::dock.
    dock: String,
    /// The Dock icon is shown now (None before the first decision).
    regular: Option<bool>,
    /// Started at login in the menu bar only: the main window stays hidden.
    background: bool,
}

const PREVIEW: &str = "preview";
const FINDER: &str = "finder";
const DOWNLOADS: &str = "downloads";
const RELOAD: &str = "reload";
const ZOOM_IN: &str = "zoom-in";
const ZOOM_OUT: &str = "zoom-out";
const ZOOM_RESET: &str = "zoom-reset";
const QUIT: &str = "quit";
const SHOW_MAIN: &str = "show-main";
/// Prefix of the items handled by the main window's page ("p:action:argument").
const PAGE: &str = "p:";
/// Find in the window in front, whichever it is.
const FIND: &str = "find";

/// `background`: started at login, in the menu bar only (the main window
/// stays hidden until opened from the menu-bar icon).
pub fn run(trace: Option<PathBuf>, background: bool) -> Result<()> {
    // If Docket.app moved since "Open at login" was turned on, follow it.
    if let (true, Ok(exe)) = (creds::open_at_login(), std::env::current_exe()) {
        let _ = creds::set_open_at_login(true, &exe);
    }
    let base = crate::ui::start(trace)?;
    let origin = base.split("/?").next().context("bad local address")?.to_string();
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = p.send_event(UserEvent::Menu(e));
    }));
    let (menu, menus) = build_menu()?;
    let cfg = creds::load_config();
    let mut shell = Shell {
        base,
        origin,
        proxy,
        wins: HashMap::new(),
        main: None,
        focused: None,
        frames: cfg.windows,
        menus,
        state: MenuState::default(),
        tray: None,
        dock: cfg.dock,
        regular: None,
        background,
    };
    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                menu.init_for_nsapp();
                shell.refresh_menus();
                // The menu-bar icon can only be made once the event loop runs.
                match shell.make_tray() {
                    Ok(t) => shell.tray = Some(t),
                    Err(e) => eprintln!("cannot show the menu-bar icon: {e:#}"),
                }
                let url = shell.base.clone();
                match shell.open(target, "main", &url, "Docket", 1280.0, 860.0) {
                    Ok(id) => shell.main = Some(id),
                    Err(e) => {
                        eprintln!("error: cannot open the window: {e:#}");
                        std::process::exit(1);
                    }
                }
                if shell.background {
                    shell.update_dock(target);
                } else {
                    shell.show_main(target);
                }
            }
            // The Dock icon was clicked.
            Event::Reopen { .. } => shell.show_main(target),
            Event::WindowEvent { window_id, event, .. } => match event {
                WindowEvent::CloseRequested => {
                    if !shell.refuse_close(Some(window_id)) {
                        shell.close(window_id);
                        shell.update_dock(target);
                    }
                }
                WindowEvent::Focused(true) => {
                    shell.focused = Some(window_id);
                    let pdf = shell.wins.get(&window_id).is_some_and(|w| w.file.is_some());
                    for item in &shell.menus.file_items {
                        item.set_enabled(pdf);
                    }
                }
                WindowEvent::Moved(_) | WindowEvent::Resized(_) => shell.remember(window_id),
                _ => {}
            },
            Event::UserEvent(UserEvent::Ipc(from, body)) => {
                if let Err(e) = shell.message(target, from, &body) {
                    eprintln!("window request failed: {e:#}");
                }
                shell.update_dock(target);
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if shell.menu(target, e.id().as_ref()) {
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}

type Items = Vec<Box<dyn IsMenuItem>>;

fn item(id: impl AsRef<str>, text: impl AsRef<str>, enabled: bool) -> Box<dyn IsMenuItem> {
    Box::new(MenuItem::with_id(id.as_ref(), text, enabled, None))
}
fn keyed(id: &str, text: &str, enabled: bool, m: Modifiers, c: Code) -> Box<dyn IsMenuItem> {
    Box::new(MenuItem::with_id(id, text, enabled, Some(Accelerator::new(m, c))))
}
fn check(id: impl AsRef<str>, text: impl AsRef<str>, enabled: bool, on: bool) -> Box<dyn IsMenuItem> {
    Box::new(CheckMenuItem::with_id(id.as_ref(), text, enabled, on, None))
}
fn sep() -> Box<dyn IsMenuItem> {
    Box::new(PredefinedMenuItem::separator())
}
fn refs(items: &Items) -> Vec<&dyn IsMenuItem> {
    items.iter().map(|b| b.as_ref()).collect()
}
fn refill(sub: &Submenu, items: Items) {
    while sub.remove_at(0).is_some() {}
    for it in &items {
        let _ = sub.append(it.as_ref());
    }
}
/// "3 reviews" style counts in menu items.
fn count(n: usize) -> String {
    if n > 0 { format!(" ({n})") } else { String::new() }
}

fn journal_items(st: &MenuState) -> Items {
    if !st.logged_in {
        return vec![item(format!("{PAGE}show"), "Log In…", true)];
    }
    let mut v: Items = st.journals.iter().map(|j| check(format!("{PAGE}journal:{}", j.id), &j.label, true, j.on)).collect();
    if !st.roles.is_empty() {
        v.push(sep());
        v.extend(st.roles.iter().map(|r| check(format!("{PAGE}role:{}", r.id), &r.label, true, r.on)));
    }
    v.push(sep());
    v.push(item(format!("{PAGE}logout"), "Log Out or Change Site", true));
    v
}

fn review_items(st: &MenuState) -> Items {
    let editor = st.mode == "editor";
    let act = editor && !st.viewer;
    let n = st.selected;
    let shift = Modifiers::META | Modifiers::SHIFT;
    let mut v: Items = Vec::new();
    if !editor {
        v.push(item("none", "Only in the Associate Editor Role", false));
        v.push(sep());
    } else if st.viewer {
        v.push(item("none", "Viewer Mode: Nothing Can Be Sent", false));
        v.push(sep());
    }
    v.extend([
        keyed(&format!("{PAGE}message"), &format!("Message Selected Reviewers{}…", count(n)), act && n > 0, shift, Code::KeyM),
        item(format!("{PAGE}extend"), format!("Extend Selected Deadlines{}…", count(n)), act && n > 0),
        item(format!("{PAGE}cancel"), format!("Cancel Selected Requests{}…", count(n)), act && n > 0),
        item(format!("{PAGE}read"), format!("Read Selected Reviews{}", count(st.done_selected)), editor && st.done_selected > 0),
        item(format!("{PAGE}clear"), "Clear Selection", editor && n > 0),
        sep(),
        item(format!("{PAGE}select-overdue"), format!("Select Overdue Reviews{}", count(st.overdue)), editor && st.overdue > 0),
        item(format!("{PAGE}select-unanswered"), format!("Select Unanswered Invitations{}", count(st.unanswered)), editor && st.unanswered > 0),
        sep(),
        item(format!("{PAGE}remind-overdue"), format!("Remind Overdue Reviewers{}…", count(st.overdue)), act && st.overdue > 0),
        item(format!("{PAGE}nudge-unanswered"), format!("Nudge Unanswered Invitations{}…", count(st.unanswered)), act && st.unanswered > 0),
        sep(),
        thanks_item(st, 0),
        thanks_item(st, 1),
        item(format!("{PAGE}site:remind"), "Send PaperPlaza Reminders…", act),
        item(format!("{PAGE}site:confirm"), "Send PaperPlaza Confirmation Requests…", act),
        item(format!("{PAGE}notes-pending"), format!("Send Pending Site Notes{}…", count(st.pending)), st.pending > 0),
        check(format!("{PAGE}settings:automation"), "Site Notes (Thanks, Confirmations, Reminders)…", true, st.notes_on),
        item(format!("{PAGE}templates"), "Edit Message Templates…", st.logged_in),
    ]);
    v
}

/// "Send Thank-You Notes for Accepted (0) or Submitted (1) Reviews…", with
/// the number on the site's list, greyed out when there is none.
fn thanks_item(st: &MenuState, which: usize) -> Box<dyn IsMenuItem> {
    let (kind, what) = if which == 0 { ("accept", "Accepted") } else { ("review", "Submitted") };
    let n = st.unthanked.map(|u| u[which]);
    let act = st.mode == "editor" && !st.viewer;
    item(format!("{PAGE}site:{kind}"), format!("Send Thank-You Notes for {what} Reviews{}…", n.map(count).unwrap_or_default()), act && n != Some(0))
}

fn invitation_items(st: &MenuState) -> Items {
    let mut v: Items = vec![
        item(format!("{PAGE}sched-list"), "Show All Automatic Invitations…", st.logged_in),
        item(format!("{PAGE}sched-run"), format!("Run Automatic Invitations Now{}…", count(st.active)), st.logged_in && st.active > 0),
        item(format!("{PAGE}sched-pause"), format!("Pause All{}", count(st.active)), st.active > 0),
        item(format!("{PAGE}sched-resume"), format!("Resume All{}", count(st.paused)), st.paused > 0),
        check(format!("{PAGE}pause-all"), "Pause All Automation", true, st.paused_all),
        sep(),
    ];
    if st.schedules.is_empty() {
        v.push(item("none", "No Automatic Invitations Yet", false));
    }
    v.extend(st.schedules.iter().map(|s| item(format!("{PAGE}sched:{}", s.id), &s.label, st.logged_in)));
    v
}

fn view_items(st: &MenuState) -> Items {
    let cmd = Modifiers::META;
    let mut v: Items = st.filters.iter().map(|f| check(format!("{PAGE}filter:{}", f.id), &f.label, true, f.on)).collect();
    if !st.mode.is_empty() {
        v.push(sep());
        v.push(check(format!("{PAGE}include-all"), &st.include_all.label, true, st.include_all.on));
    }
    if st.mode == "editor" {
        v.push(item(format!("{PAGE}fold"), "Collapse All Papers", true));
        v.push(item(format!("{PAGE}unfold"), "Expand All Papers", true));
    }
    v.extend([
        keyed(FIND, "Find…", true, cmd, Code::KeyF),
        sep(),
        keyed(RELOAD, "Reload from PaperPlaza", st.mode != "", cmd, Code::KeyR),
        sep(),
        keyed(ZOOM_RESET, "Actual Size", true, cmd, Code::Digit0),
        keyed(ZOOM_IN, "Zoom In", true, cmd, Code::Equal),
        keyed(ZOOM_OUT, "Zoom Out", true, cmd, Code::Minus),
        sep(),
        Box::new(PredefinedMenuItem::fullscreen(None)) as Box<dyn IsMenuItem>,
    ]);
    v
}

fn build_menu() -> Result<(Menu, Menus)> {
    let cmd = |m: Modifiers, c: Code| Some(Accelerator::new(m, c));
    let shift = Modifiers::META | Modifiers::SHIFT;
    let about = AboutMetadata { name: Some("Docket".into()), version: Some(env!("CARGO_PKG_VERSION").into()), ..Default::default() };
    let app = Submenu::with_items(
        "Docket",
        true,
        &[
            &PredefinedMenuItem::about(Some("About Docket"), Some(about)),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(format!("{PAGE}settings"), "Settings…", true, cmd(Modifiers::META, Code::Comma)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Hide Docket")),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(QUIT, "Quit Docket", true, cmd(Modifiers::META, Code::KeyQ)),
        ],
    )?;
    let file_items = vec![
        MenuItem::with_id(PREVIEW, "Open in Preview", false, cmd(shift, Code::KeyP)),
        MenuItem::with_id(FINDER, "Show in Finder", false, cmd(shift, Code::KeyR)),
        MenuItem::with_id(DOWNLOADS, "Save a Copy to Downloads", false, cmd(shift, Code::KeyS)),
    ];
    let file = Submenu::with_items(
        "File",
        true,
        &[
            &MenuItem::with_id(format!("{PAGE}csv"), "Export the Table as CSV", true, cmd(shift, Code::KeyE)),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(format!("{PAGE}settings-export"), "Export Settings…", true, None),
            &MenuItem::with_id(format!("{PAGE}settings-import"), "Import Settings…", true, None),
            &PredefinedMenuItem::separator(),
            &file_items[0],
            &file_items[1],
            &file_items[2],
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::close_window(Some("Close Window")),
        ],
    )?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )?;
    // Filled by refresh_menus().
    let view = Submenu::new("View", true);
    let journal = Submenu::new("Journal", true);
    let reviews = Submenu::new("Reviews", true);
    let invitations = Submenu::new("Invitations", true);
    let window = Submenu::with_items(
        "Window",
        true,
        &[
            &MenuItem::with_id(SHOW_MAIN, "Main Window", true, cmd(Modifiers::META, Code::Digit1)),
            &MenuItem::with_id(format!("{PAGE}calendar"), "Calendar", true, cmd(shift, Code::KeyK)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::maximize(Some("Zoom")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::bring_all_to_front(None),
        ],
    )?;
    window.set_as_windows_menu_for_nsapp();
    let menu = Menu::with_items(&[&app, &file, &edit, &view, &journal, &reviews, &invitations, &window])?;
    Ok((menu, Menus { file_items, journal, reviews, invitations, view }))
}

impl Shell {
    /// A window of `kind`, where the last one of that kind was.
    fn new_window(&self, target: &EventLoopWindowTarget<UserEvent>, kind: &str, title: &str, width: f64, height: f64) -> Result<Window> {
        let mut b = WindowBuilder::new().with_title(title).with_inner_size(LogicalSize::new(width, height)).with_min_inner_size(LogicalSize::new(480.0, 360.0));
        if let Some([x, y, w, h]) = self.frames.get(kind).copied() {
            // Windows of the same kind open a little below and right of each other.
            let n = self.wins.values().filter(|w| w.kind == kind).count() as f64 * 26.0;
            b = b.with_inner_size(LogicalSize::new(w, h)).with_position(LogicalPosition::new(x + n, y + n));
        }
        Ok(b.build(target)?)
    }

    /// Open a window of `kind` showing Docket's page at `url`.
    fn open(&mut self, target: &EventLoopWindowTarget<UserEvent>, kind: &str, url: &str, title: &str, width: f64, height: f64) -> Result<WindowId> {
        let window = self.new_window(target, kind, title, width, height)?;
        let id = window.id();
        let proxy = self.proxy.clone();
        let origin = self.origin.clone();
        let origin2 = self.origin.clone();
        let webview = WebViewBuilder::new()
            .with_url(url)
            .with_devtools(cfg!(debug_assertions))
            .with_ipc_handler(move |rq: wry::http::Request<String>| {
                let _ = proxy.send_event(UserEvent::Ipc(id, rq.body().clone()));
            })
            // Links to the journals, mailto: and the like go to the default apps.
            .with_navigation_handler(move |u: String| {
                let inside = u.starts_with(&format!("{origin}/")) || u.starts_with("about:");
                if !inside {
                    open_external(&u);
                }
                inside
            })
            .with_new_window_req_handler(move |u: String, _| {
                if !u.starts_with(&format!("{origin2}/")) {
                    open_external(&u);
                }
                NewWindowResponse::Deny
            })
            .with_download_started_handler(|_, _| false)
            .build(&window)?;
        self.wins.insert(id, Win { window, view: View::Web(webview), kind: kind.to_string(), file: None, zoom: 1.0, busy: false });
        Ok(id)
    }

    /// Open a PDF in PDFKit's viewer.
    fn open_pdf(&mut self, target: &EventLoopWindowTarget<UserEvent>, path: PathBuf, title: &str) -> Result<WindowId> {
        let mtm = MainThreadMarker::new().context("windows can only be opened on the main thread")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let doc = unsafe { PDFDocument::initWithURL(PDFDocument::alloc(), &url) }.with_context(|| format!("{} is not a PDF that can be shown", path.display()))?;
        let window = self.new_window(target, "pdf", title, 900.0, 1100.0)?;
        let id = window.id();
        // SAFETY: tao's content view is a live NSView owned by `window`.
        let content: &NSView = unsafe { &*(window.ns_view() as *const NSView) };
        let pdf = unsafe { PDFView::initWithFrame(PDFView::alloc(mtm), content.bounds()) };
        pdf.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        unsafe {
            pdf.setAutoScales(true);
            pdf.setDocument(Some(&doc));
        }
        content.addSubview(&pdf);
        self.wins.insert(id, Win { window, view: View::Pdf(pdf), kind: "pdf".into(), file: Some(path), zoom: 1.0, busy: false });
        Ok(id)
    }

    /// Close a window. The main one is only hidden: Docket keeps running from
    /// the menu-bar icon (automatic invitations go on), and it keeps its page.
    fn close(&mut self, id: WindowId) {
        self.remember(id);
        if self.main == Some(id) {
            if let Some(w) = self.wins.get(&id) {
                w.window.set_visible(false);
            }
            self.save_frames();
        } else {
            self.wins.remove(&id);
        }
    }

    /// Show the main window in front.
    fn show_main(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        if let Some(w) = self.main.and_then(|m| self.wins.get(&m)) {
            w.window.set_visible(true);
        }
        self.update_dock(target);
        if let Some(w) = self.main.and_then(|m| self.wins.get(&m)) {
            w.window.set_focus();
        }
    }

    /// Show or hide the Dock icon (and with it the app's menus) per Config::dock.
    fn update_dock(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        let open = self.wins.values().any(|w| w.window.is_visible());
        let regular = match self.dock.as_str() {
            "always" => true,
            "never" => false,
            _ => open,
        };
        if self.regular != Some(regular) {
            self.regular = Some(regular);
            target.set_activation_policy_at_runtime(if regular { ActivationPolicy::Regular } else { ActivationPolicy::Accessory });
            if let (true, Some(w)) = (regular, self.focused.and_then(|f| self.wins.get(&f)).filter(|w| w.window.is_visible())) {
                w.window.set_focus();
            }
        }
    }

    fn make_tray(&self) -> Result<TrayIcon> {
        // A black glyph that macOS tints for the menu bar (assets/icon/menubar.swift).
        let icon = Icon::from_rgba(include_bytes!("../assets/icon/menubar.rgba").to_vec(), 44, 44)?;
        Ok(TrayIconBuilder::new().with_icon_templated(icon).with_tooltip("Docket").with_menu(Box::new(self.tray_menu()?)).build()?)
    }

    /// The menu of the menu-bar icon: a summary and the everyday commands.
    fn tray_menu(&self) -> Result<Menu> {
        let st = &self.state;
        let editor = st.mode == "editor";
        let act = editor && !st.viewer;
        let status = if !st.logged_in {
            "Not logged in".to_string()
        } else if editor {
            format!("{}: {} overdue · {} unanswered", st.journal, st.overdue, st.unanswered)
        } else if st.mode == "reviewer" {
            format!("{}: reviewer role", st.journal)
        } else {
            "No workspace open".to_string()
        };
        let auto = if st.paused_all {
            "All automation is paused".to_string()
        } else {
            format!("Automatic invitations: {} active{}", st.active, if st.paused > 0 { format!(", {} paused", st.paused) } else { String::new() })
        };
        let notes = format!("Site notes: {}{}", if st.notes_on { "on" } else if st.notes_no_journal { "on, but for no journal (Settings)" } else { "off" }, if st.pending > 0 { format!(", {} waiting for approval", st.pending) } else { String::new() });
        let journal = journal_items(st);
        let journal = Submenu::with_items("Journal and Role", true, &refs(&journal))?;
        let items: Items = vec![
            item("none", status, false),
            item("none", auto, false),
            item("none", notes, false),
            sep(),
            item(SHOW_MAIN, "Open Docket", true),
            item(format!("{PAGE}calendar"), "Calendar…", true),
            sep(),
            thanks_item(st, 0),
            thanks_item(st, 1),
            item(format!("{PAGE}notes-pending"), format!("Send Pending Site Notes{}…", count(st.pending)), st.pending > 0),
            item(format!("{PAGE}sched-run"), format!("Run Automatic Invitations Now{}…", count(st.active)), st.logged_in && st.active > 0),
            item(format!("{PAGE}remind-overdue"), format!("Remind Overdue Reviewers{}…", count(st.overdue)), act && st.overdue > 0),
            item(format!("{PAGE}nudge-unanswered"), format!("Nudge Unanswered Invitations{}…", count(st.unanswered)), act && st.unanswered > 0),
            item(RELOAD, "Reload from PaperPlaza", st.mode != ""),
            sep(),
            check(format!("{PAGE}pause-all"), "Pause All Automation", true, st.paused_all),
            item(format!("{PAGE}settings"), "Settings…", true),
        ];
        let menu = Menu::with_items(&refs(&items))?;
        menu.append_items(&[&journal, &PredefinedMenuItem::separator(), &MenuItem::with_id(QUIT, "Quit Docket", true, None)])?;
        Ok(menu)
    }

    /// Fill the menus that follow the main window, and the menu-bar icon's.
    fn refresh_menus(&self) {
        let st = &self.state;
        refill(&self.menus.journal, journal_items(st));
        refill(&self.menus.reviews, review_items(st));
        refill(&self.menus.invitations, invitation_items(st));
        refill(&self.menus.view, view_items(st));
        if let Some(t) = &self.tray {
            match self.tray_menu() {
                Ok(m) => t.set_menu(Some(Box::new(m))),
                Err(e) => eprintln!("cannot update the menu-bar icon: {e:#}"),
            }
            let tip = if st.mode == "editor" { format!("Docket — {}: {} overdue, {} unanswered", st.journal, st.overdue, st.unanswered) } else { "Docket".into() };
            let _ = t.set_tooltip(Some(tip));
        }
    }

    /// Run a menu command in the main window's page.
    fn page_command(&mut self, target: &EventLoopWindowTarget<UserEvent>, command: &str) {
        let (action, arg) = command.split_once(':').unwrap_or((command, ""));
        // These open their own window, or need none.
        if !["settings", "pause-all", "site", "calendar"].contains(&action) {
            self.show_main(target);
        }
        self.send_menu(self.main, action, arg);
    }

    fn send_menu(&self, to: Option<WindowId>, action: &str, arg: &str) {
        if let Some(View::Web(web)) = to.and_then(|id| self.wins.get(&id)).map(|w| &w.view) {
            let _ = web.evaluate_script(&format!("window.docketEvent && window.docketEvent('menu', {})", json!({"action": action, "arg": arg})));
        }
    }

    /// True (and the busy windows are told why) when closing `id`, or quitting
    /// for None, would stop a running request. The main window is only hidden.
    fn refuse_close(&self, id: Option<WindowId>) -> bool {
        if id.is_some() && id == self.main {
            return false;
        }
        let busy: Vec<&Win> = self.wins.iter().filter(|(w, x)| x.busy && (id.is_none() || Some(**w) == id)).map(|(_, x)| x).collect();
        for w in &busy {
            if let View::Web(web) = &w.view {
                let _ = web.evaluate_script("window.docketEvent && window.docketEvent('busy-close', null)");
            }
            w.window.set_focus();
        }
        !busy.is_empty()
    }

    fn remember(&mut self, id: WindowId) {
        let Some(w) = self.wins.get(&id) else { return };
        let scale = w.window.scale_factor();
        let (Ok(pos), size) = (w.window.outer_position(), w.window.inner_size()) else { return };
        let (pos, size): (LogicalPosition<f64>, LogicalSize<f64>) = (pos.to_logical(scale), size.to_logical(scale));
        if size.width >= 100.0 && size.height >= 100.0 {
            self.frames.insert(w.kind.clone(), [pos.x, pos.y, size.width, size.height]);
        }
    }

    fn save_frames(&self) {
        // Read again: the server thread saves the other settings in the same file.
        let mut cfg = creds::load_config();
        cfg.windows = self.frames.clone();
        let _ = creds::save_config(&cfg);
    }

    fn message(&mut self, target: &EventLoopWindowTarget<UserEvent>, from: WindowId, body: &str) -> Result<()> {
        let m: Msg = serde_json::from_str(body)?;
        match m.cmd.as_str() {
            "open" => {
                let kind = if m.view.chars().all(|c| c.is_ascii_alphanumeric()) && !m.view.is_empty() { m.view.clone() } else { "other".into() };
                let url = format!("{}&view={kind}{}{}", self.base, if m.query.is_empty() { "" } else { "&" }, m.query);
                self.open(target, &kind, &url, &m.title, m.width.max(480.0), m.height.max(360.0))?;
            }
            "pdf" => {
                // Only files Docket downloaded itself.
                let path = PathBuf::from(&m.path);
                if !path.starts_with(creds::files_dir()) {
                    anyhow::bail!("not a Docket file: {}", m.path);
                }
                self.open_pdf(target, path, &m.title)?;
            }
            "close" => {
                if let Some(w) = self.wins.get(&from) {
                    // As if the window's close button was clicked.
                    w.window.set_visible(false);
                }
                self.close(from);
            }
            "busy" => {
                if let Some(w) = self.wins.get_mut(&from) {
                    w.busy = m.data.as_bool().unwrap_or(false);
                }
            }
            "menus" => {
                // Only the main window's page describes the menus.
                if Some(from) == self.main {
                    let st: MenuState = serde_json::from_value(m.data)?;
                    if st != self.state {
                        self.state = st;
                        self.refresh_menus();
                    }
                }
            }
            "command" => {
                // A menu command for the main window, asked by another window.
                if let Some(c) = m.data.as_str() {
                    self.page_command(target, c);
                }
            }
            "dock" => {
                // Chosen in the settings (which saved it).
                let mode = m.data.as_str().unwrap_or_default();
                if ["", "always", "never"].contains(&mode) {
                    self.dock = mode.to_string();
                    self.update_dock(target);
                }
            }
            "title" => {
                if let Some(w) = self.wins.get(&from) {
                    w.window.set_title(&m.title);
                }
            }
            "broadcast" => {
                let js = format!("window.docketEvent && window.docketEvent({}, {})", Value::String(m.event.clone()), m.data);
                for (id, w) in &self.wins {
                    if let (true, View::Web(web)) = (*id != from, &w.view) {
                        let _ = web.evaluate_script(&js);
                    }
                }
            }
            other => anyhow::bail!("unknown window request '{other}'"),
        }
        Ok(())
    }

    /// A menu item was chosen; true to quit.
    fn menu(&mut self, target: &EventLoopWindowTarget<UserEvent>, id: &str) -> bool {
        if let Some(command) = id.strip_prefix(PAGE) {
            self.page_command(target, command);
            return false;
        }
        let front = self.focused.and_then(|f| self.wins.get_mut(&f));
        match id {
            SHOW_MAIN => self.show_main(target),
            FIND => self.send_menu(self.focused.or(self.main), "find", ""),
            QUIT => {
                if self.refuse_close(None) {
                    return false;
                }
                if let Some(f) = self.focused {
                    self.remember(f);
                }
                self.save_frames();
                return true;
            }
            PREVIEW | FINDER | DOWNLOADS => {
                if let Some(path) = front.and_then(|w| w.file.clone()) {
                    let _ = crate::ui::reveal(&path, id);
                }
            }
            RELOAD => {
                self.show_main(target);
                if let Some(View::Web(web)) = self.main.and_then(|m| self.wins.get(&m)).map(|w| &w.view) {
                    let _ = web.evaluate_script("window.docketEvent && window.docketEvent('reload', null)");
                }
            }
            ZOOM_IN | ZOOM_OUT | ZOOM_RESET => {
                match front.map(|w| (&w.view, &mut w.zoom)) {
                    Some((View::Pdf(pdf), _)) => unsafe {
                        match id {
                            ZOOM_IN => pdf.zoomIn(None),
                            ZOOM_OUT => pdf.zoomOut(None),
                            _ => pdf.setAutoScales(true),
                        }
                    },
                    Some((View::Web(web), zoom)) => {
                        *zoom = match id {
                            ZOOM_IN => (*zoom * 1.1).min(3.0),
                            ZOOM_OUT => (*zoom / 1.1).max(0.5),
                            _ => 1.0,
                        };
                        let _ = web.zoom(*zoom);
                    }
                    None => {}
                }
            }
            _ => {}
        }
        false
    }
}

fn open_external(url: &str) {
    let lower = url.to_ascii_lowercase();
    if ["https://", "http://", "mailto:"].iter().any(|s| lower.starts_with(s)) {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
}
