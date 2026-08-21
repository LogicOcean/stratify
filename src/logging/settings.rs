//! The declarative half of the logging API: a serde schema read from the
//! configuration store.
//!
//! This is where the two halves of the crate meet in the config-to-logging
//! direction. The logging half does no parsing of its own — TOML, YAML, JSON,
//! environment layering and precedence are all [`crate::config`]'s job — and
//! this module only describes the shape of a `[logging]` section and turns it
//! into a [`Builder`](super::Builder).
//!
//! ```toml
//! # config.toml — any file the config store can read
//! [logging]
//! level = "info"
//! queue_size = 128000
//! reloadable = true
//!
//! [logging.console]
//! color = true
//!
//! [logging.file]
//! directory = "/var/log/myapp"
//! rotation = "daily"
//! retention_days = 30
//! ```
//!
//! ```rust,no_run
//! # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
//! use stratify::{config, logging::settings::Settings};
//!
//! let store = config::Builder::default().toml("config.toml", 10).build().await?;
//! let builder = Settings::from_store(&store, "logging")?;
//! builder.init()?;
//! # Ok(()) }
//! ```

use serde::{Deserialize, Serialize};

use super::error::Error;
use super::file::Rotation;
use super::rate_limit::RateLimit;
use super::sampling::SampleConfig;
use super::{ConsoleConfig, FileConfig, JsonConfig};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::EnvFilter;

/// Deserializable logging configuration (TOML, JSON, or YAML via `serde`).
///
/// Every section is optional — omitted sections keep their defaults.
/// The type is `#[non_exhaustive]`, so external callers should start with
/// [`Default`] and chain the `with_*` setters:
///
/// ```rust
/// use stratify::logging::settings::{ConsoleSettings, Settings};
///
/// let config = Settings::default()
///     .with_level("debug")
///     .with_console(ConsoleSettings {
///         color: true,
///         ..Default::default()
///     })
///     .with_reloadable(true);
///
/// assert_eq!(config.level.as_deref(), Some("debug"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Settings {
    /// Default filter directive, such as `"info"` or `"my_crate=debug"`.
    pub level: Option<String>,

    /// Console sink; absent means no console output.
    #[serde(default)]
    pub console: Option<ConsoleSettings>,

    /// JSON sink; absent means no JSON output.
    #[serde(default)]
    pub json: Option<JsonSettings>,

    /// File sink; absent means no file output.
    #[serde(default)]
    pub file: Option<FileSettings>,

    /// Rate limiting; absent means unlimited.
    #[serde(default)]
    pub rate_limit: Option<RateLimitSettings>,

    /// Sampling; absent means every event is kept.
    #[serde(default)]
    pub sampling: Option<SamplingSettings>,

    /// Per-sink queue capacity; absent means the built-in default.
    #[serde(default)]
    pub queue_size: Option<usize>,

    /// Allow the filter to be swapped at runtime.
    #[serde(default)]
    pub reloadable: Option<bool>,
}

impl Settings {
    /// Set the default tracing filter directive, such as `"info"` or
    /// `"my_crate=debug"`.
    pub fn with_level(mut self, level: impl Into<String>) -> Self {
        self.level = Some(level.into());
        self
    }

    /// Configure human-readable console output.
    pub fn with_console(mut self, config: ConsoleSettings) -> Self {
        self.console = Some(config);
        self
    }

    /// Configure structured JSON output to stdout.
    pub fn with_json(mut self, config: JsonSettings) -> Self {
        self.json = Some(config);
        self
    }

    /// Configure structured file output.
    pub fn with_file(mut self, config: FileSettings) -> Self {
        self.file = Some(config);
        self
    }

    /// Configure global event rate limiting.
    pub fn with_rate_limit(mut self, config: RateLimitSettings) -> Self {
        self.rate_limit = Some(config);
        self
    }

    /// Configure probabilistic event sampling.
    pub fn with_sampling(mut self, config: SamplingSettings) -> Self {
        self.sampling = Some(config);
        self
    }

    /// Set the non-blocking writer queue capacity.
    pub fn with_queue_size(mut self, size: usize) -> Self {
        self.queue_size = Some(size);
        self
    }

    /// Enable or disable runtime filter reloading.
    pub fn with_reloadable(mut self, enabled: bool) -> Self {
        self.reloadable = Some(enabled);
        self
    }

