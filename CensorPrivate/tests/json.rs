use censorprivacy::{error::Error, json};
use serde_json::json;

#[test]
fn strict_json_rejects_duplicates_nonfinite_invalid_unicode_and_excessive_depth() {
    for raw in [
        br#"{"a":1,"a":2}"#.as_slice(),
        br#"{"outer":[{"x":1,"x":2}]}"#,
    ] {
        assert_eq!(json::from_slice(raw), Err(Error::DuplicateJsonKey));
    }
    for raw in [
        br#"{"a":NaN}"#.as_slice(),
        br#"{"a":1e999}"#,
        br#"{"a":"\uD800"}"#,
        b"{} {}",
        b"\xff",
    ] {
        assert!(json::from_slice(raw).is_err());
    }
    let deeply_nested = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    assert!(json::from_slice(deeply_nested.as_bytes()).is_err());
    assert_eq!(
        json::from_slice(br#"{"a":123,"b":true,"c":null}"#).unwrap(),
        json!({"a":123,"b":true,"c":null})
    );
    let integer = "1234567890123456789012345678901234567890";
    assert_eq!(
        json::from_slice(integer.as_bytes()).unwrap().to_string(),
        integer
    );
}
