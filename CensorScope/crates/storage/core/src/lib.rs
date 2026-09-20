//! Unified storage backend facade.

mod backend;
mod backfill;
mod error;

pub use backend::{AncillaryRow, StorageBackend};
pub use backfill::{BackfillJob, BackfillJobKind, BackfillJobState, CallSpanRecord};
pub use error::StorageError;
