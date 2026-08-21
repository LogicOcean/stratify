use crate::config::builder::Builder;
use crate::config::error::Error;
use crate::config::source::Source;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::{Arc, RwLock};

/// Central configuration store that merges multiple sources and caches the result.
///
/// `Store` is the primary interface for accessing merged configuration.
/// It loads all sources via a [`Builder`], deep-merges them in priority order,
/// and caches the result behind a [`RwLock`] for thread-safe reads.
///
/// # Example
/// ```rust,no_run
/// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
/// use stratify::config::Builder;
///
/// let store = Builder::default()
///     .json("base.json", 100)
///     .yaml("override.yaml", 50)
///     .build().await
///     .unwrap();
///
/// assert_eq!(store.get_str("host").unwrap(), "example.com");
/// # Ok(()) }
/// ```
///
/// Call [`Store::refresh`] to reload all sources without recreating the store.
pub struct Store {
    sources: Vec<Arc<dyn Source>>,
    cache: RwLock<Value>,
}

impl Store {
    /// Create a store by building and loading all sources from a [`Builder`].
    ///
    /// This is called automatically by [`Builder::build`]; most users won't
    /// call this directly.
    pub async fn from_builder(builder: Builder) -> Result<Self, Error> {
        let sources = builder.build_sources();
        let merged = merge_all(&sources).await?;
        Ok(Self {
            sources,
            cache: RwLock::new(merged),
        })
    }

    /// Reload all configuration sources and update the cache.
    ///
    /// This re-reads every source (files, environment, etc.) and re-merges.
    /// Useful for picking up configuration changes without restarting.
    ///
    /// # Errors
    /// Returns `Err` if any source fails to load. The cache is **not** updated
    /// on error — the previous values remain.
    pub async fn refresh(&self) -> Result<(), Error> {
        let merged = merge_all(&self.sources).await?;
        let mut cache = self
            .cache
            .write()
            .map_err(|e| Error::Other(format!("cache lock poisoned: {e}")))?;
        *cache = merged;
        Ok(())
    }

    /// Get a string value by dot-separated key path.
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
    /// # use stratify::config::Builder;
    /// # let store = Builder::default().build().await.unwrap();
    #[doc = "let db_host = store.get_str(\"db.host\");"]
    /// # Ok(()) }
    /// ```
    pub fn get_str(&self, key: &str) -> Option<String> {
        self.navigate(key)
            .and_then(|v| v.as_str().map(String::from))
    }

    /// Get a u64 value by dot-separated key path.
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
    /// # use stratify::config::Builder;
    /// # let store = Builder::default().build().await.unwrap();
    #[doc = "let port = store.get_u64(\"port\");"]
    /// # Ok(()) }
    /// ```
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.navigate(key).and_then(|v| v.as_u64())
    }

    /// Get a bool value by dot-separated key path.
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
    /// # use stratify::config::Builder;
    /// # // Build a store from an empty builder (doc-tests can't access test_helpers)
    /// # let store = Builder::default().build().await.unwrap();
    /// // With actual config loaded, you'd do:
    /// // assert_eq!(store.get_bool("debug"), Some(true));
    /// let is_set = store.get_bool("feature_enabled");
    /// # Ok(()) }
    /// ```
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.navigate(key).and_then(|v| v.as_bool())
    }

    /// Deserialize a config subsection into a strongly-typed struct.
    ///
    /// Navigates to `key`, then uses serde to deserialize the JSON value
    /// into `T`. Use this for groups of related settings.
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
    /// use serde::Deserialize;
    /// use stratify::config::Builder;
    ///
    /// #[derive(Deserialize)]
    /// struct DbConfig {
    ///     host: String,
    ///     port: u16,
    /// }
    ///
    /// # let store = Builder::default().build().await.unwrap();
    #[doc = "let db: DbConfig = store.get(\"db\").unwrap();"]
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    /// Returns `Err` if the key doesn't exist or deserialization fails.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error> {
        let val = self
            .navigate(key)
            .ok_or_else(|| Error::NotFound(key.to_string()))?;
        serde_json::from_value(val).map_err(Error::from)
    }

    /// Return a clone of the entire merged configuration as a [`Value`].
    pub fn full_config(&self) -> Value {
        self.cache.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// Navigate a dot-separated key path to a [`Value`], returning `None`
    /// if any segment is missing.
    fn navigate(&self, key: &str) -> Option<Value> {
        let cache = self.cache.read().ok()?;
        let mut current: &Value = &cache;
        for segment in key.split('.') {
            current = current.get(segment)?;
        }
        Some(current.clone())
    }
}

