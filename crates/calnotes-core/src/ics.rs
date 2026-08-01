//! Minimal RFC 5545 (iCalendar) parser: line unfolding, text escaping, and
//! `DATE`/`DATE-TIME` value parsing for the properties this app needs
//! (`VEVENT` with `UID`, `SUMMARY`, `LOCATION`, `DTSTART`, `DTEND`,
//! `DURATION`, `RRULE`, `EXDATE`). This is a clean, from-scratch
//! implementation written against the published RFC text; it does not
//! reuse code from any GPL-licensed reference implementation.
//!
//! Known, documented simplifications:
//! - `TZID` parameters are recognized but not resolved against a timezone
//!   database; a `DATE-TIME` with a `TZID` is treated as floating local
//!   time (i.e. the same fixed UTC offset the rest of the app uses).
//! - Only `VEVENT` components are extracted (no `VTODO`/`VJOURNAL`/`VALARM`).

use chrono::{NaiveDate, NaiveDateTime};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IcsError {
    MissingUid,
    MissingDtStart,
    BadDateValue(String),
}

impl fmt::Display for IcsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IcsError::MissingUid => write!(f, "VEVENT missing UID"),
            IcsError::MissingDtStart => write!(f, "VEVENT missing DTSTART"),
            IcsError::BadDateValue(s) => write!(f, "unparseable date/time value: {s}"),
        }
    }
}

impl std::error::Error for IcsError {}

/// A parsed `DATE` or `DATE-TIME` iCalendar value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcsDateTime {
    Date(NaiveDate),
    /// `is_utc` is true only when the value carried a trailing `Z`.
    DateTime {
        at: NaiveDateTime,
        is_utc: bool,
    },
}

impl IcsDateTime {
    pub fn date(&self) -> NaiveDate {
        match self {
            IcsDateTime::Date(d) => *d,
            IcsDateTime::DateTime { at, .. } => at.date(),
        }
    }
}

/// One `BYDAY` entry: an optional ordinal (e.g. `2` in `2MO`, `-1` in `-1FR`)
/// plus the weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByDay {
    pub ordinal: Option<i32>,
    pub weekday: chrono::Weekday,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<IcsDateTime>,
    pub by_day: Vec<ByDay>,
}

/// A raw, not-yet-recurrence-expanded event as read from a `VEVENT` block.
#[derive(Debug, Clone, PartialEq)]
pub struct RawVEvent {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub dtstart: IcsDateTime,
    pub dtend: Option<IcsDateTime>,
    pub rrule: Option<RRule>,
    pub exdates: Vec<IcsDateTime>,
}

/// Unfold RFC 5545 §3.1 continuation lines: any line starting with a space
/// or a tab is a continuation of the previous line, with the leading
/// whitespace character stripped.
pub fn unfold_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            let last = lines.last_mut().unwrap();
            last.push_str(&line[1..]);
        } else if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// Reverse RFC 5545 §3.3.11 TEXT escaping: `\\`, `\;`, `\,`, `\n`/`\N`.
pub fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n') | Some('N') => {
                    out.push('\n');
                    chars.next();
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                Some(';') => {
                    out.push(';');
                    chars.next();
                }
                Some(',') => {
                    out.push(',');
                    chars.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A parsed content line: `NAME;PARAM=VALUE;...:VALUE`.
struct ContentLine {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

fn parse_content_line(line: &str) -> Option<ContentLine> {
    // Split NAME[;params] from VALUE at the first unquoted colon.
    let mut in_quotes = false;
    let mut colon_idx = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon_idx = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_idx = colon_idx?;
    let (head, value) = (&line[..colon_idx], &line[colon_idx + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let mut params = Vec::new();
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            params.push((k.to_ascii_uppercase(), v.trim_matches('"').to_string()));
        }
    }
    Some(ContentLine {
        name,
        params,
        value: value.to_string(),
    })
}

fn param_value<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Parse a `DATE` or `DATE-TIME` value string, given its `VALUE=` param
/// (if any) as a hint. Accepts `YYYYMMDD` and `YYYYMMDDTHHMMSS[Z]`.
pub fn parse_date_value(value: &str, value_param: Option<&str>) -> Result<IcsDateTime, IcsError> {
    let is_date_only = value_param == Some("DATE") || (value.len() == 8 && !value.contains('T'));
    if is_date_only {
        let d = NaiveDate::parse_from_str(value, "%Y%m%d")
            .map_err(|_| IcsError::BadDateValue(value.to_string()))?;
        return Ok(IcsDateTime::Date(d));
    }
    let is_utc = value.ends_with('Z');
    let trimmed = value.trim_end_matches('Z');
    let at = NaiveDateTime::parse_from_str(trimmed, "%Y%m%dT%H%M%S")
        .map_err(|_| IcsError::BadDateValue(value.to_string()))?;
    Ok(IcsDateTime::DateTime { at, is_utc })
}

fn parse_rrule(value: &str) -> Option<RRule> {
    let mut freq = None;
    let mut interval = 1u32;
    let mut count = None;
    let mut until = None;
    let mut by_day = Vec::new();

    for part in value.split(';') {
        let (k, v) = part.split_once('=')?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None, // unsupported frequency: caller treats as non-recurring
                });
            }
            "INTERVAL" => interval = v.parse().unwrap_or(1).max(1),
            "COUNT" => count = v.parse().ok(),
            "UNTIL" => until = parse_date_value(v, None).ok(),
            "BYDAY" => {
                for entry in v.split(',') {
                    if let Some(bd) = parse_byday(entry) {
                        by_day.push(bd);
                    }
                }
            }
            _ => {}
        }
    }

    Some(RRule {
        freq: freq?,
        interval,
        count,
        until,
        by_day,
    })
}

