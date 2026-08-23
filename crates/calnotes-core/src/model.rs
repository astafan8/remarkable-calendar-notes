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
    /// Every view, in the app's default button order (Day, Work Week, Week,
    /// Two Weeks, Month, Two Months). Also the order shown in the settings
    /// view picker.
    pub const ALL: [ViewMode; 6] = [
        ViewMode::Day,
        ViewMode::WorkWeek,
        ViewMode::Week,
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
    /// Which views appear as buttons on the calendar screen, in the order
    /// they should appear. Chosen in the settings screen. Empty or invalid
    /// values fall back to every view via [`AppConfig::ordered_views`].
    #[serde(default = "default_visible_views")]
    pub visible_views: Vec<ViewMode>,
    /// Text size for calendar event summaries inside a day cell. A larger
    /// number is bigger; clamped to a sane range by
    /// [`AppConfig::event_text_scale_clamped`].
    ///
    /// Kept for backward compatibility: it stores the whole-point size and
    /// is mirrored from [`AppConfig::event_text_scale_tenths`] so that older
    /// builds (which only understand whole points) still read a sane value.
    #[serde(default = "default_event_text_scale")]
    pub event_text_scale: i32,
    /// Text size for calendar event summaries in *tenths* of a point, so
    /// half-point sizes (e.g. 25 = 2.5, 35 = 3.5) are possible. When absent
    /// (older config) the whole-point [`AppConfig::event_text_scale`] is
    /// used. Clamped by [`AppConfig::event_text_scale_tenths_clamped`].
    #[serde(default)]
    pub event_text_scale_tenths: Option<i32>,
    /// The view the app opens on at startup. Clamped to a currently-visible
    /// view by [`AppConfig::startup_view`]. Ignored when
    /// [`AppConfig::startup_last_used`] is set.
    #[serde(default = "default_view_mode")]
    pub default_view: ViewMode,
    /// When true, the app opens on the last-used view instead of
    /// `default_view`.
    #[serde(default)]
    pub startup_last_used: bool,
    /// When true (default), read the pen digitizer directly for smoother
    /// handwriting; falls back to QTFB pen events automatically if the
    /// device can't be read. Turn off if raw pen ink lands in the wrong
    /// place on your unit.
    #[serde(default = "default_true")]
    pub raw_pen_input: bool,
    /// Minimum milliseconds between on-screen updates while a stroke is in
    /// progress. Ink is always captured losslessly; this only throttles how
    /// often the screen is refreshed so the display host is never flooded
    /// with more repaint requests than it can drain (which used to make a
    /// fast stroke stall and then "catch up"). 0 publishes every poll cycle
    /// (the old behaviour). Tunable from the settings screen.
    #[serde(default = "default_pen_refresh_ms")]
    pub pen_refresh_ms: i32,
}

impl AppConfig {
    /// The lower/upper bounds the settings screen enforces on the event
    /// text size.
    pub const EVENT_TEXT_SCALE_MIN: i32 = 2;
    pub const EVENT_TEXT_SCALE_MAX: i32 = 6;

    /// The same bounds expressed in tenths of a point, plus the step used by
    /// the +/- buttons (half a point).
    pub const EVENT_TEXT_SCALE_TENTHS_MIN: i32 = 20;
    pub const EVENT_TEXT_SCALE_TENTHS_MAX: i32 = 60;
    pub const EVENT_TEXT_SCALE_TENTHS_STEP: i32 = 5;

    /// Bounds and step (ms) for the pen-refresh throttle.
    pub const PEN_REFRESH_MS_MIN: i32 = 0;
    pub const PEN_REFRESH_MS_MAX: i32 = 50;
    pub const PEN_REFRESH_MS_STEP: i32 = 2;

    /// The pen-refresh throttle in milliseconds, clamped to a sane range.
    pub fn pen_refresh_ms_clamped(&self) -> i32 {
        self.pen_refresh_ms
            .clamp(Self::PEN_REFRESH_MS_MIN, Self::PEN_REFRESH_MS_MAX)
    }

