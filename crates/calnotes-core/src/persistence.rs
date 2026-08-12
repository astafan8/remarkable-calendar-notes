//! Atomic JSON persistence.
//!
//! All application state (config, sources, ink) is written by serializing
//! to a temporary file in the same directory and renaming it over the real
//! path, so a crash or power loss mid-write can never leave a half-written,
//! corrupt file in place — `rename` within a filesystem is atomic on both
//! Linux (the reMarkable's OS) and the desktop platforms used for
//! development.

use serde::{de::DeserializeOwned, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that overrides the data directory, primarily for
/// tests and desktop development. On-device this is left unset and the app
/// falls back to `~/.local/share/remarkable-calendar-notes`.
pub const DATA_DIR_ENV: &str = "REMARKABLE_CALENDAR_NOTES_DATA_DIR";

/// Resolve the directory application data is stored under, creating it if
/// necessary.
pub fn data_dir() -> io::Result<PathBuf> {
    let dir = if let Ok(over_ride) = std::env::var(DATA_DIR_ENV) {
        PathBuf::from(over_ride)
    } else {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .or_else(device_home_fallback)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME/USERPROFILE set"))?;
        Path::new(&home)
            .join(".local")
            .join("share")
            .join("remarkable-calendar-notes")
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn device_home_fallback() -> Option<std::ffi::OsString> {
    #[cfg(target_os = "linux")]
    {
        Some("/home/root".into())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Write `value` as pretty JSON to `path` atomically: serialize to a sibling
/// temp file, flush+sync it, then rename over `path`.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state")
    ));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(&json)?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Read and deserialize JSON from `path`. Returns `Ok(None)` if the file
/// does not exist yet (first run), rather than treating that as an error.
pub fn read_json_opt<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let value = serde_json::from_str(&contents)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Outcome of a fault-tolerant JSON read.
pub struct Recovered<T> {
    /// The parsed value, or `None` when the file was missing or could not
    /// be parsed (in which case the caller should fall back to defaults).
    pub value: Option<T>,
    /// Set when the file existed but could not be parsed: the human-readable
    /// parse error.
    pub error: Option<String>,
    /// Set when a corrupt file was moved aside: the path it was preserved at
    /// (or a note if it could only be deleted).
    pub recovered_from: Option<String>,
}

/// Read and deserialize JSON, but never fail because the on-disk file is
/// corrupt or schema-incompatible.
///
/// A file that exists but cannot be parsed is moved aside to
/// `<name>.corrupt-<epoch-seconds>` (so the user's data is preserved for
/// inspection and the app cannot get stuck failing on it every launch), and
/// `value` comes back `None` so the caller uses defaults. Only a genuine
/// I/O error other than "not found" is returned as `Err`.
///
/// This is what keeps a bad locally-stored config or ink file from turning
/// into a permanently blank screen: startup always proceeds with defaults.
pub fn read_json_recovering<T: DeserializeOwned>(path: &Path) -> io::Result<Recovered<T>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Recovered {
                value: None,
                error: None,
                recovered_from: None,
            });
        }
        Err(e) => return Err(e),
    };
    match serde_json::from_str(&contents) {
        Ok(value) => Ok(Recovered {
            value: Some(value),
            error: None,
            recovered_from: None,
        }),
        Err(parse_error) => {
            let recovered_from = quarantine_corrupt_file(path);
            Ok(Recovered {
                value: None,
                error: Some(parse_error.to_string()),
                recovered_from: Some(recovered_from),
            })
        }
    }
}

/// Move a corrupt file aside so it is preserved but no longer read on the
/// next launch. Falls back to deleting it if it cannot be renamed, since the
/// overriding goal is that the app must not keep failing on the same file.
fn quarantine_corrupt_file(path: &Path) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let backup = path.with_file_name(format!("{file_name}.corrupt-{stamp}"));
    match fs::rename(path, &backup) {
        Ok(()) => backup.display().to_string(),
        Err(_) => match fs::remove_file(path) {
            Ok(()) => "deleted (could not be renamed)".to_string(),
            Err(e) => format!("left in place (could not move aside: {e})"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Sample {
        n: u32,
        s: String,
    }

    #[test]
    fn writes_and_reads_back_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        let value = Sample {
            n: 42,
            s: "hi".into(),
        };
        write_json_atomic(&path, &value).unwrap();
        let restored: Option<Sample> = read_json_opt(&path).unwrap();
        assert_eq!(restored, Some(value));
    }

    #[test]
    fn missing_file_reads_as_none_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let restored: Option<Sample> = read_json_opt(&path).unwrap();
        assert_eq!(restored, None);
    }

    #[test]
    fn recovering_read_returns_value_for_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        write_json_atomic(
            &path,
            &Sample {
                n: 7,
                s: "ok".into(),
            },
        )
        .unwrap();
        let recovered: Recovered<Sample> = read_json_recovering(&path).unwrap();
        assert_eq!(recovered.value.unwrap().n, 7);
        assert!(recovered.error.is_none());
        assert!(recovered.recovered_from.is_none());
    }

    #[test]
    fn recovering_read_of_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let recovered: Recovered<Sample> = read_json_recovering(&path).unwrap();
        assert!(recovered.value.is_none());
        assert!(recovered.error.is_none());
        assert!(recovered.recovered_from.is_none());
    }

    #[test]
    fn recovering_read_quarantines_a_corrupt_file_and_falls_back_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        fs::write(&path, "{ this is not valid json ]").unwrap();

        let recovered: Recovered<Sample> = read_json_recovering(&path).unwrap();
        // Caller must fall back to defaults.
        assert!(recovered.value.is_none());
        assert!(recovered.error.is_some());
        // The corrupt file is moved aside, not left where it would fail again.
        assert!(!path.exists());
        let quarantined: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("sample.json.corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1);
    }

    #[test]
    fn no_leftover_temp_file_after_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        write_json_atomic(
            &path,
            &Sample {
                n: 1,
                s: "a".into(),
            },
        )
        .unwrap();
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].to_str().unwrap(), "sample.json");
    }

    #[test]
    #[serial_test::serial]
    fn data_dir_respects_env_override() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(DATA_DIR_ENV, dir.path());
        let resolved = data_dir().unwrap();
        assert_eq!(resolved, dir.path());
        std::env::remove_var(DATA_DIR_ENV);
    }
}
