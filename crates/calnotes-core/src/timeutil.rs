//! Time handling built on a single user-configurable fixed UTC offset.
//!
//! The reMarkable 2 has no reliable timezone database in this app's target
//! environment, so instead of shipping (or trying to keep in sync) an IANA
//! tzdata copy, the app asks the user once for a fixed UTC offset in
//! minutes (e.g. `-300` for US Eastern Standard Time, `60` for CET). All
//! wall-clock display and "today" calculations use that offset. This is an
//! intentional, documented simplification: it does not follow DST
//! transitions automatically. See docs/LIMITATIONS.md.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// A fixed offset from UTC, expressed in whole minutes (e.g. `-300`, `60`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UtcOffset {
    pub minutes: i32,
}

impl UtcOffset {
    pub fn new(minutes: i32) -> Self {
        // Clamp to a sane range; real-world offsets run from -12:00 to +14:00.
        let clamped = minutes.clamp(-14 * 60, 14 * 60);
        UtcOffset { minutes: clamped }
    }

    /// Convert a naive UTC instant into local wall-clock time under this offset.
    pub fn to_local(&self, utc: DateTime<Utc>) -> NaiveDateTime {
        utc.naive_utc() + Duration::minutes(self.minutes as i64)
    }

    /// Convert a naive timestamp that is *known to be UTC* (e.g. an ICS
    /// `DATE-TIME` with a trailing `Z`) into local wall-clock time under
    /// this offset. This can move the value across a date boundary, which
    /// is exactly why it exists as a distinct operation from "treat this
    /// naive value as already-local".
    pub fn utc_naive_to_local(&self, utc: NaiveDateTime) -> NaiveDateTime {
        utc + Duration::minutes(self.minutes as i64)
    }

    /// Convert local wall-clock time back into UTC under this offset.
    pub fn to_utc(&self, local: NaiveDateTime) -> DateTime<Utc> {
        DateTime::<Utc>::from_naive_utc_and_offset(
            local - Duration::minutes(self.minutes as i64),
            Utc,
        )
    }

    /// "Today" in local wall-clock terms, using the real current instant.
    pub fn today(&self) -> NaiveDate {
        self.to_local(Utc::now()).date()
    }

    /// Human readable form such as "+05:30" or "-08:00".
    pub fn label(&self) -> String {
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let abs = self.minutes.abs();
        format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn label_formats_positive_and_negative_offsets() {
        assert_eq!(UtcOffset::new(0).label(), "+00:00");
        assert_eq!(UtcOffset::new(-300).label(), "-05:00");
        assert_eq!(UtcOffset::new(330).label(), "+05:30");
    }

    #[test]
    fn offset_clamps_to_valid_range() {
        assert_eq!(UtcOffset::new(10_000).minutes, 14 * 60);
        assert_eq!(UtcOffset::new(-10_000).minutes, -14 * 60);
    }

    #[test]
    fn round_trips_through_utc_and_local() {
        let offset = UtcOffset::new(-300);
        let utc = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 1, 15)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            Utc,
        );
        let local = offset.to_local(utc);
        assert_eq!(local.hour(), 7);
        let back = offset.to_utc(local);
        assert_eq!(back, utc);
    }

    #[test]
    fn utc_naive_to_local_can_cross_a_date_boundary_backwards() {
        let offset = UtcOffset::new(-300);
        let utc = NaiveDate::from_ymd_opt(2026, 3, 1)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        let local = offset.utc_naive_to_local(utc);
        assert_eq!(local.date(), NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
        assert_eq!(local.hour(), 21);
    }

    #[test]
    fn utc_naive_to_local_can_cross_a_date_boundary_forwards() {
        let offset = UtcOffset::new(330);
        let utc = NaiveDate::from_ymd_opt(2026, 3, 1)
            .unwrap()
            .and_hms_opt(23, 0, 0)
            .unwrap();
        let local = offset.utc_naive_to_local(utc);
        assert_eq!(local.date(), NaiveDate::from_ymd_opt(2026, 3, 2).unwrap());
        assert_eq!(local.hour(), 4);
    }
}
