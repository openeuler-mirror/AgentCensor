use std::fmt;

/// Public failures carry fixed reason codes, never request data or library errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    PolicyInvalid,
    DetectionFailed,
    VaultDenied,
    ComputeInvalid,
    InvalidReference,
    InvalidJson,
    DuplicateJsonKey,
    ProtocolBlocked,
    SensitiveStructure,
    PrivateNumberRequiresCompute,
    ReleaseDenied,
    ServiceUnavailable,
    ResourceLimit,
    RuntimeBlocked,
    RuntimeCleanupFailed,
    RuntimeTimeout,
    TlsBlocked,
}

impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::PolicyInvalid => "policy_invalid",
            Self::DetectionFailed => "detection_failed",
            Self::VaultDenied => "vault_denied",
            Self::ComputeInvalid => "compute_invalid",
            Self::InvalidReference => "invalid_reference",
            Self::InvalidJson => "invalid_json",
            Self::DuplicateJsonKey => "duplicate_json_key",
            Self::ProtocolBlocked => "protocol_blocked",
            Self::SensitiveStructure => "sensitive_structure",
            Self::PrivateNumberRequiresCompute => "private_number_requires_compute",
            Self::ReleaseDenied => "release_denied",
            Self::ServiceUnavailable => "service_unavailable",
            Self::ResourceLimit => "resource_limit",
            Self::RuntimeBlocked => "runtime_blocked",
            Self::RuntimeCleanupFailed => "runtime_cleanup_failed",
            Self::RuntimeTimeout => "runtime_timeout",
            Self::TlsBlocked => "tls_blocked",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
