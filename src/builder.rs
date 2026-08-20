use crate::error::ConfigError;
use crate::source::{DotEnvSource, EnvSource, JsonSource, Source, YamlSource};
use std::path::Path;
use std::sync::Arc;

/// Fluent builder for assembling configuration sources in priority order.
///
/// Sources are loaded in priority order: lower priority numbers = higher precedence.
/// When multiple sources define the same key, the source with the lowest priority wins.
///
/// # Example
/// ```rust,no_run
/// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
/// use stratify::ConfigBuilder;
///
/// let store = ConfigBuilder::default()
///     .json("config/base.json", 100)
///     .yaml("config/override.yaml", 50)
///     .env("APP_", "__", 10)
///     .build().await
///     .unwrap();
/// # Ok(()) }
/// ```
#[derive(Default)]
pub struct ConfigBuilder {
    sources: Vec<Arc<dyn Source>>,
}

impl ConfigBuilder {
    /// Add an arbitrary [`Source`] implementation.
    ///
    /// Use this for custom sources that aren't covered by the convenience methods
    /// (`.json()`, `.yaml()`, `.env()`, `.dotenv()`).
    ///
    /// # Example
    /// ```rust
    /// use stratify::ConfigBuilder;
    /// use stratify::source::JsonSource;
    ///
    /// let builder = ConfigBuilder::default()
    ///     .source(JsonSource::new("config/app.json", 100));
    /// ```
    pub fn source(mut self, source: impl Source + 'static) -> Self {
        self.sources.push(Arc::new(source));
        self
    }

    /// Add a JSON file source.
    ///
    /// Loads a `.json` file and merges its contents at the given priority.
    ///
    /// # Parameters
    /// * `path` — path to the JSON file
    /// * `priority` — lower numbers = higher precedence
    pub fn json(self, path: impl AsRef<Path>, priority: u32) -> Self {
        self.source(JsonSource::new(path, priority))
    }

    /// Add a YAML file source.
    ///
    /// Loads a `.yaml` or `.yml` file and merges its contents at the given priority.
    ///
    /// # Parameters
    /// * `path` — path to the YAML file
    /// * `priority` — lower numbers = higher precedence
    pub fn yaml(self, path: impl AsRef<Path>, priority: u32) -> Self {
        self.source(YamlSource::new(path, priority))
    }

    /// Add a TOML file source.
    ///
    /// Loads a `.toml` file, converts it to JSON internally, and merges its
    /// contents at the given priority.
    ///
    /// # Parameters
    /// * `path` — path to the TOML file
    /// * `priority` — lower numbers = higher precedence
    pub fn toml(self, path: impl AsRef<Path>, priority: u32) -> Self {
        self.source(crate::source::TomlSource::new(path, priority))
    }

    /// Add an environment variable source.
    ///
    /// Captures all env vars matching `prefix`, strips the prefix, lowercases the key,
    /// and converts `separator` to `.` for nesting.
    ///
    /// # Example
    /// With prefix `"APP_"` and separator `"__"`:
    /// - `APP_HOST=localhost` → `{"host": "localhost"}`
    /// - `APP_DB__PORT=5432` → `{"db": {"port": "5432"}}`
    ///
    /// # Parameters
    /// * `prefix` — only capture env vars starting with this string
    /// * `separator` — delimiter in env var names that creates nesting (e.g. `"__"`)
    /// * `priority` — lower numbers = higher precedence
    pub fn env(
        self,
        prefix: impl Into<String>,
        separator: impl Into<String>,
        priority: u32,
    ) -> Self {
        self.source(EnvSource::new(prefix, separator, priority))
    }

    /// Add a `.env` file source.
    ///
    /// Loads environment variables from a `.env` file via [dotenvy],
    /// then captures them using the same prefix/separator semantics as `.env()`.
    ///
    /// # Errors
    /// Returns `Err` if the file cannot be read or parsed.
    ///
    /// # Parameters
    /// * `path` — path to the `.env` file
    /// * `prefix` — only capture env vars starting with this string
    /// * `separator` — delimiter in env var names that creates nesting
    /// * `priority` — lower numbers = higher precedence
    pub fn dotenv(
        self,
        path: impl AsRef<Path>,
        prefix: impl Into<String>,
        separator: impl Into<String>,
        priority: u32,
    ) -> Result<Self, ConfigError> {
        let source = DotEnvSource::new(path, &prefix.into(), &separator.into(), priority)?;
        Ok(self.source(source))
    }

