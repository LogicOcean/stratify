use crate::config::error::Error;
use crate::config::source::Source;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Environment variable configuration source.
///
/// Captures env vars matching a **prefix** and maps them to nested `serde_json::Value`.
/// The **separator** (typically `__`) creates nesting — `APP_DB__HOST` becomes `{"db": {"host": "..."}}`.
///
/// # Example
/// ```rust,no_run
/// use stratify::config::source::EnvSource;
/// // With prefix "APP_" and separator "__":
/// //   APP_HOST=localhost    → {"host": "localhost"}
/// //   APP_DB__PORT=5432     → {"db": {"port": "5432"}}
/// let source = EnvSource::new("APP_", "__", 10);
/// ```
///
/// # Variables without a shared prefix
///
/// Some settings are named by convention rather than by application: `RUST_LOG`
/// is read by `tracing-subscriber`, `AZURE_STORAGE_ACCOUNT` is what Azure
/// injects. There is no prefix that selects those and nothing else, and an
/// empty prefix captures the entire environment, `PATH` and every other
/// process's secrets included.
///
/// [`EnvSource::with_keys`] captures exactly the variables you name:
///
/// ```rust,no_run
/// use stratify::config::source::EnvSource;
/// let source = EnvSource::with_keys(["RUST_LOG", "LOG_DIR"], "__", 10);
/// ```
pub struct EnvSource {
    prefix: String,
    separator: String,
    priority: u32,
    /// When present, only these variable names are captured and `prefix` is
    /// not consulted.
    keys: Option<HashSet<String>>,
}

impl EnvSource {
    /// Create a source over environment variables matching `prefix`.
    ///
    /// `separator` controls nesting: with `__`, `APP_DB__HOST` becomes
    /// `{"db": {"host": ...}}`.
    ///
    /// `priority` follows the crate convention: lower numbers win.
    pub fn new(prefix: impl Into<String>, separator: impl Into<String>, priority: u32) -> Self {
        Self {
            prefix: prefix.into(),
            separator: separator.into(),
            priority,
            keys: None,
        }
    }

    /// Create a source over exactly the named environment variables.
    ///
    /// For settings whose names are fixed by convention rather than sharing an
    /// application prefix. Names are matched case-insensitively and appear in
    /// the configuration lowercased, so `RUST_LOG` is read as `rust_log`.
    ///
    /// Prefer this to [`EnvSource::new`] with an empty prefix: an empty prefix
    /// matches every variable in the environment, which puts `PATH` and any
    /// unrelated process's secrets into the merged configuration.
    ///
    /// `separator` still applies, so a named variable containing it nests.
    ///
    /// `priority` follows the crate convention: lower numbers win.
    ///
    /// ```rust,no_run
    /// use stratify::config::source::EnvSource;
    /// let source = EnvSource::with_keys(["RUST_LOG", "LOG_DIR"], "__", 10);
    /// ```
    pub fn with_keys<I, S>(keys: I, separator: impl Into<String>, priority: u32) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            prefix: String::new(),
            separator: separator.into(),
            priority,
            keys: Some(
                keys.into_iter()
                    .map(|k| k.as_ref().to_ascii_uppercase())
                    .collect(),
            ),
        }
    }

    fn env_to_flat(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (k, v) in std::env::vars() {
            if let Some(keys) = &self.keys {
                if !keys.contains(&k.to_ascii_uppercase()) {
                    continue;
                }
                map.insert(k.to_lowercase().replace(&self.separator, "."), v);
                continue;
            }
            if k.starts_with(&self.prefix) {
                let stripped = k[self.prefix.len()..].to_lowercase();
                map.insert(stripped.replace(&self.separator, "."), v);
            }
        }
        map
    }
}

#[async_trait]
impl Source for EnvSource {
    fn name(&self) -> &str {
        "env"
    }
    fn priority(&self) -> u32 {
        self.priority
    }

    async fn load(&self) -> Result<Value, Error> {
        let flat = self.env_to_flat();
        crate::config::source::nesting::dot_keys_to_json(flat.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::test_helpers::EnvGuard;
    use crate::config::source::Source;

    #[tokio::test]
    async fn loads_env_vars_with_prefix() {
        let _guard = EnvGuard::set("CK_HOST_T_HOST", "from_env");
        let source = EnvSource::new("CK_HOST_T_", "__", 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["host"], "from_env");
    }

    #[tokio::test]
    async fn captures_flat_keys_without_separator() {
        let _guard = EnvGuard::set("CK_FLAT_T_DB_HOST", "pg.local");
        let source = EnvSource::new("CK_FLAT_T_", "__", 0);
        let val = source.load().await.unwrap();
        assert!(val.as_object().unwrap().contains_key("db_host"));
    }

    #[tokio::test]
    async fn separator_converts_to_dot_nesting() {
        let _guard = EnvGuard::set("CK_NEST_T_DB__HOST", "nested.local");
        let source = EnvSource::new("CK_NEST_T_", "__", 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["db"]["host"], "nested.local");
    }

    #[tokio::test]
    async fn with_keys_captures_only_the_named_variables() {
        // Arrange: a named variable and an unrelated one alongside it.
        let _g1 = EnvGuard::set("CK_WANTED_VAR", "yes");
        let _g2 = EnvGuard::set("CK_UNWANTED_VAR", "no");
        let source = EnvSource::with_keys(["CK_WANTED_VAR"], "__", 0);

        // Act
        let val = source.load().await.unwrap();

        // Assert
        assert_eq!(val["ck_wanted_var"], "yes");
        assert!(
            val.as_object().unwrap().get("ck_unwanted_var").is_none(),
            "an unnamed variable must not be captured"
        );
    }

    #[tokio::test]
    async fn with_keys_does_not_capture_the_whole_environment() {
        // Arrange: PATH is always present and is exactly what an empty prefix
        // would sweep up along with every other process's secrets.
        let source = EnvSource::with_keys(["CK_DEFINITELY_NOT_SET_ANYWHERE"], "__", 0);

        // Act
        let val = source.load().await.unwrap();

        // Assert
        assert!(
            val.as_object().unwrap().is_empty(),
            "naming no present variable must yield nothing, not everything"
        );
    }

    #[tokio::test]
    async fn with_keys_matches_case_insensitively() {
        // Arrange: environment variables are conventionally upper case, but the
        // caller should not have to remember that.
        let _guard = EnvGuard::set("CK_CASE_VAR", "value");
        let source = EnvSource::with_keys(["ck_case_var"], "__", 0);

        // Act
        let val = source.load().await.unwrap();

        // Assert
        assert_eq!(val["ck_case_var"], "value");
    }

    #[tokio::test]
    async fn with_keys_still_nests_on_the_separator() {
        // Arrange
        let _guard = EnvGuard::set("CK_NESTED__CHILD", "leaf");
        let source = EnvSource::with_keys(["CK_NESTED__CHILD"], "__", 0);

        // Act
        let val = source.load().await.unwrap();

        // Assert
        assert_eq!(val["ck_nested"]["child"], "leaf");
    }

    #[tokio::test]
    async fn conflicting_key_and_nested_key_returns_error() {
        let _g1 = EnvGuard::set("CK_CONF_T_DB_LEAF", "flat");
        let _g2 = EnvGuard::set("CK_CONF_T_DB_LEAF__HOST", "nested");
        let source = EnvSource::new("CK_CONF_T_", "__", 0);
        let result = source.load().await;
        assert!(result.is_err());
    }
}
