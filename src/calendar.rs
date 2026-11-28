//! The user's own calendar events (those of the Calendar app, every account
//! set up there), read on this Mac with EventKit, only to show them next to
//! the deadlines. Nothing is written to the calendar and nothing leaves the Mac.

use chrono::NaiveDate;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Event {
    pub title: String,
    /// Local time, "YYYY-MM-DDTHH:MM".
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub calendar: String,
    /// "#rrggbb", the calendar's colour.
    pub color: String,
    pub location: String,
}

/// "granted", "denied", "ask" (not asked yet), "asking" (macOS is asking),
/// or "unavailable" (not running as Docket.app, or in tests).
pub fn status() -> &'static str {
    imp::status()
}

/// Ask macOS for access (the answer comes later: see `status`).
pub fn request() {
    imp::request()
}

/// The events from `from` to `to` (both included), at most about four months.
pub fn events(from: NaiveDate, to: NaiveDate) -> anyhow::Result<Vec<Event>> {
    anyhow::ensure!(to >= from && (to - from).num_days() <= 130, "choose at most four months");
    if status() != "granted" {
        return Ok(Vec::new());
    }
    imp::events(from, to)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn status() -> &'static str {
        "unavailable"
    }
    pub fn request() {}
    pub fn events(_: chrono::NaiveDate, _: chrono::NaiveDate) -> anyhow::Result<Vec<super::Event>> {
        Ok(Vec::new())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Event;
    use block2::RcBlock;
    use chrono::{DateTime, Local, NaiveDate, TimeZone};
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObjectProtocol};
    use objc2::{AnyThread, sel};
    use objc2_app_kit::NSColorSpace;
    use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEventStore};
    use objc2_foundation::{NSBundle, NSDate, NSError, NSString};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Store(Retained<EKEventStore>);
    // The store is only used under the mutex.
    unsafe impl Send for Store {}
    static STORE: Mutex<Option<Store>> = Mutex::new(None);
    static ASKING: AtomicBool = AtomicBool::new(false);

    /// macOS stops an app that asks without saying why in its Info.plist:
    /// only Docket.app has that text.
    fn can_ask() -> bool {
        if cfg!(test) {
            return false;
        }
        let b = NSBundle::mainBundle();
        b.objectForInfoDictionaryKey(&NSString::from_str("NSCalendarsFullAccessUsageDescription")).is_some()
            || b.objectForInfoDictionaryKey(&NSString::from_str("NSCalendarsUsageDescription")).is_some()
    }

    pub fn status() -> &'static str {
        if !can_ask() {
            return "unavailable";
        }
        let s = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        match s {
            EKAuthorizationStatus::FullAccess => "granted",
            EKAuthorizationStatus::NotDetermined if ASKING.load(Ordering::SeqCst) => "asking",
            EKAuthorizationStatus::NotDetermined => "ask",
            _ => "denied",
        }
    }

    fn store() -> Retained<EKEventStore> {
        let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(|| Store(unsafe { EKEventStore::init(EKEventStore::alloc()) })).0.clone()
    }

    pub fn request() {
        if status() != "ask" {
            return;
        }
        ASKING.store(true, Ordering::SeqCst);
        let done = RcBlock::new(|_granted: Bool, _err: *mut NSError| {
            // A store made before access was given shows no calendars.
            *STORE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            ASKING.store(false, Ordering::SeqCst);
        });
        let st = store();
        let ptr = &*done as *const _ as *mut _;
        unsafe {
            if st.respondsToSelector(sel!(requestFullAccessToEventsWithCompletion:)) {
                st.requestFullAccessToEventsWithCompletion(ptr);
            } else {
                #[allow(deprecated)]
                st.requestAccessToEntityType_completion(EKEntityType::Event, ptr);
            }
        }
    }

    fn local(t: &NSDate) -> DateTime<Local> {
        let secs = t.timeIntervalSince1970();
        DateTime::from_timestamp(secs.floor() as i64, 0).unwrap_or_default().with_timezone(&Local)
    }

    fn midnight(d: NaiveDate) -> Retained<NSDate> {
        let t = Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()).earliest().map(|t| t.timestamp()).unwrap_or_default();
        NSDate::dateWithTimeIntervalSince1970(t as f64)
    }

    pub fn events(from: NaiveDate, to: NaiveDate) -> anyhow::Result<Vec<Event>> {
        let st = store();
        let end = to.succ_opt().unwrap_or(to);
        let mut out = Vec::new();
        unsafe {
            let pred = st.predicateForEventsWithStartDate_endDate_calendars(&midnight(from), &midnight(end), None);
            for e in st.eventsMatchingPredicate(&pred).iter() {
                let (s, t) = (local(&e.startDate()), local(&e.endDate()));
                let cal = e.calendar();
                let color = cal
                    .as_ref()
                    .and_then(|c| c.color().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()))
                    .map(|c| {
                        let x = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                        format!("#{:02x}{:02x}{:02x}", x(c.redComponent()), x(c.greenComponent()), x(c.blueComponent()))
                    })
                    .unwrap_or_else(|| "#888888".into());
                out.push(Event {
                    title: e.title().to_string(),
                    start: s.format("%Y-%m-%dT%H:%M").to_string(),
                    end: t.format("%Y-%m-%dT%H:%M").to_string(),
                    all_day: e.isAllDay(),
                    calendar: cal.map(|c| c.title().to_string()).unwrap_or_default(),
                    color,
                    location: e.location().map(|l| l.to_string()).unwrap_or_default(),
                });
            }
        }
        out.sort_by(|a, b| (!a.all_day, &a.start, &a.title).cmp(&(!b.all_day, &b.start, &b.title)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn never_asks_in_tests() {
        assert_eq!(super::status(), "unavailable");
        super::request();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert!(super::events(d, d).unwrap().is_empty());
        assert!(super::events(d, d + chrono::Duration::days(200)).is_err());
    }
}
