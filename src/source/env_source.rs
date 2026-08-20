use crate::error::ConfigError;
use crate::source::Source;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

/// Environment variable configuration source.
///
/// Captures env vars matching a **prefix** and maps them to nested `serde_json::Value`.
/// The **separator** (typically `__`) creates nesting — `APP_DB__HOST` becomes `{"db": {"host": "..."}}`.
///
/// # Example
/// ```rust,no_run
/// use stratify::source::EnvSource;
/// // With prefix "APP_" and separator "__":
/// //   APP_HOST=localhost    → {"host": "localhost"}
/// //   APP_DB__PORT=5432     → {"db": {"port": "5432"}}
/// let source = EnvSource::new("APP_", "__", 10);
/// ```
pub struct EnvSource {
    prefix: String,
    separator: String,
    priority: u32,
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
        }
    }

    fn env_to_flat(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (k, v) in std::env::vars() {
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

    async fn load(&self) -> Result<Value, ConfigError> {
        let flat = self.env_to_flat();
        crate::source::nesting::dot_keys_to_json(flat.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::test_helpers::EnvGuard;
    use crate::source::Source;

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
    async fn conflicting_key_and_nested_key_returns_error() {
        let _g1 = EnvGuard::set("CK_CONF_T_DB_LEAF", "flat");
        let _g2 = EnvGuard::set("CK_CONF_T_DB_LEAF__HOST", "nested");
        let source = EnvSource::new("CK_CONF_T_", "__", 0);
        let result = source.load().await;
        assert!(result.is_err());
    }
}
