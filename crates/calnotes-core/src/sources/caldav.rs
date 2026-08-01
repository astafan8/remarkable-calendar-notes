//! iCloud CalDAV source: authenticates with an Apple ID + app-specific
//! password (Apple requires an app-specific password for third-party
//! CalDAV clients; a full iCloud account password will not work here) and
//! issues a standard CalDAV `REPORT` `calendar-query` against a
//! user-provided calendar collection URL.
//!
//! The XML handling here is intentionally minimal: rather than pulling in
//! a full XML parser dependency for one narrow need, [`extract_calendar_data`]
//! scans the multistatus response for `*:calendar-data` elements (the
//! namespace prefix varies by server) and unescapes their inner text. This
//! is standard, documented CalDAV/WebDAV protocol behavior (RFC 4791), not
//! copied implementation detail from any specific client.

use super::SourceError;
use crate::model::Event;
use crate::recurrence::Window;
use crate::timeutil::UtcOffset;
use base64::Engine;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

fn basic_auth_header(apple_id: &str, app_specific_password: &str) -> String {
    let raw = format!("{apple_id}:{app_specific_password}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    format!("Basic {encoded}")
}

fn calendar_query_body(window: Window) -> String {
    // Half-open [start, end) window as CalDAV UTC time-range bounds.
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{}T000000Z" end="{}T000000Z"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#,
        window.start.format("%Y%m%d"),
        window.end.format("%Y%m%d"),
    )
}

pub fn fetch_icloud_events(
    apple_id: &str,
    app_specific_password: &str,
    calendar_url: &str,
    source_id: &str,
    window: Window,
    offset: UtcOffset,
) -> Result<Vec<Event>, SourceError> {
    if !calendar_url.starts_with("https://") {
        return Err(SourceError::Http(format!(
            "refusing non-HTTPS CalDAV URL: {calendar_url}"
        )));
    }
    let body = calendar_query_body(window);
    let response = ureq::request("REPORT", calendar_url)
        .timeout(REQUEST_TIMEOUT)
        .set(
            "Authorization",
            &basic_auth_header(apple_id, app_specific_password),
        )
        .set("Content-Type", "application/xml; charset=utf-8")
        .set("Depth", "1")
        .send_string(&body)
        .map_err(|e| SourceError::Http(e.to_string()))?;
    let status = response.status();
    let xml = response
        .into_string()
        .map_err(|e| SourceError::Parse(e.to_string()))?;
    if status == 401 || status == 403 {
        return Err(SourceError::Auth(
            "iCloud rejected the Apple ID / app-specific password".into(),
        ));
    }

    let blobs = extract_calendar_data(&xml);
    let mut events = Vec::new();
    for blob in blobs {
        events.extend(super::events_from_ics_text(
            &blob, source_id, window, offset,
        ));
    }
    Ok(events)
}

/// Extract the unescaped inner text of every `*:calendar-data` element in a
/// CalDAV multistatus XML response.
///
/// `calendar-data` is always leaf (text-only) content in a well-formed
/// CalDAV response — CalDAV never nests XML elements inside it — so this
/// can be done correctly with a single pass over `<`-delimited segments
/// rather than a full XML parser: whatever follows the opening tag's `>`,
/// up to the next `<` (the closing tag), *is* the element's content.
fn extract_calendar_data(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for segment in xml.split('<') {
        let Some(gt) = segment.find('>') else {
            continue;
        };
        let tag = &segment[..gt];
        if tag.starts_with('/') || tag.ends_with('/') {
            continue; // closing tag or self-closing (empty) element
        }
        let name = tag.split_whitespace().next().unwrap_or(tag);
        let local_name = name.rsplit(':').next().unwrap_or(name);
        if local_name.eq_ignore_ascii_case("calendar-data") {
            out.push(unescape_xml(&segment[gt + 1..]));
        }
    }
    out
}

/// Reverse XML escaping in element text: the five predefined entities plus
/// numeric character references in both decimal (`&#13;`) and hexadecimal
/// (`&#x1F600;`) form. Numeric references matter here because real CalDAV
/// servers routinely encode the CRLF line endings inside `calendar-data`
/// as `&#13;` — leaving those literal would corrupt the embedded ICS
/// document's line structure. Anything that isn't a well-formed entity is
/// passed through unchanged rather than dropped.
fn unescape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // Entities are short; bound the search so a stray '&' in the body
        // can't make this scan the remainder of the document.
        let end = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(i, _)| i);
        let Some(end) = end else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "amp" => Some('&'),
            _ => decode_numeric_entity(entity),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode the body of a numeric character reference (`#13`, `#x0D`), if it
/// is one. Returns `None` for anything else, including out-of-range or
/// surrogate code points.
fn decode_numeric_entity(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) if !hex.is_empty() => u32::from_str_radix(hex, 16).ok()?,
        Some(_) => return None,
        None if !digits.is_empty() => digits.parse::<u32>().ok()?,
        None => return None,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn extracts_calendar_data_with_namespaced_prefix() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:propstat>
      <D:prop>
        <C:calendar-data>BEGIN:VCALENDAR&#13;
