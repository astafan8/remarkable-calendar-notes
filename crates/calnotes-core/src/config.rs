//! Loading, saving, and secret-masking for the whole app config + ink store.
//!
//! There is no required hand-written config file: [`AppState::load`]
//! synthesizes sensible defaults on first run, and every field is editable
//! from the in-app settings/source-editor screen (see `calnotes-app`).

use crate::ink::{DayNotes, InkStore};
use crate::model::AppConfig;
use crate::persistence;
use chrono::NaiveDate;
use serde::de::DeserializeOwned;
use std::io;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "config.json";
/// Legacy single-file ink store, migrated to the per-day directory on load.
const INK_FILE: &str = "ink.json";
/// Per-day ink store: `ink/<YYYY-MM-DD>.json`. Splitting the store one file
/// per date means a single stroke only rewrites that day's (small) file
/// instead of re-serializing every stroke ever drawn, which is what made the
/// app slow down as notes accumulated.
const INK_DIR: &str = "ink";

fn day_file_name(date: NaiveDate) -> String {
    format!("{date}.json")
}

pub struct AppState {
    pub config: AppConfig,
    pub ink: InkStore,
    /// Non-fatal problems encountered while loading persisted state (for
    /// example a corrupt `config.json` that was reset to defaults). The app
    /// logs these and surfaces a short hint on screen so a bad locally
    /// stored file is visible rather than silently swallowed — and never
    /// blocks startup.
    pub load_warnings: Vec<String>,
}

impl AppState {
    /// Load persisted state from the resolved data directory, filling in
    /// defaults for anything missing (including a brand-new install).
    ///
    /// This never fails because of a corrupt or schema-incompatible file:
    /// such a file is moved aside and the affected section is reset to
    /// defaults, with a warning recorded in [`AppState::load_warnings`].
    /// Guaranteeing a successful load is what keeps a bad locally stored
    /// config/ink file from turning into a permanently blank screen.
    pub fn load() -> io::Result<Self> {
        let dir = match persistence::data_dir() {
            Ok(dir) => dir,
            Err(e) => {
                // Even a missing/unwritable data directory must not stop the
                // app from starting: fall back to in-memory defaults.
                return Ok(AppState {
                    config: AppConfig::default(),
                    ink: InkStore::default(),
                    load_warnings: vec![format!(
                        "data directory unavailable ({e}); using in-memory defaults"
                    )],
                });
            }
        };
        let mut load_warnings = Vec::new();
        let config = load_section(&dir.join(CONFIG_FILE), CONFIG_FILE, &mut load_warnings);
        let ink = load_ink(&dir, &mut load_warnings);
        Ok(AppState {
            config,
            ink,
            load_warnings,
        })
    }

    pub fn save_config(&self) -> io::Result<()> {
        let dir = persistence::data_dir()?;
        persistence::write_json_atomic(&dir.join(CONFIG_FILE), &self.config)
    }

    /// Persist just one date's notes — the hot path called on every pen-up,
    /// erase, lasso, clear, and undo. Writing a single day's file keeps the
    /// cost independent of how much ink exists across all other dates. An
    /// empty day removes its file.
    pub fn save_ink_day(&self, date: NaiveDate) -> io::Result<()> {
        let dir = persistence::data_dir()?.join(INK_DIR);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(day_file_name(date));
        match self.ink.days.get(&date) {
            Some(notes) if !notes.strokes.is_empty() => {
                persistence::write_json_atomic(&path, notes)
            }
            _ => match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            },
        }
    }

    /// Persist the whole ink store as per-day files, pruning files for dates
    /// that no longer have any ink. Used by tests and any bulk save; the
    /// per-stroke hot path uses [`AppState::save_ink_day`] instead.
    pub fn save_ink(&self) -> io::Result<()> {
        let dir = persistence::data_dir()?.join(INK_DIR);
        std::fs::create_dir_all(&dir)?;
        // Remove day files whose date is gone or empty.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let keep = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok())
                    .is_some_and(|date| {
                        self.ink
                            .days
                            .get(&date)
                            .is_some_and(|n| !n.strokes.is_empty())
                    });
                if !keep {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        for (date, notes) in &self.ink.days {
            if !notes.strokes.is_empty() {
                persistence::write_json_atomic(&dir.join(day_file_name(*date)), notes)?;
            }
        }
        Ok(())
    }

    /// Where per-source offline event caches live:
    /// `<data_dir>/cache/<source_id>.json`.
    pub fn cache_path(source_id: &str) -> io::Result<PathBuf> {
        let dir = persistence::data_dir()?.join("cache");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{source_id}.json")))
    }
}

