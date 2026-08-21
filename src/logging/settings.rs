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

    /// Per-sink filter directives, overriding the global filter for one sink.
    #[serde(default)]
    pub filters: Option<FilterSettings>,

    /// Field names whose values are masked in rendered text output.
    #[serde(default)]
    pub redact: Option<Vec<String>>,

    /// Route panics through the subscriber with payload, file and line.
    #[serde(default)]
    pub capture_panics: Option<bool>,

    /// Process-wide fields attached to every line of the text sinks, such as
    /// a service name or version. A map so the file reads as `key = "value"`.
    #[serde(default)]
    pub global_fields: Option<std::collections::BTreeMap<String, String>>,

    /// Syslog sink; absent means no syslog output. Unix only, inert elsewhere.
    #[serde(default)]
    pub syslog: Option<SyslogSettings>,

    /// Application Insights export; absent means none.
    ///
    /// Applied by [`from_store`](Self::from_store), which resolves the
    /// connection string, never by [`to_builder`](Self::to_builder) — see
    /// there for why.
    #[serde(default)]
    pub app_insights: Option<AppInsightsSettings>,
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
    pub fn to_builder(&self) -> Result<super::Builder, Error> {
        // The block that cannot be honored here fails here, not silently:
        // resolving a connection string needs the store, so a caller holding
        // only `Settings` has to go through `from_store`.
        if self.app_insights.is_some() {
            return Err(Error::InvalidSettings(
                "app_insights: this block is applied by Settings::from_store, which can \
                 resolve the connection string; build from the store, or wire \
                 AppInsightsConfig on the builder yourself"
                    .to_string(),
            ));
        }
        self.base_builder()
    }

    /// Everything except `app_insights`, which needs the store.
    ///
    /// Split along the same lines as the file format: sinks, then behaviour,
    /// so each helper stays small enough to read against its block.
    fn base_builder(&self) -> Result<super::Builder, Error> {
        let builder = self.apply_sinks(super::builder())?;
        self.apply_behaviour(builder)
    }

    /// The output blocks: `console`, `json`, `file`, `syslog`.
    fn apply_sinks(&self, mut builder: super::Builder) -> Result<super::Builder, Error> {
        if let Some(c) = &self.console {
            builder = builder.console(c.to_console_config());
        }
        if let Some(j) = &self.json {
            builder = builder.json(j.to_json_config());
        }
        if let Some(fc) = &self.file {
            builder = builder.file(fc.to_file_config());
        }
        if let Some(s) = &self.syslog {
            builder = builder.syslog(s.to_syslog_config()?);
        }
        Ok(builder)
    }

    /// Everything that shapes what the sinks see: level, per-sink filters,
    /// gates, redaction, panic capture, global fields, queue size, reload.
    fn apply_behaviour(&self, mut builder: super::Builder) -> Result<super::Builder, Error> {
        if let Some(level) = &self.level {
            // A typo here must be a startup error naming the key, not a
            // filter that silently matches nothing.
            let filter = EnvFilter::builder()
                .parse(level)
                .map_err(|e| Error::InvalidSettings(format!("level: {e}")))?;
            builder = builder.with_filter(filter);
        }
        if let Some(filters) = &self.filters {
            builder = self.apply_sink_filters(builder, filters)?;
        }
        if let Some(rl) = &self.rate_limit {
            builder = builder.rate_limit(rl.to_rate_limit());
        }
        if let Some(s) = &self.sampling {
            builder = builder.sampling(s.to_sample_config());
        }
        if let Some(keys) = &self.redact {
            builder = builder.redact(keys.iter());
        }
        if self.capture_panics.unwrap_or(false) {
            builder = builder.capture_panics();
        }
        if let Some(fields) = &self.global_fields {
            for (key, value) in fields {
                builder = builder.global_field(key, value);
            }
        }
        if let Some(qs) = self.queue_size {
            builder = builder.queue_size(qs);
        }
        if self.reloadable.unwrap_or(false) {
            builder = builder.reloadable();
        }
        Ok(builder)
    }

    /// The `[filters]` block, one directive per sink.
    fn apply_sink_filters(
        &self,
        mut builder: super::Builder,
        filters: &FilterSettings,
    ) -> Result<super::Builder, Error> {
        if let Some(d) = &filters.console {
            builder = builder.console_filter(d);
        }
        if let Some(d) = &filters.json {
            builder = builder.json_filter(d);
        }
        if let Some(d) = &filters.file {
            builder = builder.file_filter(d);
        }
        if let Some(d) = &filters.syslog {
            builder = builder.syslog_filter(d);
        }
        if let Some(d) = &filters.app_insights {
            builder = self.app_insights_filter(builder, d)?;
        }
        Ok(builder)
    }

    /// Apply the Application Insights filter directives, or reject them when
    /// the feature they filter is not compiled in.
    #[cfg(feature = "appinsights")]
    fn app_insights_filter(
        &self,
        builder: super::Builder,
        directives: &str,
    ) -> Result<super::Builder, Error> {
        Ok(builder.app_insights_filter(directives))
    }

    #[cfg(not(feature = "appinsights"))]
    fn app_insights_filter(
        &self,
        _builder: super::Builder,
        _directives: &str,
    ) -> Result<super::Builder, Error> {
        Err(Error::InvalidSettings(
            "filters.app_insights: the appinsights feature is not enabled in this build"
                .to_string(),
        ))
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
        Self::read(store, key)?.apply(store)
    }

    /// Turn these settings into a builder, resolving anything that needs the
    /// store — today, the `[app_insights]` connection string.
    ///
    /// [`from_store`](Self::from_store) is `read` followed by this.
    ///
    /// # Errors
    ///
    /// Everything [`to_builder`](Self::to_builder) can report, plus
    /// [`Error::InvalidSettings`] when the `app_insights` block names a key
    /// the store cannot answer: an explicit block is intent, so a missing
    /// secret is a failure rather than a silently absent exporter.
    pub fn apply(&self, store: &crate::config::Store) -> Result<super::Builder, Error> {
        let builder = self.base_builder()?;
        self.apply_app_insights(builder, store)
    }

    #[cfg(feature = "appinsights")]
    fn apply_app_insights(
        &self,
        builder: super::Builder,
        store: &crate::config::Store,
    ) -> Result<super::Builder, Error> {
        let Some(ai) = &self.app_insights else {
            return Ok(builder);
        };
        let key = ai
            .connection_string_key
            .clone()
            .unwrap_or_else(|| super::appinsights::CONNECTION_STRING_VAR.to_lowercase());
        let connection = store.get_str(&key).ok_or_else(|| {
            Error::InvalidSettings(format!(
                "app_insights: the store has no value under {key:?}; the block names \
                 where the connection string lives, and an explicit block with a \
                 missing secret is a failure, not an absent exporter"
            ))
        })?;
        let mut config = super::appinsights::AppInsightsConfig::new(connection, &ai.service_name);
        if let Some(rate) = ai.sample_rate {
            config = config.with_sample_rate(rate);
        }
        Ok(builder.app_insights(config))
    }

    #[cfg(not(feature = "appinsights"))]
    fn apply_app_insights(
        &self,
        builder: super::Builder,
        _store: &crate::config::Store,
    ) -> Result<super::Builder, Error> {
        if self.app_insights.is_some() {
            return Err(Error::InvalidSettings(
                "app_insights: the appinsights feature is not enabled in this build".to_string(),
            ));
        }
        Ok(builder)
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

/// The `[logging.filters]` block: per-sink filter directives.
///
/// Directives use `RUST_LOG` syntax and are validated at build time, so a typo
/// is a startup error naming the sink rather than a filter that silently
/// matches nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterSettings {
    /// Console sink directives.
    #[serde(default)]
    pub console: Option<String>,
    /// JSON sink directives.
    #[serde(default)]
    pub json: Option<String>,
    /// File sink directives.
    #[serde(default)]
    pub file: Option<String>,
    /// Syslog sink directives.
    #[serde(default)]
    pub syslog: Option<String>,
    /// Application Insights directives — the usual reason this block exists,
    /// because the exporter is the sink that costs money per event.
    #[serde(default)]
    pub app_insights: Option<String>,
}