    /// Load configuration from a TOML string and return a pre-configured Builder.
    pub fn to_builder(&self) -> super::Builder {
        let mut builder = super::builder();

        if let Some(level) = &self.level {
            if let Ok(filter) = EnvFilter::builder().parse(level) {
                builder = builder.with_filter(filter);
            }
        }

        if let Some(c) = &self.console {
            builder = builder.console(c.to_console_config());
        }

        if let Some(j) = &self.json {
            builder = builder.json(j.to_json_config());
        }

        if let Some(fc) = &self.file {
            builder = builder.file(fc.to_file_config());
        }

        if let Some(rl) = &self.rate_limit {
            builder = builder.rate_limit(rl.to_rate_limit());
        }

        if let Some(s) = &self.sampling {
            builder = builder.sampling(s.to_sample_config());
        }

        if let Some(qs) = self.queue_size {
            builder = builder.queue_size(qs);
        }

        if self.reloadable.unwrap_or(false) {
            builder = builder.reloadable();
        }

        builder
    }

    /// Read a settings block out of the configuration store.
    ///
    /// `key` is a dot-separated path to the block, conventionally
    /// `"logging"`. An absent block is not an error here — the caller decides
    /// whether missing settings mean defaults or a misconfiguration — so use
    /// [`read`](Self::read) and match on [`config::Error::NotFound`] to make
    /// that choice; this method propagates it.
    ///
    /// # Errors
    ///
    /// [`Error::Settings`] when the key is missing or the block does not
    /// deserialize. Unknown keys are rejected rather than silently ignored,
    /// and the error names the offending key.
    ///
    /// [`config::Error::NotFound`]: crate::config::Error::NotFound
    pub fn from_store(store: &crate::config::Store, key: &str) -> Result<super::Builder, Error> {
        Ok(Self::read(store, key)?.to_builder())
    }

    /// Read a settings block, leaving the missing-vs-invalid distinction to
    /// the caller.
    ///
    /// # Errors
    ///
    /// [`crate::config::Error::NotFound`] when the key is absent, or a
    /// deserialization error when the block is malformed.
    pub fn read(store: &crate::config::Store, key: &str) -> Result<Self, crate::config::Error> {
        store.get(key)
    }
}

// ── TOML structs ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// The `[logging.console]` block.
#[serde(deny_unknown_fields)]
pub struct ConsoleSettings {
    /// `"stderr"` (default) or `"stdout"`.
    #[serde(default = "default_stream")]
    pub stream: String,
    /// `"utc"` (default), `"local"` or `"none"`.
    #[serde(default = "default_timestamp")]
    pub timestamp: String,
    /// ANSI colour in the output.
    #[serde(default = "default_true")]
    pub color: bool,
    /// Include the emitting thread's id on each line.
    #[serde(default = "default_true")]
    pub thread_ids: bool,
    /// Include the event's target (module path) on each line.
    #[serde(default = "default_true")]
    pub target: bool,
    /// Drop events rather than block when the queue is full.
    #[serde(default)]
    pub lossy: bool,
}