/// Load one JSON section (config or ink), recovering to `Default` if the
/// file is missing, corrupt, or unreadable, and appending a human-readable
/// warning for anything worse than "missing".
fn load_section<T: DeserializeOwned + Default>(
    path: &Path,
    name: &str,
    warnings: &mut Vec<String>,
) -> T {
    match persistence::read_json_recovering::<T>(path) {
        Ok(recovered) => {
            if let Some(error) = recovered.error {
                let backup = recovered
                    .recovered_from
                    .unwrap_or_else(|| "not preserved".to_string());
                warnings.push(format!(
                    "{name} could not be read ({error}); reset to defaults, previous file kept at {backup}"
                ));
            }
            recovered.value.unwrap_or_default()
        }
        Err(e) => {
            warnings.push(format!(
                "{name} could not be accessed ({e}); using defaults"
            ));
            T::default()
        }
    }
}

/// Load the ink store from the per-day directory, first migrating a legacy
/// single-file `ink.json` (from older versions) into per-day files. Missing
/// or corrupt individual day files are skipped with a warning rather than
/// failing the whole load.
fn load_ink(dir: &Path, warnings: &mut Vec<String>) -> InkStore {
    let ink_dir = dir.join(INK_DIR);

    // One-time migration: fold a legacy single-file store into per-day files,
    // then retire the old file so it is not re-read next launch.
    let legacy_path = dir.join(INK_FILE);
    if legacy_path.exists() {
        let legacy: InkStore = load_section(&legacy_path, INK_FILE, warnings);
        let _ = std::fs::create_dir_all(&ink_dir);
        for (date, notes) in &legacy.days {
            if !notes.strokes.is_empty() {
                let _ = persistence::write_json_atomic(&ink_dir.join(day_file_name(*date)), notes);
            }
        }
        let _ = std::fs::rename(&legacy_path, legacy_path.with_extension("json.migrated"));
    }

    let mut store = InkStore::default();
    let entries = match std::fs::read_dir(&ink_dir) {
        Ok(entries) => entries,
        // No ink yet (fresh install) — an empty store is correct.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return store,
        Err(e) => {
            warnings.push(format!("{INK_DIR} could not be listed ({e}); using no ink"));
            return store;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(date) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok())
        else {
            continue;
        };
        match persistence::read_json_recovering::<DayNotes>(&path) {
            Ok(recovered) => {
                if let Some(error) = recovered.error {
                    let backup = recovered
                        .recovered_from
                        .unwrap_or_else(|| "not preserved".to_string());
                    warnings.push(format!(
                        "ink for {date} could not be read ({error}); skipped, previous file kept at {backup}"
                    ));
                }
                if let Some(notes) = recovered.value {
                    if !notes.strokes.is_empty() {
                        store.days.insert(date, notes);
                    }
                }
            }
            Err(e) => warnings.push(format!(
                "ink for {date} could not be accessed ({e}); skipped"
            )),
        }
    }
    store
}

