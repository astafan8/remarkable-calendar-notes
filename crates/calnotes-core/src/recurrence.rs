//! Bounded RRULE recurrence expansion.
//!
//! Supports `FREQ=DAILY|WEEKLY|MONTHLY|YEARLY` with `INTERVAL`, `COUNT`,
//! `UNTIL`, and `BYDAY` (including ordinal forms like `2MO` or `-1FR` for
//! `MONTHLY`). Expansion is always bounded by a caller-supplied display
//! window and a hard instance cap, so an unbounded rule (no `COUNT`/`UNTIL`)
//! can never loop forever or blow up memory — a requirement for something
//! running on a resource-constrained e-reader.

use crate::ics::{ByDay, Freq, IcsDateTime, RawVEvent};
use crate::model::{Event, EventTime};
use crate::timeutil::UtcOffset;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};

/// Half-open date window: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

/// Safety valve: no single event expands past this many instances,
/// regardless of window size or rule.
const MAX_INSTANCES: usize = 2000;

/// One expanded occurrence: the local start date/time and duration,
/// carried forward from the master event's DTSTART/DTEND.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub start: IcsDateTime,
}

fn add_months_year_month(date: NaiveDate, months: i32) -> (i32, u32) {
    let total = date.year() * 12 + (date.month() as i32 - 1) + months;
    let year = total.div_euclid(12);
    let month = (total.rem_euclid(12) + 1) as u32;
    (year, month)
}

fn nth_weekday_of_month(
    year: i32,
    month: u32,
    weekday: Weekday,
    ordinal: i32,
) -> Option<NaiveDate> {
    if ordinal == 0 {
        return None;
    }
    if ordinal > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let first_offset = (7 + weekday.num_days_from_monday() as i64
            - first.weekday().num_days_from_monday() as i64)
            % 7;
        let day = 1 + first_offset + (ordinal as i64 - 1) * 7;
        NaiveDate::from_ymd_opt(year, month, day.try_into().ok()?)
    } else {
        let next_month_first = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = next_month_first.pred_opt()?;
        let last_offset = (7 + last.weekday().num_days_from_monday() as i64
            - weekday.num_days_from_monday() as i64)
            % 7;
        let day = last.day() as i64 - last_offset - (ordinal.unsigned_abs() as i64 - 1) * 7;
        if day < 1 {
            return None;
        }
        NaiveDate::from_ymd_opt(year, month, day as u32)
    }
}

/// Every date in `year`-`month` falling on `weekday` — the RFC 5545
/// meaning of an ordinal-less `BYDAY` entry under `FREQ=MONTHLY`
/// (e.g. `BYDAY=MO` means *every* Monday of the month, not the first one).
fn all_weekdays_of_month(year: i32, month: u32, weekday: Weekday) -> Vec<NaiveDate> {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return Vec::new();
    };
    let offset = (7 + weekday.num_days_from_monday() as i64
        - first.weekday().num_days_from_monday() as i64)
        % 7;
    let mut out = Vec::new();
    let mut d = first + Duration::days(offset);
    while d.month() == month && d.year() == year {
        out.push(d);
        d += Duration::days(7);
    }
    out
}

/// Replace the date portion of `at` with `date`, keeping the master event's
/// time-of-day (or produce a plain `Date` if the master was all-day).
fn retime(master: IcsDateTime, date: NaiveDate) -> IcsDateTime {
    match master {
        IcsDateTime::Date(_) => IcsDateTime::Date(date),
        IcsDateTime::DateTime { at, is_utc } => IcsDateTime::DateTime {
            at: NaiveDateTime::new(date, at.time()),
            is_utc,
        },
    }
}

fn until_bound(rrule_until: Option<IcsDateTime>) -> Option<NaiveDate> {
    rrule_until.map(|u| u.date())
}

