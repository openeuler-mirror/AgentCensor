//! SQLite path and connection tuning used by the storage factory.

use std::path::{Path, PathBuf};

/// Default SQLite busy timeout used by the daemon storage backend.
pub const SQLITE_DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

/// SQLite file location and connection tuning parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteStorageConfig {
    pub path: PathBuf,
    pub busy_timeout_ms: u64,
}

impl SqliteStorageConfig {
    /// Creates file-backed configuration with the default busy timeout.
    pub fn direct_path(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            busy_timeout_ms: SQLITE_DEFAULT_BUSY_TIMEOUT_MS,
        }
    }
}
