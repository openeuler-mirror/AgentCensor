use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CensorFsError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum ErrorCode {
    BadRequest = 1,
    NotFound = 2,
    AccessDenied = 3,
    HeadChanged = 4,
    StateChanged = 5,
    BusyOpenWriters = 6,
    AlreadyExistsDifferent = 7,
    NoSpace = 8,
    ReadOnly = 9,
    IoError = 10,
    ResultUnknown = 11,
    Unsupported = 12,
    Corrupt = 13,
    Conflict = 14,
    AlreadyUpToDate = 15,
    DirectoryNotEmpty = 16,
}

#[derive(Debug, Error)]
#[error("{code:?}: {message}")]
pub struct CensorFsError {
    pub code: ErrorCode,
    pub message: String,
}

impl CensorFsError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Corrupt, message)
    }

    pub fn not_found(kind: &str, id: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("{kind} {id} was not found"))
    }
}

impl From<std::io::Error> for CensorFsError {
    fn from(value: std::io::Error) -> Self {
        let code = if value.raw_os_error() == Some(28) {
            ErrorCode::NoSpace
        } else {
            ErrorCode::IoError
        };
        Self::new(code, value.to_string())
    }
}

impl From<Box<bincode::ErrorKind>> for CensorFsError {
    fn from(value: Box<bincode::ErrorKind>) -> Self {
        Self::corrupt(value.to_string())
    }
}
