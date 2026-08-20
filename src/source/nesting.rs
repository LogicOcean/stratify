//! Shared conversion from flat, delimited keys into nested JSON.
//!
//! Several sources present configuration as a flat key/value list rather than a
//! tree: environment variables use a separator such as `__`, and Azure App
//! Configuration conventionally uses `:`. Both need the same expansion into a
//! nested [`Value`], so it lives here rather than in either one.

use crate::error::ConfigError;
use serde_json::{Map, Value};

/// Expand dot-separated keys into a nested JSON object.
///
/// Keys are processed shallowest-first, so that a conflict between a leaf and a
/// branch at the same path (`db.host` as both a value and an object) is detected
/// rather than silently resolved by ordering.
///
/// # Errors
/// Returns [`ConfigError::MergeConflict`] when a key requires a branch where an
/// earlier key already placed a scalar.
pub(crate) fn dot_keys_to_json<'a, I>(flat: I) -> Result<Value, ConfigError>
where
    I: IntoIterator<Item = (&'a String, &'a String)>,
{
    let mut root = Map::new();
    let mut sorted: Vec<(&String, &String)> = flat.into_iter().collect();
    sorted.sort_by_key(|(k, _)| k.matches('.').count());

    for (key, val) in sorted {
        let parts: Vec<&str> = key.split('.').collect();
        let mut current = &mut root;
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                current.insert(part.to_string(), Value::String(val.clone()));
            } else {
                let entry = current
                    .entry(part.to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                current = match entry.as_object_mut() {
                    Some(obj) => obj,
                    None => return Err(ConfigError::MergeConflict(key.clone())),
                };
            }
        }
    }
    Ok(Value::Object(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn expand(pairs: &[(&str, &str)]) -> Result<Value, ConfigError> {
        let owned: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        dot_keys_to_json(owned.iter())
    }

    #[test]
    fn flat_keys_stay_flat() {
        // Arrange / Act
        let value = expand(&[("host", "localhost")]).expect("expands");

        // Assert
        assert_eq!(value["host"], "localhost");
    }

    #[test]
    fn dotted_keys_become_nested() {
        // Arrange / Act
        let value = expand(&[("db.host", "pg"), ("db.port", "5432")]).expect("expands");

        // Assert
        assert_eq!(value["db"]["host"], "pg");
        assert_eq!(value["db"]["port"], "5432");
    }

    #[test]
    fn deeply_nested_keys_expand_fully() {
        // Arrange / Act
        let value = expand(&[("a.b.c.d", "deep")]).expect("expands");

        // Assert
        assert_eq!(value["a"]["b"]["c"]["d"], "deep");
    }

    #[test]
    fn a_leaf_blocking_a_branch_is_a_conflict() {
        // Arrange: "db" cannot be both a string and an object.
        let result = expand(&[("db", "flat"), ("db.host", "nested")]);

        // Assert
        assert!(matches!(result, Err(ConfigError::MergeConflict(_))));
    }

    #[test]
    fn an_empty_input_is_an_empty_object() {
        // Arrange / Act
        let value = expand(&[]).expect("expands");

        // Assert
        assert_eq!(value, Value::Object(Map::new()));
    }
}
