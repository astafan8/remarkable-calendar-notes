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
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME/USERPROFILE set"))?;
        Path::new(&home)
            .join(".local")
            .join("share")
            .join("remarkable-calendar-notes")
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
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
