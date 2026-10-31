use serde_json::Value;

pub(crate) fn parse_event_metadata(raw: &str) -> Value {
    if let Ok(Value::Object(metadata)) = serde_json::from_str::<Value>(raw) {
        return Value::Object(metadata);
    }
    let metadata = raw
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            (
                unescape_metadata(key),
                Value::String(unescape_metadata(value)),
            )
        })
        .collect();
    Value::Object(metadata)
}

fn unescape_metadata(raw: &str) -> String {
    let mut result = String::new();
    let mut chars = raw.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('e') => result.push('='),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(character);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_metadata_retains_value_types() {
        assert_eq!(
            parse_event_metadata(r#"{ "state": "covered", "target_fd": 4 }"#),
            json!({"state": "covered", "target_fd": 4}),
        );
    }

    #[test]
    fn legacy_metadata_preserves_escaped_values() {
        assert_eq!(
            parse_event_metadata("guard.decision=deny\nkey\\evalue=line1\\nline2\\\\name"),
            json!({"guard.decision": "deny", "key=value": "line1\nline2\\name"}),
        );
    }

    #[test]
    fn empty_or_unrecognized_metadata_is_an_empty_object() {
        for text in ["", "invalid", "[]", "null"] {
            assert_eq!(parse_event_metadata(text), json!({}));
        }
    }
}
