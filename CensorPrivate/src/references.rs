use crate::error::{Error, Result};
use regex::Regex;
use std::{fmt, sync::LazyLock};

pub const TOKEN_PATTERN: &str = r"<CP_[A-Z][A-Z0-9_]*_[a-z0-9]{8}>";
/// Public tool-schema grammar used by the DSH TLS adapter, including aliases.
pub const TLS_TOOL_REFERENCE_PATTERN: &str =
    r"^(?:<CP_[A-Z][A-Z0-9_]*_[a-z0-9]{8}>|~P[A-Z]{10,}|~[A-Z]{5,10}|[A-Z]{3,5})$";
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
        const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut suffix = String::with_capacity(8);
        while suffix.len() < 8 {
            let mut bytes = [0u8; 16];
            getrandom::getrandom(&mut bytes).map_err(|_| Error::ServiceUnavailable)?;
            // 252 is divisible by 36: rejection avoids modulo bias.
            for byte in bytes.into_iter().filter(|byte| *byte < 252) {
                suffix.push(ALPHABET[usize::from(byte) % ALPHABET.len()] as char);
                if suffix.len() == 8 {
                    break;
                }
            }
        }
        Ok(Self(format!("<CP_{entity_type}_{suffix}>")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_grammar_accepts_base36_and_rejects_other_lengths_and_characters() {
        for valid in ["<CP_PERSON_az09xy78>", "<CP_PRIVATE_RESULT_00000000>"] {
            assert!(Reference::parse(valid).is_ok());
            assert!(valid_reference(valid, false));
        }
        for invalid in [
            "<CP_PERSON_abcdefg>",
            "<CP_PERSON_abcdefghi>",
            "<CP_PERSON_ABC12345>",
            "<CP_PERSON_ab-c1234>",
            "<CP_PERSON_0123456789abcdef0123456789abcdef>",
            "prefix<CP_PERSON_az09xy78>",
            "<CP_PERSON_az09xy78>suffix",
            "<CP_PERSON_az09xy78>\n",
        ] {
            assert!(Reference::parse(invalid).is_err(), "{invalid}");
        }
    }
}