/// Expand `raw.rrule` (or the single occurrence, if there is none) into
/// dates that fall within `window`, honoring `COUNT`/`UNTIL` against the
/// *unbounded* occurrence sequence (i.e. counting/until-checking happens
/// before the window filter, matching RFC 5545 semantics) and `EXDATE`.
pub fn expand_occurrences(raw: &RawVEvent, window: Window) -> Vec<Occurrence> {
    let mut out = Vec::new();
    let start_date = raw.dtstart.date();
    let exdates: Vec<NaiveDate> = raw.exdates.iter().map(|d| d.date()).collect();

    let Some(rrule) = &raw.rrule else {
        // Non-recurring: always return the single occurrence. Window
        // filtering against the event's *full* span (which may extend past
        // its start date, e.g. a multi-day all-day event) happens in the
        // higher-level `build_events` once the duration is known.
        out.push(Occurrence { start: raw.dtstart });
        return out;
    };

    let until = until_bound(rrule.until);
    let mut produced = 0u32;
    let count_ok = |d: NaiveDate, produced: u32| -> bool {
        if let Some(u) = until {
            if d > u {
                return false;
            }
        }
        if let Some(c) = rrule.count {
            if produced >= c {
                return false;
            }
        }
        true
    };

    match rrule.freq {
        Freq::Daily => {
            let interval = rrule.interval.max(1) as i64;
            if let Some(c) = rrule.count {
                // COUNT bounds the total occurrence count directly, so a
                // linear walk from DTSTART is always cheap regardless of
                // how far in the past DTSTART is.
                let mut d = start_date;
                for _ in 0..c {
                    if let Some(u) = until {
                        if d > u {
                            break;
                        }
                    }
                    if d >= window.start && d < window.end && !exdates.contains(&d) {
                        out.push(Occurrence {
                            start: retime(raw.dtstart, d),
                        });
                    }
                    d += Duration::days(interval);
                }
            } else {
                // Unbounded or UNTIL-only: an old DTSTART could be many
                // thousands of days before the window, so jump straight to
                // the first on-cadence date at or after window.start instead
                // of stepping one day at a time from DTSTART.
                let mut d = start_date;
                if window.start > start_date {
                    let gap_days = (window.start - start_date).num_days();
                    let steps = gap_days / interval;
                    d = start_date + Duration::days(steps * interval);
                }
                let mut iterations = 0usize;
                while d < window.end && iterations < MAX_INSTANCES {
                    iterations += 1;
                    if let Some(u) = until {
                        if d > u {
                            break;
                        }
                    }
                    if d >= window.start && !exdates.contains(&d) {
                        out.push(Occurrence {
                            start: retime(raw.dtstart, d),
                        });
                    }
                    d += Duration::days(interval);
                }
            }
        }
        Freq::Weekly => {
            let by_day = if rrule.by_day.is_empty() {
                vec![ByDay {
                    ordinal: None,
                    weekday: start_date.weekday(),
                }]
            } else {
                rrule.by_day.clone()
            };
            // Walk week-by-week from the week containing DTSTART.
            let week_start =
                start_date - Duration::days(start_date.weekday().num_days_from_monday() as i64);
            let mut week = week_start;
            let mut iterations = 0usize;
            'weeks: loop {
                iterations += 1;
                if iterations > MAX_INSTANCES {
                    break;
                }
                let mut days_this_week: Vec<NaiveDate> = by_day
                    .iter()
                    .filter_map(|bd| {
                        let offset = bd.weekday.num_days_from_monday() as i64;
                        let d = week + Duration::days(offset);
                        (d >= start_date).then_some(d)
                    })
                    .collect();
                days_this_week.sort();
                for d in days_this_week {
                    if !count_ok(d, produced) {
                        break 'weeks;
                    }
                    if d >= window.start && d < window.end && !exdates.contains(&d) {
                        out.push(Occurrence {
                            start: retime(raw.dtstart, d),
                        });
                    }
                    produced += 1;
                }
                week += Duration::weeks(rrule.interval as i64);
                if week > window.end && (rrule.until.is_none() || week > until.unwrap()) {
                    break;
                }
                if out.len() >= MAX_INSTANCES {
                    break;
                }
            }
        }
        Freq::Monthly => {
            let mut month_index = 0i32;
            let mut iterations = 0usize;
            loop {
                iterations += 1;
                if iterations > MAX_INSTANCES {
                    break;
                }
                let (year, month) =
                    add_months_year_month(start_date, month_index * rrule.interval as i32);
                // First of the target month, used only for window/until
                // boundary comparisons — the actual candidate dates below
                // are computed independently and may not include day 1.
                let Some(month_start) = NaiveDate::from_ymd_opt(year, month, 1) else {
                    break;
                };
                let candidates: Vec<NaiveDate> = if rrule.by_day.is_empty() {
                    // Same day-of-month as DTSTART; RFC 5545 says a target
                    // month that doesn't have that day (e.g. day 30 in
                    // February) simply produces no occurrence that month,
                    // rather than being an error.
                    NaiveDate::from_ymd_opt(year, month, start_date.day())
                        .into_iter()
                        .collect()
                } else {
                    rrule
                        .by_day
                        .iter()
                        .flat_map(|bd| match bd.ordinal {
                            Some(n) => nth_weekday_of_month(year, month, bd.weekday, n)
                                .into_iter()
                                .collect::<Vec<_>>(),
                            // No ordinal under FREQ=MONTHLY means every
                            // matching weekday in the month (RFC 5545 §3.3.10).
                            None => all_weekdays_of_month(year, month, bd.weekday),
                        })
                        .collect()
                };
                let mut candidates = candidates;
                candidates.sort();
                let mut stop = false;
                for d in candidates {
                    if d < start_date {
                        continue;
                    }
                    if !count_ok(d, produced) {
                        stop = true;
                        break;
                    }
                    if d >= window.start && d < window.end && !exdates.contains(&d) {
                        out.push(Occurrence {
                            start: retime(raw.dtstart, d),
                        });
                    }
                    produced += 1;
                }
                if stop {
                    break;
                }
                if month_start > window.end
                    && (rrule.until.is_none() || month_start > until.unwrap())
                {
                    break;
                }
                month_index += 1;
                if out.len() >= MAX_INSTANCES {
                    break;
                }
            }
        }
        Freq::Yearly => {
            let mut year_index = 0i32;
            let mut iterations = 0usize;
            loop {
                iterations += 1;
                if iterations > MAX_INSTANCES {
                    break;
                }
                let target_year = start_date.year() + year_index * rrule.interval as i32;
                let Some(d) =
                    NaiveDate::from_ymd_opt(target_year, start_date.month(), start_date.day())
                else {
                    year_index += 1;
                    continue;
                };
                if d < start_date {
                    year_index += 1;
                    continue;
                }
                if !count_ok(d, produced) {
                    break;
                }
                if d >= window.start && d < window.end && !exdates.contains(&d) {
                    out.push(Occurrence {
                        start: retime(raw.dtstart, d),
                    });
                }
                produced += 1;
                if d > window.end && (rrule.until.is_none() || d > until.unwrap()) {
                    break;
                }
                year_index += 1;
                if out.len() >= MAX_INSTANCES {
                    break;
                }
            }
        }
    }

    out
}

