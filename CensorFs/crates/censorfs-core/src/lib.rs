//! CensorFS core: immutable generations, private ticket overlays and recoverable publishing.

pub mod api_generated;
pub mod branch;
pub mod codec;
pub mod control;
pub mod error;
pub mod fault;
pub mod fsck;
pub mod ids;
pub mod model;
pub mod namespace;
pub mod persist;
pub mod store;
pub mod upper;
pub mod viewfs;

#[cfg(target_os = "linux")]
pub mod fuse_adapter;

pub use branch::{InitOptions, CensorFs};
pub use error::{ErrorCode, Result, CensorFsError};
pub use ids::*;
pub use model::*;
