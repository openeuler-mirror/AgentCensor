//! Storage facade error type.

use std::fmt;

/// Backend-independent storage failure with a stable operation stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageError {
    pub stage: String,
    pub message: String,
}

impl StorageError {
    pub fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.stage, self.message)
    }
}
