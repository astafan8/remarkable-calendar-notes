//! Loading, saving, and secret-masking for the whole app config + ink store.
//!
//! There is no required hand-written config file: [`AppState::load`]
//! synthesizes sensible defaults on first run, and every field is editable
//! from the in-app settings/source-editor screen (see `calnotes-app`).

use crate::ink::InkStore;
use crate::model::AppConfig;
use crate::persistence;
use std::io;
use std::path::PathBuf;

const CONFIG_FILE: &str = "config.json";
const INK_FILE: &str = "ink.json";

pub struct AppState {
    pub config: AppConfig,
    pub ink: InkStore,
}

impl AppState {
    /// Load persisted state from the resolved data directory, filling in
    /// defaults for anything missing (including a brand-new install).
    pub fn load() -> io::Result<Self> {
        let dir = persistence::data_dir()?;
        let config = persistence::read_json_opt(&dir.join(CONFIG_FILE))?.unwrap_or_default();
        let ink = persistence::read_json_opt(&dir.join(INK_FILE))?.unwrap_or_default();
        Ok(AppState { config, ink })
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
}
