//! Backend-neutral storage configuration used by daemon bootstrap.

use std::path::Path;

use sqlite_storage::SqliteStorageConfig;

/// Configuration for the persistent event and process-tree storage backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageConfig {
    Sqlite(SqliteStorageConfig),
}

impl StorageConfig {
    pub fn sqlite_path(path: impl AsRef<Path>) -> Self {
        Self::Sqlite(SqliteStorageConfig::direct_path(path))
    }

    pub fn sqlite(path: impl AsRef<Path>, busy_timeout_ms: u64) -> Self {
        Self::Sqlite(SqliteStorageConfig {
            path: path.as_ref().to_path_buf(),
            busy_timeout_ms,
        })
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Sqlite(config) => &config.path,
        }
    }

    pub const fn sqlite_busy_timeout_ms(&self) -> u64 {
        match self {
            Self::Sqlite(config) => config.busy_timeout_ms,
        }
    }
}
