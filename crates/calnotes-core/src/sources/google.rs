//! Google Calendar via OAuth 2.0 device authorization grant (RFC 8628).
//!
//! The user creates their own OAuth client (any "TV and Limited Input"
//! device client works) and enters the client ID/secret in-app; the app
//! never ships its own client credentials. The device flow shows a short
//! verification code and URL the user completes on any other
//! browser-capable device, then polls until Google issues tokens. Only the
//! refresh token is persisted (never logged); a fresh access token is
//! requested on every sync.

use super::SourceError;
use crate::model::Event;
use crate::recurrence::Window;
use crate::timeutil::UtcOffset;
use serde::Deserialize;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    #[serde(default = "default_interval")]
    pub interval: u64,
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
}

/// Step 1 of the device flow: request a `user_code`/`verification_url` to
/// show the user, and a `device_code` to poll with.
pub fn request_device_code(client_id: &str) -> Result<DeviceCodeResponse, SourceError> {
    let response = ureq::post(DEVICE_CODE_URL)
        .timeout(REQUEST_TIMEOUT)
        .send_form(&[("client_id", client_id), ("scope", CALENDAR_SCOPE)])
        .map_err(|e| SourceError::Http(e.to_string()))?;
    response
        .into_json()
        .map_err(|e| SourceError::Parse(e.to_string()))
}

/// The outcome of a single device-flow poll attempt.
pub enum PollOutcome {
    /// The user hasn't approved yet; wait `interval` seconds and try again.
    Pending,
    /// The server asked us to poll less often; increase the interval before
    /// the next attempt (RFC 8628 §3.5 `slow_down`).
    SlowDown,
    /// Login completed successfully.
    Approved {
        access_token: String,
        refresh_token: String,
    },
}

/// Step 2: poll once for whether the user has approved the device code.
/// The caller is responsible for the polling loop/sleep so this stays
/// synchronous, testable, and UI-thread-friendly (the settings screen can
/// call this once per tick while showing the code).
pub fn poll_device_token(
    client_id: &str,
    client_secret: &str,
    device_code: &str,
) -> Result<PollOutcome, SourceError> {
    let response = ureq::post(TOKEN_URL).timeout(REQUEST_TIMEOUT).send_form(&[
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("device_code", device_code),
        ("grant_type", DEVICE_GRANT_TYPE),
    ]);

    match response {
        Ok(resp) => {
            let token: TokenResponse = resp
                .into_json()
                .map_err(|e| SourceError::Parse(e.to_string()))?;
            let refresh_token = token
                .refresh_token
                .ok_or_else(|| SourceError::Auth("Google did not return a refresh token".into()))?;
            Ok(PollOutcome::Approved {
                access_token: token.access_token,
                refresh_token,
            })
        }
        Err(ureq::Error::Status(400, resp)) | Err(ureq::Error::Status(428, resp)) => {
            let body: Result<TokenErrorResponse, _> = resp.into_json();
            match body.map(|b| b.error) {
                Ok(code) if code == "authorization_pending" => Ok(PollOutcome::Pending),
                Ok(code) if code == "slow_down" => Ok(PollOutcome::SlowDown),
                Ok(code) => Err(SourceError::Auth(code)),
                Err(e) => Err(SourceError::Parse(e.to_string())),
            }
        }
        Err(e) => Err(SourceError::Http(e.to_string())),
    }
}

/// Exchange a persisted refresh token for a fresh access token.
pub fn refresh_access_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<String, SourceError> {
    let response = ureq::post(TOKEN_URL)
        .timeout(REQUEST_TIMEOUT)
        .send_form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .map_err(|e| SourceError::Http(e.to_string()))?;
    let token: TokenResponse = response
        .into_json()
        .map_err(|e| SourceError::Parse(e.to_string()))?;
    Ok(token.access_token)
}

#[derive(Debug, Deserialize)]
struct CalendarEventsResponse {
    items: Vec<GoogleEvent>,
}

