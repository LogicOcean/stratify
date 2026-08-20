use crate::error::ConfigError;
use crate::source::Source;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Configuration source that loads settings from a `.toml` file.
///
/// TOML is parsed with the `toml` crate and converted to `serde_json::Value`
/// for merging. Lower priority numbers = higher precedence.
///
/// # Example
/// ```rust
/// use stratify::source::TomlSource;
///
/// let source = TomlSource::new("config.toml", 50);
/// ```
pub struct TomlSource {
    path: PathBuf,
    priority: u32,
}

impl TomlSource {
    /// Create a new TOML source from a file path with the given priority.
    ///
    /// `priority` — lower numbers = higher precedence when merging.
    pub fn new(path: impl AsRef<Path>, priority: u32) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            priority,
        }
    }
}

#[async_trait]
impl Source for TomlSource {
    fn name(&self) -> &str {
        "toml"
    }

    fn priority(&self) -> u32 {
        self.priority
    }

    async fn load(&self) -> Result<Value, ConfigError> {
        let content = std::fs::read_to_string(&self.path)?;
        let toml_val: toml::Value =
            toml::from_str(&content).map_err(|e| ConfigError::Toml(e.to_string()))?;
        Ok(toml_to_json(toml_val))
    }
}

/// Convert a toml::Value to serde_json::Value recursively.
fn toml_to_json(val: toml::Value) -> Value {
    match val {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => {
            let n = serde_json::Number::from(i);
            Value::Number(n)
        }
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
        toml::Value::Array(arr) => Value::Array(arr.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map: serde_json::Map<String, Value> = table
                .into_iter()
                .map(|(k, v)| (k, toml_to_json(v)))
                .collect();
            Value::Object(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::test_helpers::write_temp;

    #[tokio::test]
    async fn loads_valid_toml() {
        let f = write_temp("host = \"localhost\"\nport = 5432\n");
        let source = TomlSource::new(f.path(), 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["host"], "localhost");
        assert_eq!(val["port"], 5432);
    }

    #[tokio::test]
    async fn loads_nested_toml() {
        let f = write_temp("[db]\nhost = \"pg.example.com\"\nport = 5432\n");
        let source = TomlSource::new(f.path(), 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["db"]["host"], "pg.example.com");
        assert_eq!(val["db"]["port"], 5432);
    }

    #[tokio::test]
    async fn loads_arrays() {
        let f = write_temp("origins = [\"localhost\", \"example.com\"]\n");
        let source = TomlSource::new(f.path(), 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["origins"][0], "localhost");
        assert_eq!(val["origins"][1], "example.com");
    }

    #[tokio::test]
    async fn loads_booleans() {
        let f = write_temp("debug = true\ncache = false\n");
        let source = TomlSource::new(f.path(), 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["debug"], true);
        assert_eq!(val["cache"], false);
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let source = TomlSource::new("/nonexistent/path.toml", 0);
        assert!(source.load().await.is_err());
    }

    #[tokio::test]
    async fn invalid_toml_is_error() {
        let f = write_temp("= invalid toml");
        let source = TomlSource::new(f.path(), 0);
        let err = source.load().await.unwrap_err();
        assert!(err.to_string().contains("TOML"));
    }

    #[tokio::test]
    async fn name_and_priority() {
        let source = TomlSource::new("config.toml", 42);
        assert_eq!(source.name(), "toml");
        assert_eq!(source.priority(), 42);
    }
}