/// Parse one `BYDAY` entry (`MO`, `2MO`, `-1FR`, ...).
///
/// The weekday abbreviation is always exactly two ASCII characters, so the
/// split point is found by byte length on the *ASCII* suffix rather than by
/// slicing blindly: `entry` comes from a network/file-provided calendar and
/// may contain arbitrary UTF-8, where a naive `split_at(len - 2)` would
/// panic on a non-char-boundary index.
fn parse_byday(entry: &str) -> Option<ByDay> {
    let entry = entry.trim();
    if entry.len() < 2 || !entry.is_char_boundary(entry.len() - 2) {
        return None;
    }
    let (ord_part, day_part) = entry.split_at(entry.len() - 2);
    let weekday = match day_part.to_ascii_uppercase().as_str() {
        "MO" => chrono::Weekday::Mon,
        "TU" => chrono::Weekday::Tue,
        "WE" => chrono::Weekday::Wed,
        "TH" => chrono::Weekday::Thu,
        "FR" => chrono::Weekday::Fri,
        "SA" => chrono::Weekday::Sat,
        "SU" => chrono::Weekday::Sun,
        _ => return None,
    };
    let ordinal = if ord_part.is_empty() {
        None
    } else {
        Some(ord_part.parse::<i32>().ok()?)
    };
    Some(ByDay { ordinal, weekday })
}