    /// Return all registered sources, sorted by priority (ascending).
    ///
    /// Lower priority numbers come first — they have higher precedence during merging.
    /// Add an Azure App Configuration source.
    ///
    /// Available under the `azure` feature. The credential is supplied by the
    /// caller so that a service can use a managed identity in Azure and a
    /// developer credential locally without this crate choosing for it.
    ///
    /// For a label filter or a non-default separator, construct
    /// [`AzureAppConfigSource`](crate::source::AzureAppConfigSource) directly
    /// and pass it to [`ConfigBuilder::source`].
    ///
    /// `priority` follows the crate convention: lower numbers win.
    #[cfg(feature = "azure")]
    pub fn azure(
        self,
        endpoint: impl Into<String>,
        credential: std::sync::Arc<dyn azure_core::credentials::TokenCredential>,
        priority: u32,
    ) -> Self {
        let source = crate::source::AzureAppConfigSource::new(endpoint, credential, priority);
        self.source(source)
    }

    /// Add a source over exactly the named environment variables.
    ///
    /// For settings named by convention rather than by application, such as
    /// `RUST_LOG` or `AZURE_STORAGE_ACCOUNT`, where no prefix selects them and
    /// nothing else. See [`EnvSource::with_keys`](crate::source::EnvSource::with_keys).
    ///
    /// `priority` follows the crate convention: lower numbers win.
    pub fn env_keys<I, S>(self, keys: I, separator: impl Into<String>, priority: u32) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.source(EnvSource::with_keys(keys, separator, priority))
    }

    /// Consume the builder and return its sources, ordered by precedence.
    ///
    /// Sorted so that the lowest priority number comes first. Most callers want
    /// [`ConfigBuilder::build`] instead; this is exposed for anyone assembling a
    /// [`ConfigStore`](crate::ConfigStore) by hand.
    pub fn build_sources(mut self) -> Vec<Arc<dyn Source>> {
        self.sources.sort_by_key(|s| s.priority());
        self.sources
    }

    /// Build and load all sources into a [`ConfigStore`](crate::store::ConfigStore).
    ///
    /// This is the terminal operation — after calling `build()`, you get a
    /// fully-loaded, cached [`ConfigStore`](crate::store::ConfigStore) ready for querying.
    ///
    /// # Errors
    /// Returns `Err` if any source fails to load.
    pub async fn build(self) -> Result<crate::store::ConfigStore, ConfigError> {
        crate::store::ConfigStore::from_builder(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builder_stacks_sources() {
        let builder = ConfigBuilder::default().json("/tmp/base.json", 100);

        let sources = builder.build_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name(), "json");
        assert_eq!(sources[0].priority(), 100);
    }

    #[tokio::test]
    async fn toml_source_is_registered() {
        let builder = ConfigBuilder::default().toml("/tmp/config.toml", 50);
        let sources = builder.build_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name(), "toml");
        assert_eq!(sources[0].priority(), 50);
    }

    #[tokio::test]
    async fn multiple_sources_sorted_by_priority() {
        let builder = ConfigBuilder::default()
            .json("/tmp/base.json", 100)
            .yaml("/tmp/override.yaml", 50)
            .env("APP_", "__", 10);

        let sources = builder.build_sources();
        // Sorted ascending: env(10) first, then yaml(50), then json(100)
        assert_eq!(sources.len(), 3);
        assert_eq!(sources[0].priority(), 10);
        assert_eq!(sources[0].name(), "env");
        assert_eq!(sources[1].priority(), 50);
        assert_eq!(sources[1].name(), "yaml");
        assert_eq!(sources[2].priority(), 100);
        assert_eq!(sources[2].name(), "json");
    }

    #[tokio::test]
    async fn source_method_adds_custom_source() {
        use crate::source::JsonSource;
        let builder = ConfigBuilder::default().source(JsonSource::new("/tmp/custom.json", 25));

        let sources = builder.build_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].priority(), 25);
    }

    #[tokio::test]
    async fn dotenv_method_works() {
        use crate::source::test_helpers::{write_temp, EnvGuard};

        let f = write_temp("CONFIGKIT_BUILDER_KEY=built\n");
        let builder = ConfigBuilder::default().dotenv(f.path(), "CONFIGKIT_B", "__", 5);
        // dotenvy loaded the var; ensure cleanup
        let _guard = EnvGuard::remove_on_drop("CONFIGKIT_BUILDER_KEY");
        assert!(builder.is_ok());

        let sources = builder.unwrap().build_sources();
        assert_eq!(sources[0].priority(), 5);
        assert_eq!(sources[0].name(), "dotenv");
    }

    #[tokio::test]
    async fn default_creates_empty_builder() {
        let builder = ConfigBuilder::default();
        let sources = builder.build_sources();
        assert!(sources.is_empty());
    }
}
