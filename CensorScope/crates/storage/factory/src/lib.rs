//! Storage backend configuration and factory.

mod builder;
mod config;

pub use builder::open_storage_backend;
pub use config::StorageConfig;