/// Mask a secret (password, token, client secret) for display in the
/// settings UI: keep the first and last character (if long enough) and
/// replace the middle with bullets, so a user can visually confirm they
/// typed *something* plausible without the full value ever being shown or
/// screenshotted. Short secrets are fully masked.
///
/// This is a *display* masking only — see docs/SECURITY.md for the
/// plaintext-at-rest limitation of the persisted JSON files themselves.
pub fn mask_secret(secret: &str) -> String {
    let len = secret.chars().count();
    if len == 0 {
        return String::new();
    }
    if len <= 4 {
        return "\u{2022}".repeat(len);
    }
    let chars: Vec<char> = secret.chars().collect();
    let mut out = String::new();
    out.push(chars[0]);
    out.push_str(&"\u{2022}".repeat(len - 2));
    out.push(chars[len - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_secret_hides_the_middle_of_a_long_secret() {
        let masked = mask_secret("hunter2-app-specific-password");
        assert!(masked.starts_with('h'));
        assert!(masked.ends_with('d'));
        assert!(!masked.contains("hunter2"));
    }

    #[test]
    fn mask_secret_fully_hides_short_secrets() {
        assert_eq!(mask_secret("ab"), "\u{2022}\u{2022}");
        assert_eq!(mask_secret(""), "");
    }

    #[test]
    #[serial_test::serial]
    fn app_state_first_run_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let state = AppState::load().unwrap();
        assert!(state.config.sources.is_empty());
        assert!(state.ink.days.is_empty());
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn app_state_round_trips_config_and_ink_through_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let mut state = AppState::load().unwrap();
        state.config.utc_offset_minutes = -300;
        state
            .ink
            .begin_stroke(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        state.save_config().unwrap();
        state.save_ink().unwrap();

        let reloaded = AppState::load().unwrap();
        assert_eq!(reloaded.config.utc_offset_minutes, -300);
        assert_eq!(reloaded.ink.days.len(), 1);
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn save_ink_day_writes_one_file_per_date_and_prunes_empty_days() {
        use crate::ink::NormPoint;
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());

        let day = chrono::NaiveDate::from_ymd_opt(2026, 5, 4).unwrap();
        let other = chrono::NaiveDate::from_ymd_opt(2026, 5, 5).unwrap();
        let mut state = AppState::load().unwrap();
        for date in [day, other] {
            let idx = state.ink.begin_stroke(date);
            for (x, y) in [(0.1, 0.1), (0.2, 0.2)] {
                state.ink.push_point(
                    date,
                    idx,
                    NormPoint {
                        x,
                        y,
                        pressure: 1.0,
                    },
                );
            }
            state.save_ink_day(date).unwrap();
        }

        // Exactly one file per date under ink/.
        let ink_dir = dir.path().join(INK_DIR);
        assert!(ink_dir.join("2026-05-04.json").exists());
        assert!(ink_dir.join("2026-05-05.json").exists());

        // Reload sees both days.
        let reloaded = AppState::load().unwrap();
        assert_eq!(reloaded.ink.days.len(), 2);

        // Clearing a day removes just its file, leaving the other intact.
        let mut state = reloaded;
        state.ink.clear_day(day);
        state.save_ink_day(day).unwrap();
        assert!(!ink_dir.join("2026-05-04.json").exists());
        assert!(ink_dir.join("2026-05-05.json").exists());
        assert_eq!(AppState::load().unwrap().ink.days.len(), 1);

        std::env::remove_var(persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn legacy_single_file_ink_is_migrated_to_per_day_files() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        // A legacy ink.json written by an older version.
        std::fs::write(
            dir.path().join(INK_FILE),
            r#"{
                "days": {
                    "2026-06-01": {"strokes": [{"points": [
                        {"x": 0.1, "y": 0.2, "pressure": 1.0},
                        {"x": 0.3, "y": 0.4, "pressure": 1.0}
                    ]}]},
                    "2026-06-02": {"strokes": [{"points": [
                        {"x": 0.5, "y": 0.6, "pressure": 1.0},
                        {"x": 0.7, "y": 0.8, "pressure": 1.0}
                    ]}]}
                }
            }"#,
        )
        .unwrap();

        let state = AppState::load().unwrap();
        assert_eq!(state.ink.days.len(), 2);

        // The per-day files now exist and the legacy file has been retired.
        let ink_dir = dir.path().join(INK_DIR);
        assert!(ink_dir.join("2026-06-01.json").exists());
        assert!(ink_dir.join("2026-06-02.json").exists());
        assert!(!dir.path().join(INK_FILE).exists());
        assert!(dir.path().join("ink.json.migrated").exists());

        // A second load (no legacy file) still sees the migrated ink.
        assert_eq!(AppState::load().unwrap().ink.days.len(), 2);
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn app_state_loads_v0_1_3_persisted_json() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            r#"{
                "utc_offset_minutes": 0,
                "view_mode": "Month",
                "sources": [],
                "anchor_date": "2026-08-12"
            }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(INK_FILE),
            r#"{
                "days": {
                    "2026-08-12": {
                        "strokes": [{
                            "points": [
                                {"x": 0.1, "y": 0.2, "pressure": 1.0},
                                {"x": 0.3, "y": 0.4, "pressure": 0.5}
                            ]
                        }]
                    }
                }
            }"#,
        )
        .unwrap();

        let state = AppState::load().unwrap();
        assert_eq!(state.config.view_mode, crate::model::ViewMode::Month);
        assert_eq!(state.ink.days.len(), 1);
        assert_eq!(
            state
                .ink
                .strokes_for(chrono::NaiveDate::from_ymd_opt(2026, 8, 12).unwrap())
                .len(),
            1
        );
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }

    #[test]
    #[serial_test::serial]
    fn corrupt_persisted_state_never_blocks_startup_and_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        // A config that was truncated/garbled by an interrupted write or an
        // incompatible version — the exact situation that used to fail load
        // and leave a blank screen.
        std::fs::write(dir.path().join(CONFIG_FILE), "{ \"view_mode\": ").unwrap();
        std::fs::write(dir.path().join(INK_FILE), "not json at all").unwrap();

        let state = AppState::load().unwrap();

        // Load succeeds with defaults instead of erroring.
        assert_eq!(state.config, crate::model::AppConfig::default());
        assert!(state.ink.days.is_empty());
        // Two warnings: one per corrupt file, each pointing at the backup.
        assert_eq!(state.load_warnings.len(), 2);
        assert!(state
            .load_warnings
            .iter()
            .any(|w| w.contains(CONFIG_FILE) && w.contains("reset to defaults")));

        // The bad files are moved aside so the next launch can't refail.
        assert!(!dir.path().join(CONFIG_FILE).exists());
        assert!(!dir.path().join(INK_FILE).exists());
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".corrupt-")
            })
            .count();
        assert_eq!(backups, 2);
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }
}
