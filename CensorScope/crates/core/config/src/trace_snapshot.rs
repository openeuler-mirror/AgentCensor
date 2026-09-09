//! Immutable trace-time capture configuration.

use std::time::SystemTime;

use model_core::capability::CapabilityRequest;
use model_core::ids::ProfileName;

use crate::capture_profile::CaptureProfile;

/// Immutable copy of profile settings stored with a newly created trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureProfileSnapshot {
    pub profile_name: ProfileName,
    pub captured_at: SystemTime,
    pub capability_requests: Vec<CapabilityRequest>,
}

impl CaptureProfileSnapshot {
    /// Capture the profile at one point in time.
    pub fn from_profile(profile: &CaptureProfile, captured_at: SystemTime) -> Self {
        Self {
            profile_name: profile.name.clone(),
            captured_at,
            capability_requests: profile.capabilities.clone(),
        }
    }
}