/// Compute the master event's (all-day day-count or timed) duration.
fn duration_of(raw: &RawVEvent) -> EventDuration {
    match (raw.dtstart, raw.dtend) {
        (IcsDateTime::Date(start), Some(IcsDateTime::Date(end))) => {
            EventDuration::AllDayDays((end - start).num_days().max(1))
        }
        (IcsDateTime::Date(_), _) => EventDuration::AllDayDays(1),
        (IcsDateTime::DateTime { at: start, .. }, Some(IcsDateTime::DateTime { at: end, .. })) => {
            EventDuration::Timed(end - start)
        }
        (IcsDateTime::DateTime { .. }, _) => EventDuration::Timed(Duration::zero()),
    }
}

enum EventDuration {
    AllDayDays(i64),
    Timed(Duration),
}

/// Expand `raw` and convert each in-window occurrence into a display-ready
/// [`Event`], filtering by full event-span overlap with `window` (not just
/// occurrence start), so multi-day/all-day events that begin before the
/// window but extend into it are still included.
///
/// `offset` is the app's configured fixed UTC offset. Occurrences whose
/// `DATE-TIME` carried a trailing `Z` are genuine UTC instants and are
/// converted into local wall-clock time with it (which can move them onto a
/// different calendar date); floating/`TZID` values are taken as already
/// local, per the documented simplification in `crate::ics`.
pub fn build_events(
    raw: &RawVEvent,
    source_id: &str,
    window: Window,
    offset: UtcOffset,
) -> Vec<Event> {
    let duration = duration_of(raw);
    // Widen the occurrence-generation window backwards so a long event
    // starting well before `window.start` is still generated, and by a day
    // on each side so a UTC->local shift across midnight cannot drop an
    // occurrence that belongs on screen. RRULEs are still hard-capped by
    // MAX_INSTANCES regardless, and every generated occurrence is filtered
    // against the real `window` below.
    let lookback_days = match duration {
        EventDuration::AllDayDays(d) => d,
        EventDuration::Timed(_) => 1,
    };
    let gen_window = Window {
        start: window.start - Duration::days(lookback_days + 1),
        end: window.end + Duration::days(1),
    };

    expand_occurrences(raw, gen_window)
        .into_iter()
        .filter_map(|occ| {
            let time = match (occ.start, &duration) {
                (IcsDateTime::Date(d), EventDuration::AllDayDays(days)) => EventTime::AllDay {
                    start: d,
                    end_exclusive: d + Duration::days(*days),
                },
                (IcsDateTime::DateTime { at, is_utc }, EventDuration::Timed(dur)) => {
                    let start = if is_utc {
                        offset.utc_naive_to_local(at)
                    } else {
                        at
                    };
                    EventTime::Timed {
                        start,
                        end: start + *dur,
                    }
                }
                _ => return None,
            };
            let overlaps =
                time.start_date() < window.end && time.end_date_inclusive() >= window.start;
            if !overlaps {
                return None;
            }
            let id = if raw.rrule.is_some() {
                format!("{}#{}", raw.uid, occurrence_key(&occ.start))
            } else {
                raw.uid.clone()
            };
            Some(Event {
                id,
                source_id: source_id.to_string(),
                summary: raw.summary.clone(),
                location: raw.location.clone(),
                time,
            })
        })
        .collect()
}

