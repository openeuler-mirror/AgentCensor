use std::io;

#[derive(Debug, thiserror::Error)]
pub enum PivotError {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("transaction conflict: {0}")]
    Conflict(String),
    #[error("component {component} failed: {message}")]
    Component {
        component: &'static str,
        message: String,
    },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl PivotError {
    pub fn component(component: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Component {
            component,
            message: error.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, PivotError>;