impl ConsoleSettings {
    fn to_console_config(&self) -> ConsoleConfig {
        ConsoleConfig {
            target_stream: parse_stream(&self.stream),
            timestamp: parse_timestamp(&self.timestamp),
            use_color: self.color,
            thread_ids: self.thread_ids,
            target: self.target,
            lossy: self.lossy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// The `[logging.json]` block.
#[serde(deny_unknown_fields)]
pub struct JsonSettings {
    /// Include the span scope on each record.
    #[serde(default = "default_true")]
    pub span_list: bool,
    /// Flatten event fields into the top-level object.
    #[serde(default = "default_true")]
    pub flatten: bool,
    /// Drop events rather than block when the queue is full.
    #[serde(default)]
    pub lossy: bool,
}

impl JsonSettings {
    fn to_json_config(&self) -> JsonConfig {
        JsonConfig {
            span_list: self.span_list,
            flatten: self.flatten,
            lossy: self.lossy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// The `[logging.file]` block.
#[serde(deny_unknown_fields)]
pub struct FileSettings {
    /// Directory the log files are written to. Created when absent.
    pub directory: String,
    /// Base name before the rotation suffix. Defaults to `"app.log"`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// `"json"` (default) or `"text"`.
    #[serde(default = "default_file_format")]
    pub format: String,
    /// `"utc"` (default), `"local"` or `"none"`.
    #[serde(default = "default_timestamp")]
    pub timestamp: String,
    /// `"daily"` (default), `"hourly"`, `"never"` or `"size"`.
    #[serde(default = "default_rotation")]
    pub rotation: String,
    /// Delete rotated files older than this many days; `0` keeps all.
    #[serde(default)]
    pub retention_days: u32,
    /// Roll at this many bytes. Only used when `rotation = "size"`.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    /// Retired files to keep when `rotation = "size"`.
    #[serde(default = "default_max_files")]
    pub max_files: usize,
    /// Gzip retired files. Requires the `compression` feature.
    #[serde(default)]
    pub compress: bool,
    /// Include the span scope on each JSON record.
    #[serde(default = "default_true")]
    pub span_list: bool,
    /// Flatten event fields into the top-level JSON object.
    #[serde(default = "default_true")]
    pub flatten: bool,
    /// Drop events rather than block when the queue is full.
    #[serde(default)]
    pub lossy: bool,
}

impl FileSettings {
    fn to_file_config(&self) -> FileConfig {
        let rot = match self.rotation.as_str() {
            "hourly" => Rotation::Hourly,
            "never" => Rotation::Never,
            "size" => Rotation::Size {
                max_bytes: self.max_bytes,
                max_files: self.max_files,
            },
            _ => Rotation::Daily,
        };
        FileConfig {
            directory: self.directory.clone(),
            rotation: rot,
            retention_days: self.retention_days,
            json_config: JsonConfig {
                span_list: self.span_list,
                flatten: self.flatten,
                lossy: self.lossy,
            },
            format: parse_file_format(&self.format),
            prefix: self.prefix.clone(),
            timestamp: parse_timestamp(&self.timestamp),
            compress: self.compress,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// The `[logging.rate_limit]` block.
#[serde(deny_unknown_fields)]
pub struct RateLimitSettings {
    /// Events admitted per window; the rest are discarded.
    pub max_events: u64,
    /// Window length in seconds. Defaults to 1.
    #[serde(default = "default_one")]
    pub per_secs: u64,
}

impl RateLimitSettings {
    fn to_rate_limit(&self) -> RateLimit {
        RateLimit {
            max_events: self.max_events,
            per_secs: self.per_secs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// The `[logging.sampling]` block.
#[serde(deny_unknown_fields)]
pub struct SamplingSettings {
    /// Fraction of events kept, clamped to `0.0..=1.0` when applied.
    pub rate: f64,
    /// Events at this level or more severe bypass sampling entirely.
    #[serde(default = "default_trace")]
    pub min_level: String,
}

impl SamplingSettings {
    fn to_sample_config(&self) -> SampleConfig {
        let min_level = match self.min_level.as_str() {
            "error" => LevelFilter::ERROR,
            "warn" => LevelFilter::WARN,
            "info" => LevelFilter::INFO,
            "debug" => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        };
        SampleConfig {
            rate: self.rate.clamp(0.0, 1.0),
            min_level,
        }
    }
}

// ── Serde defaults ──────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}
fn default_one() -> u64 {
    1
}
fn default_rotation() -> String {
    "daily".into()
}
fn default_trace() -> String {
    "trace".into()
}

fn default_stream() -> String {
    "stderr".to_string()
}

fn default_timestamp() -> String {
    "utc".to_string()
}

/// 64 MiB: large enough that rolling is rare, small enough to open in an editor.
fn default_max_bytes() -> u64 {
    64 * 1024 * 1024
}

fn default_max_files() -> usize {
    5
}

fn default_prefix() -> String {
    "app.log".to_string()
}

fn default_file_format() -> String {
    "json".to_string()
}

/// Unrecognised values fall back to the default rather than failing the load.
///
/// A typo in one optional field should not stop a service from starting with
/// logging it can otherwise use.
fn parse_stream(value: &str) -> super::ConsoleTarget {
    match value.to_ascii_lowercase().as_str() {
        "stdout" => super::ConsoleTarget::Stdout,
        _ => super::ConsoleTarget::Stderr,
    }
}

fn parse_timestamp(value: &str) -> super::TimestampFormat {
    match value.to_ascii_lowercase().as_str() {
        "local" => super::TimestampFormat::Local,
        "none" => super::TimestampFormat::None,
        _ => super::TimestampFormat::Utc,
    }
}

fn parse_file_format(value: &str) -> super::FileFormat {
    match value.to_ascii_lowercase().as_str() {
        "text" => super::FileFormat::Text,
        _ => super::FileFormat::Json,
    }
}

impl Default for ConsoleSettings {
    fn default() -> Self {
        Self {
            stream: default_stream(),
            timestamp: default_timestamp(),
            color: true,
            thread_ids: true,
            target: true,
            lossy: false,
        }
    }
}

impl Default for JsonSettings {
    fn default() -> Self {
        Self {
            span_list: true,
            flatten: true,
            lossy: false,
        }
    }
}

impl Default for FileSettings {
    fn default() -> Self {
        Self {
            directory: String::new(),
            prefix: default_prefix(),
            format: default_file_format(),
            timestamp: default_timestamp(),
            rotation: default_rotation(),
            max_bytes: default_max_bytes(),
            max_files: default_max_files(),
            compress: false,
            retention_days: 0,
            span_list: true,
            flatten: true,
            lossy: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::test_helpers::write_temp;

    /// Read a `[logging]` block the way production does: through a real
    /// config store, so these tests cover the whole path from file to
    /// `Settings` rather than a parser the logging half no longer owns.
    async fn read_logging(toml: &str) -> Result<Settings, crate::config::Error> {
        let file = write_temp(toml);
        let store = crate::config::Builder::default()
            .toml(file.path(), 10)
            .build()
            .await
            .expect("the store builds from a temp file");
        Settings::read(&store, "logging")
    }

    #[tokio::test]
    async fn parse_minimal_config() {
        let cfg = read_logging("[logging]\nlevel = \"info\"\n").await.unwrap();
        assert_eq!(cfg.level.as_deref(), Some("info"));
        assert!(cfg.console.is_none());
        assert!(cfg.file.is_none());
        assert!(cfg.rate_limit.is_none());
    }

    #[tokio::test]
    async fn an_absent_block_is_reported_as_not_found() {
        let result = read_logging("[other]\nkey = 1\n").await;
        assert!(matches!(result, Err(crate::config::Error::NotFound(_))));
    }

    #[tokio::test]
    async fn rejects_unknown_keys_in_every_section() {
        let cases = [
            ("[logging]\nlevle = \"info\"", "levle"),
            ("[logging.console]\ncolour = true", "colour"),
            ("[logging.json]\nspan_lsit = true", "span_lsit"),
            (
                "[logging.file]\ndirectory = \"/tmp\"\nretention_day = 7",
                "retention_day",
            ),
            (
                "[logging.rate_limit]\nmax_events = 5\nper_sec = 1",
                "per_sec",
            ),
            (
                "[logging.sampling]\nrate = 0.5\nmin_levle = \"debug\"",
                "min_levle",
            ),
        ];

        for (input, unknown_key) in cases {
            let error = read_logging(input)
                .await
                .expect_err("unknown keys must be rejected")
                .to_string();
            assert!(
                error.contains(unknown_key),
                "error did not name {unknown_key:?}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn parse_full_config() {
        let toml = r#"
[logging]
level = "debug"
queue_size = 256_000
reloadable = true

[logging.console]
color = true
thread_ids = false
target = true
lossy = false

[logging.file]
directory = "/var/log/app"
rotation = "hourly"
retention_days = 30

[logging.rate_limit]
max_events = 50
per_secs = 1

[logging.sampling]
rate = 0.1
min_level = "debug"
"#;
        let cfg = read_logging(toml).await.unwrap();
        assert_eq!(cfg.level.as_deref(), Some("debug"));
        assert!(cfg.console.is_some());
        assert!(cfg.file.is_some());
        assert!(cfg.rate_limit.is_some());
        assert!(cfg.sampling.is_some());
        assert_eq!(cfg.queue_size, Some(256_000));
        assert_eq!(cfg.reloadable, Some(true));

        let fc = cfg.file.as_ref().unwrap();
        assert_eq!(fc.rotation, "hourly");
        assert_eq!(fc.retention_days, 30);

        let s = cfg.sampling.as_ref().unwrap();
        assert_eq!(s.rate, 0.1);
        assert_eq!(s.min_level, "debug");
    }

    #[tokio::test]
    async fn rate_limit_settings_default_per_secs_to_1() {
        let cfg = read_logging("[logging.rate_limit]\nmax_events = 10\n")
            .await
            .unwrap();
        let rl = cfg.rate_limit.unwrap();
        assert_eq!(rl.max_events, 10);
        assert_eq!(rl.per_secs, 1);
    }

    #[tokio::test]
    async fn sampling_rate_clamped() {
        let cfg = read_logging("[logging.sampling]\nrate = 15.0\n")
            .await
            .unwrap();
        let s = cfg.sampling.unwrap();
        assert_eq!(s.rate, 15.0); // stored as-is, clamped in to_sample_config
        assert_eq!(s.to_sample_config().rate, 1.0);
    }

    #[tokio::test]
    async fn from_store_produces_a_builder() {
        let toml = r#"
[logging]
level = "info"

[logging.console]
color = true

[logging.rate_limit]
max_events = 100
"#;
        let file = write_temp(toml);
        let store = crate::config::Builder::default()
            .toml(file.path(), 10)
            .build()
            .await
            .expect("the store builds");

        let builder = Settings::from_store(&store, "logging");
        assert!(builder.is_ok());
    }
}
