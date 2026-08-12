//! Small dependency-free device log used when the framebuffer cannot show
//! enough information to diagnose startup/runtime failures.

use calnotes_core::persistence;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_FILE: &str = "calendar-notes.log";
const PREVIOUS_LOG_FILE: &str = "calendar-notes.previous.log";
const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOG: OnceLock<Mutex<File>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

#[cfg(unix)]
pub fn write_start_marker() {
    use std::os::unix::fs::OpenOptionsExt;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    let directory = PathBuf::from("/home/root/.local/share/remarkable-calendar-notes");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let pid = std::process::id();
    let temporary = directory.join(format!(".process-started-{pid}-{timestamp:.3}.tmp"));
    let marker = directory.join("process-started.txt");
    let Ok(mut file) = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
    else {
        return;
    };
    let result = writeln!(
        file,
        "version={}\npid={pid}\ntimestamp={timestamp:.3}",
        env!("CARGO_PKG_VERSION")
    )
    .and_then(|()| file.sync_all());
    drop(file);
    if result.is_ok() {
        let _ = fs::rename(&temporary, marker);
    } else {
        let _ = fs::remove_file(temporary);
    }
}

pub fn init() -> Option<PathBuf> {
    if let Some(path) = LOG_PATH.get() {
        return Some(path.clone());
    }
    let preferred = preferred_log_directory();
    let path = open_log_in(&preferred)
        .or_else(|| open_log_in(&std::env::temp_dir()))
        .map(|(file, path)| {
            let _ = LOG.set(Mutex::new(file));
            path
        })?;
    let _ = LOG_PATH.set(path.clone());

    std::panic::set_hook(Box::new(move |panic_info| {
        log(format_args!("PANIC: {panic_info}"));
    }));
    log(format_args!(
        "=== Calendar Notes {} starting; pid={} ===",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    Some(path)
}

fn preferred_log_directory() -> PathBuf {
    if let Ok(over_ride) = std::env::var(persistence::DATA_DIR_ENV) {
        return PathBuf::from(over_ride);
    }
    #[cfg(unix)]
    {
        PathBuf::from("/home/root/.local/share/remarkable-calendar-notes")
    }
    #[cfg(not(unix))]
    {
        persistence::data_dir().unwrap_or_else(|_| std::env::temp_dir())
    }
}

fn open_log_in(dir: &std::path::Path) -> Option<(File, PathBuf)> {
    fs::create_dir_all(dir).ok()?;
    let path = dir.join(LOG_FILE);
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
        let previous = dir.join(PREVIOUS_LOG_FILE);
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&path, previous);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    Some((file, path))
}

#[cfg(unix)]
pub fn path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

pub fn log(message: fmt::Arguments<'_>) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    if let Some(file) = LOG.get() {
        let lock = match file.try_lock() {
            Ok(lock) => Some(lock),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(mut file) = lock {
            if file
                .metadata()
                .is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES)
            {
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let _ = writeln!(file, "[{timestamp:.3}] === log truncated at 1 MiB ===");
            }
            let _ = writeln!(file, "[{timestamp:.3}] {message}");
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn logger_creates_a_retrievable_file_in_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let path = init().unwrap();
        #[cfg(unix)]
        assert_eq!(path, super::path().unwrap());
        log(format_args!("diagnostic test marker"));
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("diagnostic test marker"));
        std::env::remove_var(persistence::DATA_DIR_ENV);
    }
}
