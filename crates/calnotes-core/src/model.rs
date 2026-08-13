//! Core domain model: events, calendar sources, and app-wide configuration.

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

/// Either an all-day event (a plain date, no time-of-day) or a timed event
/// (local wall-clock start/end, interpreted through the configured fixed
/// UTC offset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EventTime {
    AllDay {
        start: NaiveDate,
        /// Exclusive end date, per RFC 5545 DTEND semantics for DATE values.
        end_exclusive: NaiveDate,
    },
    Timed {
        start: NaiveDateTime,
        end: NaiveDateTime,
    },
}

impl EventTime {
    pub fn start_date(&self) -> NaiveDate {
        match self {
            EventTime::AllDay { start, .. } => *start,
            EventTime::Timed { start, .. } => start.date(),
        }
    }

    pub fn end_date_inclusive(&self) -> NaiveDate {
        match self {
            EventTime::AllDay { end_exclusive, .. } => {
                end_exclusive.pred_opt().unwrap_or(*end_exclusive)
            }
            EventTime::Timed { end, .. } => end.date(),
        }
    }

    pub fn is_all_day(&self) -> bool {
        matches!(self, EventTime::AllDay { .. })
    }

    /// Every calendar date (inclusive) this event's span touches.
    pub fn dates(&self) -> Vec<NaiveDate> {
        let mut out = Vec::new();
        let mut d = self.start_date();
        let last = self.end_date_inclusive();
        while d <= last {
            out.push(d);
            d = d.succ_opt().unwrap_or(d);
            if out.len() > 3660 {
                break; // safety valve against malformed spans
            }
        }
        out
    }
}

/// A single (possibly recurrence-expanded) calendar event ready for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Stable identity: `{ics UID}#{recurrence-id}` for expanded instances.
    pub id: String,
    pub source_id: String,
    pub summary: String,
    pub location: Option<String>,
    pub time: EventTime,
}

/// Where an app's read-only events come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SourceKind {
    /// A local `.ics` file path on the device (or, for desktop dev/testing,
    /// any accessible filesystem path).
    LocalIcs { path: String },
    /// An arbitrary HTTPS URL serving an `.ics` document.
    HttpsIcs { url: String },
    /// Google Calendar via OAuth 2.0 device authorization flow.
    GoogleCalendar {
        client_id: String,
        client_secret: String,
        calendar_id: String,
        /// Persisted after a successful device-flow login. Never logged.
        refresh_token: Option<String>,
    },
    /// iCloud via CalDAV, authenticated with an app-specific password.
    IcloudCalDav {
        apple_id: String,
        /// App-specific password (`xxxx-xxxx-xxxx-xxxx`). Persisted in
        /// plaintext on-disk; see docs/SECURITY.md for the accepted
        /// limitation on reMarkable's storage model.
        app_specific_password: String,
        calendar_url: String,
    },
}

impl SourceKind {
    pub fn type_label(&self) -> &'static str {
        match self {
            SourceKind::LocalIcs { .. } => "Local .ics file",
            SourceKind::HttpsIcs { .. } => "HTTPS .ics URL",
            SourceKind::GoogleCalendar { .. } => "Google Calendar",
            SourceKind::IcloudCalDav { .. } => "iCloud CalDAV",
        }
    }
}

/// A configured calendar source and its runtime status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSource {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub kind: SourceKind,
    #[serde(default)]
    pub last_status: SourceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state")]
pub enum SourceStatus {
    #[default]
    NeverSynced,
    Ok {
        synced_at_utc: NaiveDateTime,
        event_count: usize,
    },
    Error {
        synced_at_utc: NaiveDateTime,
        message: String,
    },
}

/// Which of the display modes is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    Day,
    Week,
    WorkWeek,
    TwoWeeks,
    Month,
    TwoMonths,
}

impl ViewMode {
    pub const ALL: [ViewMode; 6] = [
        ViewMode::Day,
        ViewMode::Week,
        ViewMode::WorkWeek,
        ViewMode::TwoWeeks,
        ViewMode::Month,
        ViewMode::TwoMonths,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            ViewMode::Day => "Day",
            ViewMode::Week => "Week",
            ViewMode::WorkWeek => "Work Week",
            ViewMode::TwoWeeks => "Two Weeks",
            ViewMode::Month => "Month",
            ViewMode::TwoMonths => "Two Months",
        }
    }
}

/// Whole application configuration, persisted as a single JSON document.
/// There is no required hand-written config file: every field here is
/// editable from the in-app settings screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub utc_offset_minutes: i32,
    #[serde(default = "default_view_mode")]
    pub view_mode: ViewMode,
    #[serde(default)]
    pub sources: Vec<CalendarSource>,
    /// Selected day used as the anchor for whichever view is active. On a
    /// fresh install this deserializes to the sentinel below, which the app
    /// replaces with the real current date at the configured UTC offset —
    /// a fresh install must never open on a hardcoded calendar date.
    #[serde(default = "unset_anchor")]
    pub anchor_date: NaiveDate,
}

fn default_view_mode() -> ViewMode {
    ViewMode::Month
}

/// Sentinel for "no anchor chosen yet". Any date before 2000 is treated as
/// unset by the app (see `App::new`), so this can never be confused with a
/// date a user actually navigated to.
fn unset_anchor() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
}

/// Whether `date` is the "not chosen yet" sentinel.
pub fn is_unset_anchor(date: NaiveDate) -> bool {
    date.year() < 2000
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            utc_offset_minutes: 0,
            view_mode: ViewMode::Month,
            sources: Vec::new(),
            anchor_date: unset_anchor(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_day_event_dates_are_inclusive_of_start_exclusive_of_end() {
        let t = EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(2026, 3, 4).unwrap(),
        };
        let dates = t.dates();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
            ]
        );
    }

    #[test]
    fn timed_event_spanning_midnight_touches_two_dates() {
        let t = EventTime::Timed {
            start: NaiveDate::from_ymd_opt(2026, 3, 1)
                .unwrap()
                .and_hms_opt(23, 0, 0)
                .unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 3, 2)
                .unwrap()
                .and_hms_opt(1, 0, 0)
                .unwrap(),
        };
        assert_eq!(t.dates().len(), 2);
    }

    #[test]
    fn default_config_anchor_is_the_unset_sentinel_not_a_hardcoded_date() {
        let config = AppConfig::default();
        assert!(is_unset_anchor(config.anchor_date));
        // A real navigated-to date is never mistaken for "unset".
        assert!(!is_unset_anchor(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        ));
    }

    #[test]
    fn config_without_an_anchor_field_deserializes_to_the_unset_sentinel() {
        let config: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(is_unset_anchor(config.anchor_date));
    }
}
