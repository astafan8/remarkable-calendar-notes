//! Calendar source implementations: local `.ics` files, arbitrary HTTPS
//! `.ics` URLs, Google Calendar (OAuth device flow), and iCloud (CalDAV with
//! an app-specific password). All network I/O uses `ureq` with its
//! `rustls`-backed `tls` feature — pure Rust TLS with bundled root
//! certificates, no OpenSSL/system-TLS dependency, which keeps
//! cross-compiling to the reMarkable's armv7 target simple.
//!
//! Every source funnels through [`refresh_source`], which always updates
//! [`crate::model::SourceStatus`] and falls back to the last successful
//! offline cache on any error, so one broken source (bad credentials, no
//! network, malformed ICS) never crashes the app or blanks out data that
//! *was* fetched successfully before.

pub mod cache;
pub mod caldav;
pub mod google;
pub mod https_ics;
pub mod local_ics;

use crate::model::{CalendarSource, Event, SourceKind, SourceStatus};
use crate::recurrence::Window;
use crate::timeutil::UtcOffset;
use chrono::Utc;
use std::fmt;

#[derive(Debug)]
pub enum SourceError {
    Io(std::io::Error),
    Http(String),
    Parse(String),
    Auth(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::Io(e) => write!(f, "I/O error: {e}"),
            SourceError::Http(msg) => write!(f, "network error: {msg}"),
            SourceError::Parse(msg) => write!(f, "parse error: {msg}"),
            SourceError::Auth(msg) => write!(f, "authentication error: {msg}"),
        }
    }
}

impl std::error::Error for SourceError {}

impl From<std::io::Error> for SourceError {
    fn from(e: std::io::Error) -> Self {
        SourceError::Io(e)
    }
}

/// Fetch fresh events for one configured source, expanded over `window` and
/// converted into wall-clock time at `offset`.
/// On success, updates `source.last_status` and writes the offline cache.
/// On failure, updates `source.last_status` with the error and returns the
/// last cached events (if any) so the display never goes blank because of
/// a transient network problem.
pub fn refresh_source(
    source: &mut CalendarSource,
    window: Window,
    offset: UtcOffset,
) -> Vec<Event> {
    let result = fetch_events(source, window, offset);
    match result {
        Ok(events) => {
            source.last_status = SourceStatus::Ok {
                synced_at_utc: Utc::now().naive_utc(),
                event_count: events.len(),
            };
            let _ = cache::save_cache(&source.id, &events);
            events
        }
        Err(e) => {
            source.last_status = SourceStatus::Error {
                synced_at_utc: Utc::now().naive_utc(),
                message: e.to_string(),
            };
            cache::load_cache(&source.id).unwrap_or_default()
        }
    }
}

fn fetch_events(
    source: &CalendarSource,
    window: Window,
    offset: UtcOffset,
) -> Result<Vec<Event>, SourceError> {
    match &source.kind {
        SourceKind::LocalIcs { path } => {
            let text = local_ics::read_ics_file(path)?;
            Ok(events_from_ics_text(&text, &source.id, window, offset))
        }
        SourceKind::HttpsIcs { url } => {
            let text = https_ics::fetch_ics(url)?;
            Ok(events_from_ics_text(&text, &source.id, window, offset))
        }
        SourceKind::IcloudCalDav {
            apple_id,
            app_specific_password,
            calendar_url,
        } => caldav::fetch_icloud_events(
            apple_id,
            app_specific_password,
            calendar_url,
            &source.id,
            window,
            offset,
        ),
        SourceKind::GoogleCalendar {
            client_id,
            client_secret,
            calendar_id,
            refresh_token,
        } => {
            let token = refresh_token.as_ref().ok_or_else(|| {
                SourceError::Auth("Google Calendar source has not completed login".into())
            })?;
            let access_token = google::refresh_access_token(client_id, client_secret, token)?;
            google::fetch_events(&access_token, calendar_id, &source.id, window, offset)
        }
    }
}

pub(crate) fn events_from_ics_text(
    text: &str,
    source_id: &str,
    window: Window,
    offset: UtcOffset,
) -> Vec<Event> {
    let (raw_events, _errors) = crate::ics::parse_calendar(text);
    raw_events
        .iter()
        .flat_map(|raw| crate::recurrence::build_events(raw, source_id, window, offset))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceKind;
    use chrono::NaiveDate;

    #[test]
    #[serial_test::serial]
    fn local_ics_source_missing_file_reports_error_but_does_not_panic() {
        let mut source = CalendarSource {
            id: "s1".into(),
            label: "Missing".into(),
            enabled: true,
            kind: SourceKind::LocalIcs {
                path: "/nonexistent/path/does/not/exist.ics".into(),
            },
            last_status: SourceStatus::NeverSynced,
        };
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(crate::persistence::DATA_DIR_ENV, dir.path());
        let window = Window {
            start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
        };
        let events = refresh_source(&mut source, window, UtcOffset::new(0));
        assert!(events.is_empty());
        assert!(matches!(source.last_status, SourceStatus::Error { .. }));
        std::env::remove_var(crate::persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn local_ics_source_caches_and_falls_back_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(crate::persistence::DATA_DIR_ENV, dir.path());
        let ics_path = dir.path().join("cal.ics");
        std::fs::write(
            &ics_path,
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x1\r\nSUMMARY:Hi\r\nDTSTART:20260105\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        let mut source = CalendarSource {
            id: "s2".into(),
            label: "Local".into(),
            enabled: true,
            kind: SourceKind::LocalIcs {
                path: ics_path.to_string_lossy().to_string(),
            },
            last_status: SourceStatus::NeverSynced,
        };
        let window = Window {
            start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
        };
        let events = refresh_source(&mut source, window, UtcOffset::new(0));
        assert_eq!(events.len(), 1);
        assert!(matches!(source.last_status, SourceStatus::Ok { .. }));

        // Now make the file disappear and refresh again: should fall back
        // to the cached event instead of going blank.
        std::fs::remove_file(&ics_path).unwrap();
        let events2 = refresh_source(&mut source, window, UtcOffset::new(0));
        assert_eq!(events2.len(), 1);
        assert!(matches!(source.last_status, SourceStatus::Error { .. }));
        std::env::remove_var(crate::persistence::DATA_DIR_ENV);
    }
}
