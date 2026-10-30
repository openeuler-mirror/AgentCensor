use crate::contract::{MAX_ARGUMENTS, MAX_DETAIL_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION};
use crate::event::ExternalEvent;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event schema version {0} is unsupported")]
    UnsupportedSchema(u16),
    #[error("invalid event field: {0}")]
    InvalidField(&'static str),
    #[error("event frame is too large: {0} bytes")]
    FrameTooLarge(usize),
}

pub fn validate_event(event: &ExternalEvent) -> Result<(), ContractError> {
    if event.schema_version != SCHEMA_VERSION {
        return Err(ContractError::UnsupportedSchema(event.schema_version));
    }
    if event.source_instance_id.is_empty() || event.source_instance_id.len() > MAX_TOKEN_BYTES {
        return Err(ContractError::InvalidField("source_instance_id"));
    }
    if event.source_sequence == 0 {
        return Err(ContractError::InvalidField("source_sequence"));
    }
    if event.occurred_at_ns == 0 {
        return Err(ContractError::InvalidField("occurred_at_ns"));
    }
    if event.event.operation.is_empty() || event.event.operation.len() > MAX_TOKEN_BYTES {
        return Err(ContractError::InvalidField("operation"));
    }
    if event.event.target.args.len() > MAX_ARGUMENTS {
        return Err(ContractError::InvalidField("target.args"));
    }
    if event
        .event
        .target
        .detail
        .as_ref()
        .is_some_and(|detail| detail.len() > MAX_DETAIL_BYTES)
    {
        return Err(ContractError::InvalidField("target.detail"));
    }
    Ok(())
}
