use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("failed to read policy {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("invalid group name {0:?}")]
    InvalidGroupName(String),
    #[error("invalid domain name {0:?}")]
    InvalidDomainName(String),
    #[error("domain {0:?} is defined more than once")]
    DuplicateDomain(String),
    #[error("domain {domain:?} references missing policy group {group:?}")]
    MissingGroup { domain: String, group: String },
    #[error("file rule {path:?} has an empty deny list")]
    EmptyFileDeny { path: String },
    #[error("unknown file operation {operation:?} in rule {path:?}")]
    UnknownFileOperation { path: String, operation: String },
    #[error("{kind} value must be an absolute path: {value:?}")]
    RelativePath { kind: &'static str, value: String },
    #[error("{kind} value contains a NUL byte: {value:?}")]
    EmbeddedNul { kind: &'static str, value: String },
    #[error("{kind} value is {actual} bytes; maximum is {maximum}: {value:?}")]
    KeyTooLong {
        kind: &'static str,
        value: String,
        actual: usize,
        maximum: usize,
    },
    #[error("argument rule must contain 1 to {maximum} tokens: {rule:?}")]
    ArgTokenCount { rule: String, maximum: usize },
    #[error("argument token is {actual} bytes; maximum is {maximum}: {token:?}")]
    ArgTokenTooLong {
        token: String,
        actual: usize,
        maximum: usize,
    },
    #[error("invalid network rule {rule:?}: {reason}")]
    Network { rule: String, reason: String },
}
