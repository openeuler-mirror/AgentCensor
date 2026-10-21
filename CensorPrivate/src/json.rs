//! Strict JSON parser: duplicate object keys are rejected at every depth.
use crate::error::{Error, Result};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::{collections::BTreeSet, fmt};

struct Seed {
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        if self.depth > 64 {
            return Err(de::Error::custom("json_depth"));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<(), E> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(de::Error::custom("invalid_json_number"))
        }
    }
    fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_string<E: de::Error>(self, _: String) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> std::result::Result<(), A::Error> {
        while access
            .next_element_seed(Seed {
                depth: self.depth + 1,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> std::result::Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = access.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate_json_key"));
            }
            access.next_value_seed(Seed {
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}

pub fn from_slice(raw: &[u8]) -> Result<Value> {
    let mut parser = serde_json::Deserializer::from_slice(raw);
    Seed { depth: 0 }
        .deserialize(&mut parser)
        .map_err(|error| {
            if error.to_string().starts_with("duplicate_json_key") {
                Error::DuplicateJsonKey
            } else {
                Error::InvalidJson
            }
        })?;
    parser.end().map_err(|_| Error::InvalidJson)?;
    // Parse again with arbitrary_precision after the duplicate/depth preflight.
    // Large business integers must never be silently rounded through f64.
    let value: Value = serde_json::from_slice(raw).map_err(|_| Error::InvalidJson)?;
    fn finite(value: &Value) -> bool {
        match value {
            Value::Number(number) => {
                let text = number.to_string();
                !text.contains(['.', 'e', 'E']) || number.as_f64().is_some_and(f64::is_finite)
            }
            Value::Array(values) => values.iter().all(finite),
            Value::Object(values) => values.values().all(finite),
            _ => true,
        }
    }
    if !finite(&value) {
        return Err(Error::InvalidJson);
    }
    Ok(value)
}
