//! Lenient accessors for transcript JSON. Transcript formats are undocumented
//! and drift, so a missing or mistyped field reads as absent instead of
//! failing the parse.

use jiff::Timestamp;
use serde_json::Value;

pub fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, key| v.get(key))
}

pub fn str_at<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    at(v, path).and_then(Value::as_str)
}

pub fn array_at<'a>(v: &'a Value, path: &str) -> &'a [Value] {
    at(v, path)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub fn timestamp_at(v: &Value, path: &str) -> Option<Timestamp> {
    str_at(v, path)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn at_follows_nested_keys() {
        let v = json!({"a": {"b": {"c": 1}}});
        assert_eq!(at(&v, "a.b.c"), Some(&json!(1)));
        assert_eq!(at(&v, "a.x.c"), None);
    }

    #[test]
    fn at_through_non_object_is_none() {
        let v = json!({"a": [1, 2]});
        assert_eq!(at(&v, "a.b"), None);
    }

    #[test]
    fn str_at_rejects_other_types() {
        let v = json!({"s": "x", "n": 1});
        assert_eq!(str_at(&v, "s"), Some("x"));
        assert_eq!(str_at(&v, "n"), None);
    }

    #[test]
    fn array_at_defaults_to_empty() {
        let v = json!({"a": [1], "s": "x"});
        assert_eq!(array_at(&v, "a"), [json!(1)]);
        assert!(array_at(&v, "s").is_empty());
        assert!(array_at(&v, "missing").is_empty());
    }

    #[test]
    fn timestamp_at_parses_rfc3339() {
        let v = json!({"t": "2026-09-15T19:42:00.000Z", "bad": "yesterday"});
        assert_eq!(
            timestamp_at(&v, "t").unwrap().to_string(),
            "2026-09-15T19:42:00Z"
        );
        assert_eq!(timestamp_at(&v, "bad"), None);
        assert_eq!(timestamp_at(&v, "missing"), None);
    }
}