/// Deep-merge `other` into `target`, recursively merging objects.
///
/// Scalar and array values from `other` **replace** `target` entirely.
/// Objects are merged key-by-key — keys present in `other` but not
/// `target` are added, and shared keys are recursively merged.
///
/// Note: arrays are replaced wholesale, not concatenated. If you need
/// additive array semantics (e.g., appending to `allowed_origins`),
/// handle that at the application level after loading.
fn deep_merge(target: &mut Value, other: &Value) {
    match (target, other) {
        (Value::Object(t), Value::Object(o)) => {
            for (k, v) in o {
                if let Some(existing) = t.get_mut(k) {
                    deep_merge(existing, v);
                } else {
                    t.insert(k.clone(), v.clone());
                }
            }
        }
        (t, o) => *t = o.clone(),
    }
}

/// Load all sources and merge them in **reverse** priority order.
///
/// The lowest-priority source (highest number) is loaded first, then
/// higher-priority sources override its values via [`deep_merge`].
async fn merge_all(sources: &[Arc<dyn Source>]) -> Result<Value, Error> {
    let mut merged = Value::Object(serde_json::Map::new());
    for source in sources.iter().rev() {
        let loaded = source.load().await?;
        deep_merge(&mut merged, &loaded);
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::test_helpers::{write_temp, EnvGuard};

    #[tokio::test]
    async fn loads_and_merges_json_config() {
        let base = write_temp(r#"{"host": "localhost", "port": 5432, "debug": false}"#);
        let override_file = write_temp(r#"{"host": "prod.example.com", "debug": true}"#);

        let store = Builder::default()
            .json(base.path(), 100)
            .json(override_file.path(), 50)
            .build()
            .await
            .unwrap();

        assert_eq!(store.get_str("host").unwrap(), "prod.example.com");
        assert_eq!(store.get_u64("port").unwrap(), 5432);
        assert!(store.get_bool("debug").unwrap());
    }

    #[tokio::test]
    async fn deep_merges_nested_objects() {
        let base = write_temp(r#"{"db": {"host": "localhost", "port": 5432}}"#);
        let override_file = write_temp(r#"{"db": {"host": "db.example.com"}}"#);

        let store = Builder::default()
            .json(base.path(), 100)
            .json(override_file.path(), 50)
            .build()
            .await
            .unwrap();

        assert_eq!(store.get_str("db.host").unwrap(), "db.example.com");
        assert_eq!(store.get_u64("db.port").unwrap(), 5432);
    }

    #[tokio::test]
    async fn refresh_reloads_from_files() {
        let f = write_temp(r#"{"version": 1}"#);
        let store = Builder::default().json(f.path(), 0).build().await.unwrap();

        assert_eq!(store.get_u64("version").unwrap(), 1);

        // Write new content to the same temp file
        std::fs::write(f.path(), r#"{"version": 2}"#).unwrap();
        store.refresh().await.unwrap();
        assert_eq!(store.get_u64("version").unwrap(), 2);
    }

    #[tokio::test]
    async fn deserializes_into_struct() {
        use serde::Deserialize;

        #[derive(Deserialize, Debug, PartialEq)]
        struct DbConfig {
            host: String,
            port: u16,
        }

        let json_file = write_temp(r#"{"db": {"host": "pg.example.com", "port": 5432}}"#);
        let store = Builder::default()
            .json(json_file.path(), 0)
            .build()
            .await
            .unwrap();

        let db: DbConfig = store.get("db").unwrap();
        assert_eq!(db.host, "pg.example.com");
        assert_eq!(db.port, 5432);
    }

    #[tokio::test]
    async fn missing_key_returns_none() {
        let f = write_temp(r#"{"exists": true}"#);
        let store = Builder::default().json(f.path(), 0).build().await.unwrap();

        assert!(store.get_str("nonexistent").is_none());
        assert!(store.get_u64("nonexistent").is_none());
        assert!(store.get_bool("nonexistent").is_none());
    }

    #[tokio::test]
    async fn missing_key_get_typed_returns_error() {
        let f = write_temp(r#"{"exists": true}"#);
        let store = Builder::default().json(f.path(), 0).build().await.unwrap();

        let result: Result<Value, _> = store.get("nonexistent");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn builds_from_empty_builder() {
        let store = Builder::default().build().await.unwrap();
        assert_eq!(store.full_config(), Value::Object(serde_json::Map::new()));
    }

    #[tokio::test]
    async fn env_and_file_merge_correctly() {
        let _guard = EnvGuard::set("CK_T5_HOST", "from-env");
        let file = write_temp(r#"{"host": "from-file", "port": 8080}"#);

        let store = Builder::default()
            .json(file.path(), 100) // loaded first (lowest priority)
            .env("CK_T5_", "__", 10) // loaded second (highest priority)
            .build()
            .await
            .unwrap();

        // env wins because it has higher priority (lower number)
        assert_eq!(store.get_str("host").unwrap(), "from-env");
        // port only in file, should still be available
        assert_eq!(store.get_u64("port").unwrap(), 8080);
    }

    #[tokio::test]
    async fn full_config_returns_complete_json() {
        let f = write_temp(r#"{"key": "value"}"#);
        let store = Builder::default().json(f.path(), 0).build().await.unwrap();

        let full = store.full_config();
        assert_eq!(full["key"], "value");
    }
}
