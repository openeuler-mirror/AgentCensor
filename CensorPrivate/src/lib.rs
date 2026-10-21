//! Trusted, transport-independent privacy core.
//!
//! References identify values; only a locally issued run capability authorizes
//! access. Secret values have no public serialization or Debug implementation.
#![forbid(unsafe_code)]

pub mod compute;
pub mod detection;
pub mod display;
pub mod error;
pub mod gate;
pub mod json;
pub mod policy;
pub mod protocols;
pub mod references;
pub mod response;
pub mod server;
pub mod streaming;
pub mod vault;
