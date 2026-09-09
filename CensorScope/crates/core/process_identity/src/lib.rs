//! Process identity contracts, coordinate resolution, and logical-ID management.

mod contract;
mod manager;
#[cfg(test)]
mod manager_tests;

pub use contract::{
    HostProcessCoordinates, IdentityLookupError, NamespaceIdentity, NamespaceProcessCoordinates,
    ProcessIdentity, ProcessIdentityReader, ProcessObservation, ProcessRecord,
    ProcessResolutionState, SessionIdentity,
};
pub use manager::{ProcessIdentityError, ProcessIdentityManager, ProcessResolution};
