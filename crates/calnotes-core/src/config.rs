//! Loading, saving, and secret-masking for the whole app config + ink store.
//!
//! There is no required hand-written config file: [`AppState::load`]
//! synthesizes sensible defaults on first run, and every field is editable
//! from the in-app settings/source-editor screen (see `calnotes-app`).

use crate::ink::InkStore;
use crate::model::AppConfig;
use crate::persistence;
use serde::de::DeserializeOwned;
use std::io;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "config.json";
const INK_FILE: &str = "ink.json";

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
        let ink = load_section(&dir.join(INK_FILE), INK_FILE, &mut load_warnings);
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

    pub fn save_ink(&self) -> io::Result<()> {
        let dir = persistence::data_dir()?;
        persistence::write_json_atomic(&dir.join(INK_FILE), &self.ink)
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