    /// The event text size, clamped to the supported range.
    pub fn event_text_scale_clamped(&self) -> i32 {
        self.event_text_scale
            .clamp(Self::EVENT_TEXT_SCALE_MIN, Self::EVENT_TEXT_SCALE_MAX)
    }

    /// The event text size in tenths of a point, clamped to the supported
    /// range. Falls back to the whole-point value for configs written before
    /// half-point sizes existed.
    pub fn event_text_scale_tenths_clamped(&self) -> i32 {
        self.event_text_scale_tenths
            .unwrap_or(self.event_text_scale_clamped() * 10)
            .clamp(
                Self::EVENT_TEXT_SCALE_TENTHS_MIN,
                Self::EVENT_TEXT_SCALE_TENTHS_MAX,
            )
    }

    /// The event text size as a fractional point value used by the renderer.
    pub fn event_text_scale_f32(&self) -> f32 {
        self.event_text_scale_tenths_clamped() as f32 / 10.0
    }

    /// A human-readable label for the current size, e.g. "3" or "3.5".
    pub fn event_text_scale_label(&self) -> String {
        let tenths = self.event_text_scale_tenths_clamped();
        if tenths % 10 == 0 {
            (tenths / 10).to_string()
        } else {
            format!("{}.{}", tenths / 10, tenths % 10)
        }
    }

    /// Set the event text size from a value in tenths of a point, clamping
    /// to the supported range and keeping the legacy whole-point field in
    /// sync so older builds stay sane.
    pub fn set_event_text_scale_tenths(&mut self, tenths: i32) {
        let clamped = tenths.clamp(
            Self::EVENT_TEXT_SCALE_TENTHS_MIN,
            Self::EVENT_TEXT_SCALE_TENTHS_MAX,
        );
        self.event_text_scale_tenths = Some(clamped);
        // Round to the nearest whole point for the legacy field.
        self.event_text_scale =
            ((clamped + 5) / 10).clamp(Self::EVENT_TEXT_SCALE_MIN, Self::EVENT_TEXT_SCALE_MAX);
    }

    /// The views to show as buttons, in order — de-duplicated, and falling
    /// back to every view (in the default order) when nothing valid is
    /// selected, so the calendar screen is never left without buttons.
    pub fn ordered_views(&self) -> Vec<ViewMode> {
        let mut seen = Vec::new();
        for v in &self.visible_views {
            if !seen.contains(v) {
                seen.push(*v);
            }
        }
        if seen.is_empty() {
            ViewMode::ALL.to_vec()
        } else {
            seen
        }
    }

    /// The view to open on at startup: the last-used view when
    /// `startup_last_used` is set, otherwise the configured `default_view` —
    /// each clamped to a currently-visible view.
    pub fn startup_view(&self, last_used: ViewMode) -> ViewMode {
        let views = self.ordered_views();
        let wanted = if self.startup_last_used {
            last_used
        } else {
            self.default_view
        };
        if views.contains(&wanted) {
            wanted
        } else {
            views[0]
        }
    }
}

fn default_view_mode() -> ViewMode {
    ViewMode::Month
}

fn default_visible_views() -> Vec<ViewMode> {
    ViewMode::ALL.to_vec()
}

fn default_event_text_scale() -> i32 {
    3
}

fn default_true() -> bool {
    true
}