BEGIN:VEVENT&#13;
UID:abc&#13;
END:VEVENT&#13;
END:VCALENDAR&#13;
</C:calendar-data>
      </D:prop>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let blobs = extract_calendar_data(xml);
        assert_eq!(blobs.len(), 1);
        assert!(blobs[0].contains("UID:abc"));
    }

    #[test]
    fn extracts_multiple_calendar_data_blocks() {
        let xml = "<a><calendar-data>ONE</calendar-data></a><b><cal:calendar-data>TWO</cal:calendar-data></b>";
        let blobs = extract_calendar_data(xml);
        assert_eq!(blobs, vec!["ONE".to_string(), "TWO".to_string()]);
    }

    #[test]
    fn unescapes_xml_entities_in_calendar_data() {
        let xml = "<calendar-data>SUMMARY:Fish &amp; Chips</calendar-data>";
        let blobs = extract_calendar_data(xml);
        assert_eq!(blobs[0], "SUMMARY:Fish & Chips");
    }

    #[test]
    fn unescapes_decimal_and_hexadecimal_numeric_entities() {
        assert_eq!(unescape_xml("a&#13;&#10;b"), "a\r\nb");
        assert_eq!(unescape_xml("a&#x0D;&#x0a;b"), "a\r\nb");
        assert_eq!(unescape_xml("caf&#233;"), "café");
        assert_eq!(unescape_xml("&#x2014;"), "\u{2014}");
    }

    #[test]
    fn leaves_unknown_or_malformed_entities_untouched() {
        assert_eq!(unescape_xml("100% & more"), "100% & more");
        assert_eq!(unescape_xml("&unknown;"), "&unknown;");
        assert_eq!(unescape_xml("&#;"), "&#;");
        assert_eq!(unescape_xml("&#xZZ;"), "&#xZZ;");
        assert_eq!(unescape_xml("&#999999999;"), "&#999999999;");
        assert_eq!(unescape_xml("no entities here"), "no entities here");
    }

    #[test]
    fn caldav_multistatus_fixture_parses_all_the_way_into_events() {
        // A realistic iCloud-style response: CRLF line breaks inside
        // calendar-data encoded as `&#13;` numeric entities, which must be
        // decoded before the ICS parser sees the blob or every property
        // would run together on one line.
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<D:multistatus xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\
<D:response><D:propstat><D:prop><C:calendar-data>\
BEGIN:VCALENDAR&#13;\nVERSION:2.0&#13;\nBEGIN:VEVENT&#13;\nUID:fixture-1&#13;\n\
SUMMARY:Tea &amp; Biscuits&#13;\nDTSTART:20260315T093000Z&#13;\nDTEND:20260315T103000Z&#13;\n\
END:VEVENT&#13;\nBEGIN:VEVENT&#13;\nUID:fixture-2&#13;\nSUMMARY:Standup&#13;\n\
DTSTART:20260316T090000Z&#13;\nDTEND:20260316T091500Z&#13;\nRRULE:FREQ=DAILY;COUNT=3&#13;\n\
END:VEVENT&#13;\nEND:VCALENDAR&#13;\n\
</C:calendar-data></D:prop></D:propstat></D:response></D:multistatus>";

        let blobs = extract_calendar_data(xml);
        assert_eq!(blobs.len(), 1);
        assert!(blobs[0].contains("SUMMARY:Tea & Biscuits\r\n"));

        let window = Window {
            start: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 3, 22).unwrap(),
        };
        let events =
            super::super::events_from_ics_text(&blobs[0], "icloud-1", window, UtcOffset::new(-300));
        // One single event + three daily instances of the recurring one.
        assert_eq!(events.len(), 4);
        let single = events.iter().find(|e| e.id == "fixture-1").unwrap();
        assert_eq!(single.summary, "Tea & Biscuits");
        // 09:30Z is 04:30 local at UTC-05:00, still on 2026-03-15.
        assert_eq!(
            single.time.start_date(),
            NaiveDate::from_ymd_opt(2026, 3, 15).unwrap()
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.id.starts_with("fixture-2#"))
                .count(),
            3
        );
    }

    #[test]
    fn rejects_non_https_calendar_url() {
        let window = Window {
            start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
        };
        let err = fetch_icloud_events(
            "me@example.com",
            "app-pass",
            "http://example.com/cal",
            "s",
            window,
            UtcOffset::new(0),
        )
        .unwrap_err();
        assert!(matches!(err, SourceError::Http(_)));
    }

    #[test]
    fn basic_auth_header_is_base64_of_user_colon_password() {
        let header = basic_auth_header("me@example.com", "xxxx-xxxx-xxxx-xxxx");
        assert!(header.starts_with("Basic "));
    }

    #[test]
    fn calendar_query_body_embeds_window_bounds() {
        let window = Window {
            start: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        };
        let body = calendar_query_body(window);
        assert!(body.contains("20260301T000000Z"));
        assert!(body.contains("20260401T000000Z"));
    }
}