fn occurrence_key(at: &IcsDateTime) -> String {
    match at {
        IcsDateTime::Date(d) => d.format("%Y%m%d").to_string(),
        IcsDateTime::DateTime { at, .. } => at.format("%Y%m%dT%H%M%S").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ics::parse_calendar;

    fn window(y1: i32, m1: u32, d1: u32, y2: i32, m2: u32, d2: u32) -> Window {
        Window {
            start: NaiveDate::from_ymd_opt(y1, m1, d1).unwrap(),
            end: NaiveDate::from_ymd_opt(y2, m2, d2).unwrap(),
        }
    }

    fn utc() -> UtcOffset {
        UtcOffset::new(0)
    }

    #[test]
    fn daily_with_count_stops_after_count() {
        let ics = "BEGIN:VEVENT\r\nUID:d1\r\nDTSTART:20260101\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2026, 2, 1));
        assert_eq!(occ.len(), 3);
        assert_eq!(
            occ[2].start.date(),
            NaiveDate::from_ymd_opt(2026, 1, 3).unwrap()
        );
    }

    #[test]
    fn weekly_byday_expands_each_matching_weekday() {
        let ics = "BEGIN:VEVENT\r\nUID:w1\r\nDTSTART:20260105T090000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=6\r\nEND:VEVENT\r\n";
        // 2026-01-05 is a Monday.
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2026, 2, 1));
        assert_eq!(occ.len(), 6);
        let dates: Vec<_> = occ.iter().map(|o| o.start.date()).collect();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 9).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 14).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 16).unwrap(),
            ]
        );
    }

    #[test]
    fn monthly_same_day_of_month_with_interval() {
        let ics = "BEGIN:VEVENT\r\nUID:m1\r\nDTSTART:20260115\r\nRRULE:FREQ=MONTHLY;INTERVAL=2;COUNT=3\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2027, 1, 1));
        let dates: Vec<_> = occ.iter().map(|o| o.start.date()).collect();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
            ]
        );
    }

    #[test]
    fn monthly_last_friday_of_month() {
        let ics = "BEGIN:VEVENT\r\nUID:m2\r\nDTSTART:20260130\r\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=3\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2026, 6, 1));
        let dates: Vec<_> = occ.iter().map(|o| o.start.date()).collect();
        // Last Fridays of Jan, Feb, Mar 2026.
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 1, 30).unwrap(),
                NaiveDate::from_ymd_opt(2026, 2, 27).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 27).unwrap(),
            ]
        );
    }

    #[test]
    fn yearly_anniversary_respects_until() {
        let ics = "BEGIN:VEVENT\r\nUID:y1\r\nDTSTART:20240229\r\nRRULE:FREQ=YEARLY;UNTIL=20300101T000000Z\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2024, 1, 1, 2029, 1, 1));
        // Leap day anniversary only recurs on leap years within this window.
        assert!(occ.iter().all(|o| o.start.date().month() == 2));
    }

    #[test]
    fn exdate_removes_a_specific_occurrence() {
        let ics = "BEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20260101\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20260102\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2026, 2, 1));
        let dates: Vec<_> = occ.iter().map(|o| o.start.date()).collect();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(),
            ]
        );
    }

    #[test]
    fn unbounded_rule_is_still_capped_by_window() {
        let ics =
            "BEGIN:VEVENT\r\nUID:u1\r\nDTSTART:20200101\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2026, 1, 8));
        assert_eq!(occ.len(), 7);
    }

    #[test]
    fn build_events_excludes_non_recurring_event_outside_window() {
        let ics = "BEGIN:VEVENT\r\nUID:n1\r\nDTSTART:20200101\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let built = build_events(&events[0], "src", window(2026, 1, 1, 2026, 1, 8), utc());
        assert!(built.is_empty());
    }

    #[test]
    fn build_events_includes_multiday_all_day_event_overlapping_window_start() {
        let ics = "BEGIN:VEVENT\r\nUID:md1\r\nSUMMARY:Trip\r\nDTSTART;VALUE=DATE:20251230\r\nDTEND;VALUE=DATE:20260103\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let built = build_events(&events[0], "src", window(2026, 1, 1, 2026, 1, 8), utc());
        assert_eq!(built.len(), 1);
        assert_eq!(
            built[0].time.start_date(),
            NaiveDate::from_ymd_opt(2025, 12, 30).unwrap()
        );
    }

    #[test]
    fn build_events_expands_recurring_timed_event_with_correct_duration() {
        let ics = "BEGIN:VEVENT\r\nUID:t1\r\nSUMMARY:Standup\r\nDTSTART:20260105T090000\r\nDTEND:20260105T091500\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let built = build_events(&events[0], "src", window(2026, 1, 1, 2026, 1, 8), utc());
        assert_eq!(built.len(), 2);
        if let EventTime::Timed { start, end } = built[0].time {
            assert_eq!((end - start).num_minutes(), 15);
        } else {
            panic!("expected Timed");
        }
        assert_eq!(built[0].id, "t1#20260105T090000");
    }

    #[test]
    fn monthly_byday_without_ordinal_emits_every_matching_weekday() {
        let ics = "BEGIN:VEVENT\r\nUID:m3\r\nDTSTART:20260105\r\nRRULE:FREQ=MONTHLY;BYDAY=MO\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        // January 2026 Mondays: 5, 12, 19, 26 (DTSTART is the 5th).
        let occ = expand_occurrences(&events[0], window(2026, 1, 1, 2026, 2, 1));
        let dates: Vec<_> = occ.iter().map(|o| o.start.date()).collect();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 19).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 26).unwrap(),
            ]
        );
    }

    #[test]
    fn monthly_byday_without_ordinal_handles_multiple_weekdays() {
        let ics = "BEGIN:VEVENT\r\nUID:m4\r\nDTSTART:20260302\r\nRRULE:FREQ=MONTHLY;BYDAY=MO,FR;COUNT=4\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let occ = expand_occurrences(&events[0], window(2026, 3, 1, 2026, 4, 1));
        let dates: Vec<_> = occ.iter().map(|o| o.start.date()).collect();
        // March 2026 starts on a Sunday: Mon 2, Fri 6, Mon 9, Fri 13, ...
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 6).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 9).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 13).unwrap(),
            ]
        );
    }

    #[test]
    fn utc_ics_datetime_is_converted_into_the_configured_local_offset() {
        let ics = "BEGIN:VEVENT\r\nUID:z1\r\nSUMMARY:Call\r\nDTSTART:20260301T233000Z\r\nDTEND:20260302T003000Z\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let built = build_events(
            &events[0],
            "src",
            window(2026, 3, 1, 2026, 3, 8),
            UtcOffset::new(-300),
        );
        assert_eq!(built.len(), 1);
        match built[0].time {
            EventTime::Timed { start, end } => {
                // 23:30Z on Mar 1 is 18:30 local on Mar 1 at UTC-05:00.
                assert_eq!(start.date(), NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
                assert_eq!(start.format("%H:%M").to_string(), "18:30");
                assert_eq!((end - start).num_minutes(), 60);
            }
            _ => panic!("expected Timed"),
        }
    }

    #[test]
    fn utc_ics_datetime_crossing_midnight_lands_on_the_previous_local_date() {
        let ics = "BEGIN:VEVENT\r\nUID:z2\r\nSUMMARY:Night\r\nDTSTART:20260301T023000Z\r\nDTEND:20260301T033000Z\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        // The event only appears in the *February* window once converted.
        let built = build_events(
            &events[0],
            "src",
            window(2026, 2, 22, 2026, 3, 1),
            UtcOffset::new(-300),
        );
        assert_eq!(built.len(), 1);
        assert_eq!(
            built[0].time.start_date(),
            NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
        );
    }

    #[test]
    fn utc_ics_datetime_crossing_midnight_forwards_lands_on_the_next_local_date() {
        let ics = "BEGIN:VEVENT\r\nUID:z3\r\nSUMMARY:Morning\r\nDTSTART:20260301T230000Z\r\nDTEND:20260302T000000Z\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let built = build_events(
            &events[0],
            "src",
            window(2026, 3, 2, 2026, 3, 9),
            UtcOffset::new(330),
        );
        assert_eq!(built.len(), 1);
        assert_eq!(
            built[0].time.start_date(),
            NaiveDate::from_ymd_opt(2026, 3, 2).unwrap()
        );
    }

    #[test]
    fn floating_ics_datetime_is_left_as_local_wall_clock() {
        let ics = "BEGIN:VEVENT\r\nUID:f1\r\nSUMMARY:Local\r\nDTSTART:20260301T233000\r\nDTEND:20260302T003000\r\nEND:VEVENT\r\n";
        let (events, _) = parse_calendar(ics);
        let built = build_events(
            &events[0],
            "src",
            window(2026, 3, 1, 2026, 3, 8),
            UtcOffset::new(-300),
        );
        assert_eq!(built.len(), 1);
        match built[0].time {
            EventTime::Timed { start, .. } => {
                assert_eq!(start.format("%H:%M").to_string(), "23:30");
                assert_eq!(start.date(), NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
            }
            _ => panic!("expected Timed"),
        }
    }
}