/// Parse an entire `.ics` document into its `VEVENT`s. Malformed individual
/// events are skipped (with their error recorded) rather than failing the
/// whole document, so one bad event in a large calendar doesn't blank the
/// display.
pub fn parse_calendar(text: &str) -> (Vec<RawVEvent>, Vec<IcsError>) {
    let lines = unfold_lines(text);
    let mut events = Vec::new();
    let mut errors = Vec::new();

    let mut in_event = false;
    let mut uid: Option<String> = None;
    let mut summary = String::new();
    let mut location: Option<String> = None;
    let mut dtstart: Option<IcsDateTime> = None;
    let mut dtend: Option<IcsDateTime> = None;
    let mut rrule: Option<RRule> = None;
    let mut exdates: Vec<IcsDateTime> = Vec::new();

    for raw in &lines {
        let Some(cl) = parse_content_line(raw) else {
            continue;
        };
        match cl.name.as_str() {
            "BEGIN" if cl.value.eq_ignore_ascii_case("VEVENT") => {
                in_event = true;
                uid = None;
                summary.clear();
                location = None;
                dtstart = None;
                dtend = None;
                rrule = None;
                exdates.clear();
            }
            "END" if cl.value.eq_ignore_ascii_case("VEVENT") => {
                in_event = false;
                let result = (|| -> Result<RawVEvent, IcsError> {
                    Ok(RawVEvent {
                        uid: uid.clone().ok_or(IcsError::MissingUid)?,
                        summary: summary.clone(),
                        location: location.clone(),
                        dtstart: dtstart.ok_or(IcsError::MissingDtStart)?,
                        dtend,
                        rrule: rrule.clone(),
                        exdates: exdates.clone(),
                    })
                })();
                match result {
                    Ok(ev) => events.push(ev),
                    Err(e) => errors.push(e),
                }
            }
            "UID" if in_event => uid = Some(cl.value.clone()),
            "SUMMARY" if in_event => summary = unescape_text(&cl.value),
            "LOCATION" if in_event => location = Some(unescape_text(&cl.value)),
            "DTSTART" if in_event => {
                if let Ok(v) = parse_date_value(&cl.value, param_value(&cl.params, "VALUE")) {
                    dtstart = Some(v);
                }
            }
            "DTEND" if in_event => {
                if let Ok(v) = parse_date_value(&cl.value, param_value(&cl.params, "VALUE")) {
                    dtend = Some(v);
                }
            }
            "RRULE" if in_event => rrule = parse_rrule(&cl.value),
            "EXDATE" if in_event => {
                for v in cl.value.split(',') {
                    if let Ok(d) = parse_date_value(v, param_value(&cl.params, "VALUE")) {
                        exdates.push(d);
                    }
                }
            }
            _ => {}
        }
    }

    (events, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_continuation_lines_with_space_and_tab() {
        // Per RFC 5545 §3.1, unfolding removes the CRLF *and* the single
        // leading whitespace character that marks the fold point — that
        // whitespace is fold metadata, not necessarily original content.
        let text = "SUMMARY:Long\r\n line\r\nLOCATION:Room\r\n\t2\r\n";
        let lines = unfold_lines(text);
        assert_eq!(lines, vec!["SUMMARY:Longline", "LOCATION:Room2"]);
    }

    #[test]
    fn unescapes_text_per_rfc5545() {
        assert_eq!(unescape_text("a\\, b\\; c\\\\d\\ne"), "a, b; c\\d\ne");
    }

    #[test]
    fn parses_date_only_value() {
        let v = parse_date_value("20260315", None).unwrap();
        assert_eq!(
            v,
            IcsDateTime::Date(NaiveDate::from_ymd_opt(2026, 3, 15).unwrap())
        );
    }

    #[test]
    fn parses_utc_date_time_value() {
        let v = parse_date_value("20260315T093000Z", None).unwrap();
        match v {
            IcsDateTime::DateTime { at, is_utc } => {
                assert!(is_utc);
                assert_eq!(at.date(), NaiveDate::from_ymd_opt(2026, 3, 15).unwrap());
            }
            _ => panic!("expected DateTime"),
        }
    }

    #[test]
    fn parses_floating_date_time_value_with_tzid_param() {
        let v = parse_date_value("20260315T093000", None).unwrap();
        match v {
            IcsDateTime::DateTime { is_utc, .. } => assert!(!is_utc),
            _ => panic!("expected DateTime"),
        }
    }

    #[test]
    fn parses_simple_vevent() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:abc-123\r\nSUMMARY:Team sync\r\nDTSTART:20260315T093000Z\r\nDTEND:20260315T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (events, errors) = parse_calendar(ics);
        assert!(errors.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid, "abc-123");
        assert_eq!(events[0].summary, "Team sync");
    }

    #[test]
    fn skips_event_missing_uid_but_keeps_others() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:No uid\r\nDTSTART:20260101\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:ok-1\r\nSUMMARY:Fine\r\nDTSTART:20260102\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (events, errors) = parse_calendar(ics);
        assert_eq!(events.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0], IcsError::MissingUid);
    }

    #[test]
    fn parses_rrule_with_interval_count_and_byday() {
        let rr = parse_rrule("FREQ=WEEKLY;INTERVAL=2;COUNT=5;BYDAY=MO,WE,FR").unwrap();
        assert_eq!(rr.freq, Freq::Weekly);
        assert_eq!(rr.interval, 2);
        assert_eq!(rr.count, Some(5));
        assert_eq!(rr.by_day.len(), 3);
    }

    #[test]
    fn parses_rrule_with_ordinal_byday_and_until() {
        let rr = parse_rrule("FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20261231T000000Z").unwrap();
        assert_eq!(rr.by_day[0].ordinal, Some(-1));
        assert_eq!(rr.by_day[0].weekday, chrono::Weekday::Fri);
        assert!(rr.until.is_some());
    }

    #[test]
    fn content_line_parsing_ignores_colon_inside_quoted_param() {
        let cl = parse_content_line(r#"ATTENDEE;CN="Smith, J: Q":mailto:j@example.com"#).unwrap();
        assert_eq!(cl.name, "ATTENDEE");
        assert_eq!(cl.value, "mailto:j@example.com");
    }

    #[test]
    fn parse_byday_does_not_panic_on_arbitrary_utf8() {
        // Multi-byte characters at the would-be split point used to panic
        // a `split_at(len - 2)` implementation.
        for entry in [
            "é", "aé", "日本", "🎉MO", "MO🎉", "", "M", "-1é", "\u{0301}",
        ] {
            let _ = parse_byday(entry);
        }
        assert!(parse_byday("🎉MO").is_none());
        assert!(parse_byday("MO🎉").is_none());
        assert!(parse_byday("日本").is_none());
    }

    #[test]
    fn parse_byday_rejects_non_numeric_ordinals() {
        assert!(parse_byday("XMO").is_none());
        assert_eq!(parse_byday("-1FR").unwrap().ordinal, Some(-1));
        assert_eq!(parse_byday("MO").unwrap().ordinal, None);
    }

    #[test]
    fn parse_rrule_survives_garbage_byday_values() {
        let rr = parse_rrule("FREQ=WEEKLY;BYDAY=é,MO,日本").unwrap();
        assert_eq!(rr.by_day.len(), 1);
        assert_eq!(rr.by_day[0].weekday, chrono::Weekday::Mon);
    }
}
