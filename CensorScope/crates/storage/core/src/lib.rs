//! Unified storage backend facade.

mod backfill;
mod backend;
mod error;

pub use backfill::{
    BackfillJob, BackfillJobKind, BackfillJobState, CallSpanRecord,
};
pub use backend::StorageBackend;
pub use error::StorageError;