/// The `[logging.syslog]` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyslogSettings {
    /// The tag identifying this process in each message.
    pub tag: String,
    /// `"user"`, `"daemon"` (default), or `"local0"` through `"local7"`.
    #[serde(default)]
    pub facility: Option<String>,
}

impl SyslogSettings {
    fn to_syslog_config(&self) -> Result<super::syslog::SyslogConfig, Error> {
        use super::syslog::Facility;
        let facility = match self.facility.as_deref() {
            None => Facility::Daemon,
            Some("user") => Facility::User,
            Some("daemon") => Facility::Daemon,
            Some("local0") => Facility::Local0,
            Some("local1") => Facility::Local1,
            Some("local2") => Facility::Local2,
            Some("local3") => Facility::Local3,
            Some("local4") => Facility::Local4,
            Some("local5") => Facility::Local5,
            Some("local6") => Facility::Local6,
            Some("local7") => Facility::Local7,
            Some(other) => {
                return Err(Error::InvalidSettings(format!(
                    "syslog.facility: unknown facility {other:?}; expected user, daemon, or local0..local7"
                )))
            }
        };
        Ok(super::syslog::SyslogConfig::new(&self.tag).with_facility(facility))
    }
}

/// The `[logging.app_insights]` block.
///
/// The connection string is a secret, so the block names the *key* it is found
/// under rather than holding the value: the file stays safe to commit, and the
/// secret arrives through the store — environment, `.env`, or a vault-backed
/// source — like everything else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppInsightsSettings {
    /// Reported as the service name, so several services in one workspace can
    /// be told apart. Required: nothing sensible can be defaulted.
    pub service_name: String,
    /// Store key holding the connection string. Defaults to
    /// `applicationinsights_connection_string`, the conventional variable
    /// lowercased the way the environment source stores it.
    #[serde(default)]
    pub connection_string_key: Option<String>,
    /// Fraction of traces exported, `0.0..=1.0`. Defaults to `1.0`. Log
    /// records are not sampled — this bounds the span volume, which is what
    /// grows with traffic.
    #[serde(default)]
    pub sample_rate: Option<f64>,
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
    async fn the_full_surface_parses_from_one_block() {
        let toml = r#"
[logging]
level = "info"
redact = ["password", "authorization"]
capture_panics = true

[logging.filters]
console = "warn"
file = "debug"

[logging.global_fields]
service = "nse-api"
version = "1.0.0"

[logging.syslog]
tag = "nse"
facility = "local3"

[logging.console]
color = false
"#;
        let cfg = read_logging(toml).await.unwrap();
        assert_eq!(
            cfg.redact.as_deref(),
            Some(&["password".to_string(), "authorization".to_string()][..])
        );
        assert_eq!(cfg.capture_panics, Some(true));
        let filters = cfg.filters.as_ref().unwrap();
        assert_eq!(filters.console.as_deref(), Some("warn"));
        assert_eq!(filters.file.as_deref(), Some("debug"));
        let fields = cfg.global_fields.as_ref().unwrap();
        assert_eq!(fields.get("service").map(String::as_str), Some("nse-api"));
        assert_eq!(cfg.syslog.as_ref().unwrap().tag, "nse");

        // And the whole thing builds.
        assert!(cfg.to_builder().is_ok());
    }

    #[tokio::test]
    async fn an_unknown_syslog_facility_is_a_startup_error_naming_the_key() {
        let toml = r#"
[logging.syslog]
tag = "nse"
facility = "local9"
"#;
        let cfg = read_logging(toml).await.unwrap();
        let error = match cfg.to_builder() {
            Err(e) => e.to_string(),
            Ok(_) => panic!("local9 does not exist"),
        };
        assert!(error.contains("syslog.facility"), "got: {error}");
        assert!(error.contains("local9"), "got: {error}");
    }

    #[tokio::test]
    async fn an_invalid_level_is_a_startup_error_not_a_silent_default() {
        let toml = r#"
[logging]
level = "not[a]filter"
"#;
        let cfg = read_logging(toml).await.unwrap();
        let error = match cfg.to_builder() {
            Err(e) => e.to_string(),
            Ok(_) => panic!("the level must parse"),
        };
        assert!(error.contains("level"), "got: {error}");
    }

    #[cfg(feature = "appinsights")]
    #[tokio::test]
    async fn to_builder_refuses_an_app_insights_block_it_cannot_resolve() {
        // Arrange: the block needs the store; a caller holding only the
        // settings must be sent to from_store rather than silently losing
        // the exporter they configured.
        let toml = r#"
[logging.app_insights]
service_name = "nse-api"
"#;
        let cfg = read_logging(toml).await.unwrap();

        // Act
        let error = match cfg.to_builder() {
            Err(e) => e.to_string(),
            Ok(_) => panic!("the block needs the store"),
        };

        // Assert
        assert!(error.contains("from_store"), "got: {error}");
    }

    #[cfg(feature = "appinsights")]
    #[tokio::test]
    async fn an_app_insights_block_with_no_secret_in_the_store_fails_loudly() {
        // Arrange: an explicit block is intent; a missing secret must not
        // become a silently absent exporter.
        let toml = r#"
[logging.app_insights]
service_name = "nse-api"
"#;
        let file = write_temp(toml);
        let store = crate::config::Builder::default()
            .toml(file.path(), 10)
            .build()
            .await
            .expect("store builds");

        // Act
        let error = match Settings::from_store(&store, "logging") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("no connection string anywhere"),
        };

        // Assert: names the key it looked under.
        assert!(
            error.contains("applicationinsights_connection_string"),
            "got: {error}"
        );
    }

    #[cfg(feature = "appinsights")]
    #[tokio::test]
    async fn an_app_insights_block_resolves_its_secret_through_the_store() {
        // Arrange: the connection string sits in the store under the block's
        // named key, the way the environment or a vault source would put it.
        let toml = r#"
connection = "InstrumentationKey=00000000-0000-0000-0000-000000000000"

[logging.app_insights]
service_name = "nse-api"
connection_string_key = "connection"
sample_rate = 0.25
"#;
        let file = write_temp(toml);
        let store = crate::config::Builder::default()
            .toml(file.path(), 10)
            .build()
            .await
            .expect("store builds");

        // Act / Assert: resolving succeeds; the exporter itself is lazy, so
        // no network is contacted here.
        assert!(Settings::from_store(&store, "logging").is_ok());
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