fn default_pen_refresh_ms() -> i32 {
    12
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
            visible_views: default_visible_views(),
            event_text_scale: default_event_text_scale(),
            event_text_scale_tenths: None,
            default_view: default_view_mode(),
            startup_last_used: false,
            raw_pen_input: true,
            pen_refresh_ms: default_pen_refresh_ms(),
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
    fn ordered_views_dedupes_and_falls_back_to_all_when_empty() {
        let config = AppConfig {
            visible_views: vec![ViewMode::Month, ViewMode::Day, ViewMode::Month],
            ..AppConfig::default()
        };
        assert_eq!(config.ordered_views(), vec![ViewMode::Month, ViewMode::Day]);
        let config = AppConfig {
            visible_views: vec![],
            ..AppConfig::default()
        };
        assert_eq!(config.ordered_views(), ViewMode::ALL.to_vec());
    }

    #[test]
    fn startup_view_clamps_to_a_visible_view() {
        let config = AppConfig {
            visible_views: vec![ViewMode::Day, ViewMode::Week],
            default_view: ViewMode::Week, // visible → used as-is
            ..AppConfig::default()
        };
        assert_eq!(config.startup_view(ViewMode::Month), ViewMode::Week);
        let config = AppConfig {
            visible_views: vec![ViewMode::Day, ViewMode::Week],
            default_view: ViewMode::Month, // not visible → first visible
            ..AppConfig::default()
        };
        assert_eq!(config.startup_view(ViewMode::TwoMonths), ViewMode::Day);
    }

    #[test]
    fn startup_view_uses_last_used_when_enabled() {
        let config = AppConfig {
            visible_views: vec![ViewMode::Day, ViewMode::Week, ViewMode::Month],
            startup_last_used: true,
            default_view: ViewMode::Day,
            ..AppConfig::default()
        };
        // Last-used wins over default_view when visible…
        assert_eq!(config.startup_view(ViewMode::Week), ViewMode::Week);
        // …and falls back to the first visible view when not.
        assert_eq!(config.startup_view(ViewMode::TwoMonths), ViewMode::Day);
    }

    #[test]
    fn event_text_scale_is_clamped_to_the_supported_range() {
        let config = AppConfig {
            event_text_scale: 99,
            ..AppConfig::default()
        };
        assert_eq!(
            config.event_text_scale_clamped(),
            AppConfig::EVENT_TEXT_SCALE_MAX
        );
        let config = AppConfig {
            event_text_scale: -5,
            ..AppConfig::default()
        };
        assert_eq!(
            config.event_text_scale_clamped(),
            AppConfig::EVENT_TEXT_SCALE_MIN
        );
    }

    #[test]
    fn event_text_scale_tenths_supports_half_points_and_migrates_old_configs() {
        // A config without the tenths field falls back to the whole-point
        // value (3 -> 3.0).
        let legacy: AppConfig = serde_json::from_str(r#"{"event_text_scale":4}"#).unwrap();
        assert_eq!(legacy.event_text_scale_tenths_clamped(), 40);
        assert_eq!(legacy.event_text_scale_f32(), 4.0);
        assert_eq!(legacy.event_text_scale_label(), "4");

        // Setting a half-point value clamps, labels, and mirrors the legacy
        // whole-point field.
        let mut config = AppConfig::default();
        config.set_event_text_scale_tenths(35);
        assert_eq!(config.event_text_scale_tenths_clamped(), 35);
        assert_eq!(config.event_text_scale_f32(), 3.5);
        assert_eq!(config.event_text_scale_label(), "3.5");
        assert_eq!(config.event_text_scale, 4); // nearest whole point

        // Out-of-range values clamp to the supported tenths bounds.
        config.set_event_text_scale_tenths(1000);
        assert_eq!(
            config.event_text_scale_tenths_clamped(),
            AppConfig::EVENT_TEXT_SCALE_TENTHS_MAX
        );
        config.set_event_text_scale_tenths(0);
        assert_eq!(
            config.event_text_scale_tenths_clamped(),
            AppConfig::EVENT_TEXT_SCALE_TENTHS_MIN
        );
    }

    #[test]
    fn default_button_order_starts_with_day_then_work_week_then_week() {
        assert_eq!(
            &ViewMode::ALL[..3],
            &[ViewMode::Day, ViewMode::WorkWeek, ViewMode::Week]
        );
    }

    #[test]
    fn config_without_an_anchor_field_deserializes_to_the_unset_sentinel() {
        let config: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(is_unset_anchor(config.anchor_date));
    }

    #[test]
    fn config_defaults_enable_raw_pen_and_the_new_display_fields() {
        // An old config (pre-dating these fields) still deserializes with
        // sensible defaults.
        let config: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(config.raw_pen_input);
        assert_eq!(config.visible_views, ViewMode::ALL.to_vec());
        assert_eq!(config.event_text_scale, 3);
        assert!(!config.startup_last_used);
    }
}
