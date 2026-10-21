use crate::error::{Error, Result};
use regex::Regex;
use std::{fmt, sync::LazyLock};

pub const TOKEN_PATTERN: &str = r"<CP_[A-Z][A-Z0-9_]*_[0-9a-f]{32}>";
pub const TLS_ALIAS_PATTERN: &str = r"(?:~P[A-Za-z]{10,}|~[A-Za-z]{5,10}|[A-Z]{3,5})";
static TLS_ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("^{TLS_ALIAS_PATTERN}$")).expect("constant alias grammar")
});
pub fn valid_reference(value: &str, allow_tls_alias: bool) -> bool {
    Reference::parse(value).is_ok()
        || (allow_tls_alias && value.len() <= 512 && TLS_ALIAS_RE.is_match(value))
}
pub(crate) static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(TOKEN_PATTERN).expect("constant reference grammar"));

/// Parsing proves syntax only. It never confers a vault capability.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Reference(String);

impl Reference {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() > 512 || TOKEN_RE.find(value).is_none_or(|m| m.as_str() != value) {
            return Err(Error::InvalidReference);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn issue(entity_type: &str) -> Result<Self> {
        Ok(Self(format!("<CP_{entity_type}_{}>", random_hex::<16>()?)))
    }
}

impl fmt::Debug for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Reference").field(&self.0).finish()
    }
}
impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl serde::Serialize for Reference {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

pub(crate) fn random_hex<const N: usize>() -> Result<String> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Error::ServiceUnavailable)?;
    let mut result = String::with_capacity(N * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(result)
}
