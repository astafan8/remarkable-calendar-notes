//! Offline event cache: the last successfully fetched events for a source,
//! persisted so the app has something to show (with a visible "stale"
//! status) even with no network connectivity at all.

use crate::config::AppState;
use crate::model::Event;
use crate::persistence;

pub fn save_cache(source_id: &str, events: &[Event]) -> std::io::Result<()> {
    let path = AppState::cache_path(source_id)?;
    persistence::write_json_atomic(&path, &events.to_vec())
}

pub fn load_cache(source_id: &str) -> std::io::Result<Vec<Event>> {
    let path = AppState::cache_path(source_id)?;
    Ok(persistence::read_json_opt(&path)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EventTime;
    use chrono::NaiveDate;

    #[test]
    #[serial_test::serial]
    fn cache_round_trips_events() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let events = vec![Event {
            id: "e1".into(),
            source_id: "src".into(),
            summary: "Test".into(),
            location: None,
            time: EventTime::AllDay {
                start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            },
        }];
        save_cache("src", &events).unwrap();
        let restored = load_cache("src").unwrap();
        assert_eq!(restored, events);
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn cache_for_unknown_source_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let restored = load_cache("never-seen").unwrap();
        assert!(restored.is_empty());
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }
}
