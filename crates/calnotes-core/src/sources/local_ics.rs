//! Local `.ics` file source: reads a plain filesystem path. On the device
//! this is typically a file the user has copied on via USB/SSH; on
//! desktop, any accessible path works, which is what makes this source
//! trivial to exercise in tests.

use super::SourceError;

pub fn read_ics_file(path: &str) -> Result<String, SourceError> {
    std::fs::read_to_string(path).map_err(SourceError::Io)
}
