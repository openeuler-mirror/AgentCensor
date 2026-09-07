use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use std::fmt::{Display, Formatter};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
            pub fn parse(value: &str) -> Result<Self, uuid::Error> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id!(InstanceId);
uuid_id!(RequestId);
uuid_id!(TxId);
uuid_id!(TicketId);
uuid_id!(GenerationId);
uuid_id!(ManifestId);
uuid_id!(ObjectId);
uuid_id!(CandidateId);
uuid_id!(ViewId);

impl RequestId {
    /// Derive a stable sub-request id for an idempotent composite operation.
    pub fn derive(self, label: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.0.as_bytes());
        hasher.update(label.as_bytes());
        let digest = hasher.finalize();
        let mut bytes: [u8; 16] = digest.as_bytes()[..16].try_into().unwrap();
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Self(Uuid::from_bytes(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct BranchId(String);

impl BranchId {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err("branch id must contain 1..=128 bytes");
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        {
            return Err("branch id contains an unsupported byte");
        }
        if value.starts_with('/')
            || value.ends_with('/')
            || value
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err("branch id contains an invalid path component");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn storage_name(&self) -> String {
        hex::encode(self.0.as_bytes())
    }
}

impl Display for BranchId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BranchId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}