#[derive(Debug, Deserialize)]
struct GoogleEvent {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    location: Option<String>,
    start: GoogleEventDateTime,
    end: GoogleEventDateTime,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleEventDateTime {
    date: Option<String>,
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
}

/// Fetch events (already recurrence-expanded server-side via
/// `singleEvents=true`) within `window` from the Calendar API v3.
///
/// `access_token` is sent as a standard OAuth 2.0 bearer credential
/// (RFC 6750 §2.1). It is never logged: errors returned from here carry
/// only `ureq`'s URL/status text, never request headers or token values.
pub fn fetch_events(
    access_token: &str,
    calendar_id: &str,
    source_id: &str,
    window: Window,
    offset: UtcOffset,
) -> Result<Vec<Event>, SourceError> {
    let encoded_id = urlencode(calendar_id);
    // Ask Google for the UTC span that corresponds to the local display
    // window, so an event that only falls in-window after the fixed-offset
    // conversion is still returned.
    let time_min = offset.to_utc(window.start.and_hms_opt(0, 0, 0).unwrap());
    let time_max = offset.to_utc(window.end.and_hms_opt(0, 0, 0).unwrap());
    let url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/{encoded_id}/events\
         ?singleEvents=true&orderBy=startTime\
         &timeMin={}&timeMax={}",
        time_min.format("%Y-%m-%dT%H:%M:%SZ"),
        time_max.format("%Y-%m-%dT%H:%M:%SZ"),
    );
    let response = ureq::get(&url)
        .timeout(REQUEST_TIMEOUT)
        .set("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(|e| SourceError::Http(e.to_string()))?;
    let parsed: CalendarEventsResponse = response
        .into_json()
        .map_err(|e| SourceError::Parse(e.to_string()))?;

    let mut events = Vec::new();
    for item in parsed.items {
        if item.status.as_deref() == Some("cancelled") {
            continue;
        }
        let Some(time) = google_event_time(&item.start, &item.end, offset) else {
            continue;
        };
        events.push(Event {
            id: item.id,
            source_id: source_id.to_string(),
            summary: item.summary,
            location: item.location,
            time,
        });
    }
    Ok(events)
}

/// Convert a Google event's start/end into the app's wall-clock model.
///
/// All-day events carry plain dates and are used as-is. Timed events carry
/// RFC 3339 timestamps *with* an explicit offset (`Z` or `±HH:MM`); those
/// are genuine instants, so they are converted to UTC and then into the
/// app's single configured fixed offset — which can legitimately place an
/// event on a different calendar date than the one Google's string spells.
fn google_event_time(
    start: &GoogleEventDateTime,
    end: &GoogleEventDateTime,
    offset: UtcOffset,
) -> Option<crate::model::EventTime> {
    use chrono::NaiveDate;
    if let (Some(sd), Some(ed)) = (&start.date, &end.date) {
        let s = NaiveDate::parse_from_str(sd, "%Y-%m-%d").ok()?;
        let e = NaiveDate::parse_from_str(ed, "%Y-%m-%d").ok()?;
        return Some(crate::model::EventTime::AllDay {
            start: s,
            end_exclusive: e,
        });
    }
    if let (Some(sdt), Some(edt)) = (&start.date_time, &end.date_time) {
        let s = parse_rfc3339_to_local(sdt, offset)?;
        let e = parse_rfc3339_to_local(edt, offset)?;
        return Some(crate::model::EventTime::Timed { start: s, end: e });
    }
    None
}

/// Parse an RFC 3339 timestamp and express it in local wall-clock time at
/// `offset`. A value carrying no offset marker at all (which Google does
/// not normally emit for `dateTime`) is treated as already-local floating
/// time, matching the ICS path's handling of `TZID`/floating values.
fn parse_rfc3339_to_local(value: &str, offset: UtcOffset) -> Option<chrono::NaiveDateTime> {
    use chrono::{DateTime, NaiveDateTime};
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Some(offset.utc_naive_to_local(dt.naive_utc()));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").ok()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_escapes_reserved_characters_but_keeps_at_sign() {
        assert_eq!(urlencode("me@example.com"), "me@example.com");
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn parses_all_day_google_event_dates() {
        let start = GoogleEventDateTime {
            date: Some("2026-03-01".into()),
            date_time: None,
        };
        let end = GoogleEventDateTime {
            date: Some("2026-03-02".into()),
            date_time: None,
        };
        let time = google_event_time(&start, &end, UtcOffset::new(0)).unwrap();
        assert!(time.is_all_day());
    }

    fn timed(start: &str, end: &str, offset_minutes: i32) -> crate::model::EventTime {
        google_event_time(
            &GoogleEventDateTime {
                date: None,
                date_time: Some(start.into()),
            },
            &GoogleEventDateTime {
                date: None,
                date_time: Some(end.into()),
            },
            UtcOffset::new(offset_minutes),
        )
        .unwrap()
    }

    #[test]
    fn rfc3339_offset_is_converted_into_the_configured_fixed_offset() {
        // 09:00-05:00 == 14:00Z == 15:00 at UTC+01:00.
        let time = timed("2026-03-01T09:00:00-05:00", "2026-03-01T10:00:00-05:00", 60);
        match time {
            crate::model::EventTime::Timed { start, end } => {
                assert_eq!(
                    start.format("%Y-%m-%d %H:%M").to_string(),
                    "2026-03-01 15:00"
                );
                assert_eq!((end - start).num_minutes(), 60);
            }
            _ => panic!("expected Timed"),
        }
    }

    #[test]
    fn rfc3339_conversion_can_move_an_event_to_the_next_local_date() {
        // 23:30-05:00 on Mar 1 == 04:30Z on Mar 2 == 09:30 on Mar 2 at UTC+05:00.
        let time = timed(
            "2026-03-01T23:30:00-05:00",
            "2026-03-02T00:30:00-05:00",
            300,
        );
        assert_eq!(
            time.start_date(),
            chrono::NaiveDate::from_ymd_opt(2026, 3, 2).unwrap()
        );
    }

    #[test]
    fn rfc3339_conversion_can_move_an_event_to_the_previous_local_date() {
        // 01:00Z on Mar 1 == 20:00 on Feb 28 at UTC-05:00.
        let time = timed("2026-03-01T01:00:00Z", "2026-03-01T02:00:00Z", -300);
        assert_eq!(
            time.start_date(),
            chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
        );
    }

    #[test]
    fn rfc3339_value_without_an_offset_is_treated_as_local_floating_time() {
        let parsed = parse_rfc3339_to_local("2026-03-01T09:00:00", UtcOffset::new(-300)).unwrap();
        assert_eq!(
            parsed.format("%Y-%m-%d %H:%M").to_string(),
            "2026-03-01 09:00"
        );
    }

    #[test]
    fn device_code_response_deserializes_from_google_json() {
        let json = r#"{"device_code":"d1","user_code":"ABCD-EFGH","verification_url":"https://www.google.com/device","interval":5,"expires_in":1800}"#;
        let parsed: DeviceCodeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.user_code, "ABCD-EFGH");
        assert_eq!(parsed.interval, 5);
    }
}
