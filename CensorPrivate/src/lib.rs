//! Trusted, transport-independent privacy core.
//!
//! References identify values; only a locally issued run capability authorizes
//! access. Secret values have no public serialization or Debug implementation.
#![forbid(unsafe_code)]

pub mod detection;
pub mod error;
pub mod json;
pub mod policy;
pub mod references;
pub mod vault;
