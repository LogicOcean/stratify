//! One call that reads configuration and stands up logging, in that order.
//!
//! This is the only code in the crate that needs both halves at once, which is
//! why it lives here rather than in either of them: the config half must never
//! depend on the logging half, and the logging half only ever *reads from* a
//! store it is handed.

use std::path::{Path, PathBuf};

use crate::config;
use crate::logging;
use crate::logging::settings::Settings;

/// What [`init`] hands back: the configuration everything else reads, and the
/// handle that keeps the logging workers alive.
#[non_exhaustive]
pub struct Bootstrap {
    /// The merged configuration store.
    pub config: config::Store,
    /// Keep this for the process lifetime; it is what `flush` and `shutdown`
    /// are driven through.
    pub logging: logging::Handle,
}

/// Everything that can go wrong before the first log line.
///
/// Split from the halves' own errors because nothing can be *logged* while
/// this is failing — the subscriber is not installed yet — so the message has
/// to carry the whole story on its own.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InitError {
    /// The configuration store could not be built.
    #[error("could not load configuration: {0}")]
    Config(#[from] config::Error),
    /// The `[logging]` block was present but invalid, or the stack could not
    /// be installed.
    #[error("could not initialize logging: {0}")]
    Logging(#[from] logging::Error),
}

/// How [`init_with`] builds the stack. [`Options::new`] gives the
/// conventional setup; every field is overridable from there.
#[non_exhaustive]
pub struct Options {
    /// Recorded on every Application Insights record, and useful in file
    /// prefixes. Required because nothing sensible can be defaulted.
    pub service_name: String,
    /// TOML file read at the lowest precedence, skipped silently when absent.
    /// `config.toml` by convention.
    pub config_file: PathBuf,
    /// Read a `.env` file that *overrides* the environment. On by default: a
    /// value written in the file should not lose to a shell variable someone
    /// exported last week. The corollary is that a `.env` must never reach a
    /// deployed image.
    pub dotenv: bool,
    /// The store key holding the [`Settings`] block. `"logging"` by
    /// convention. An absent block means defaults (console at `info`), not an
    /// error; a present-but-invalid block is a startup error.
    pub logging_key: String,
    /// Prefix filter for the environment source. Empty by default, capturing
    /// the whole environment: platform-injected values — connection strings
    /// above all — have conventional names, not an application prefix, and
    /// must be reachable without listing each one in advance. The trade is
    /// that everything else (`PATH` included) sits in the store too; the store
    /// is never logged by this crate, but a caller that dumps it wholesale
    /// should set a prefix here instead.
    pub env_prefix: String,
}

impl Options {
    /// The conventional stack for `service_name`.
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            config_file: PathBuf::from("config.toml"),
            dotenv: true,
            logging_key: "logging".to_string(),
            env_prefix: String::new(),
        }
    }
}

/// Read configuration, then stand up logging from it, then install the global
/// subscriber. The conventional form of [`init_with`].
///
/// Precedence, lowest to highest: `config.toml` (when present) < environment
/// < `.env` (when present). The environment is captured un-prefixed so that
/// platform-injected values — connection strings above all — are reachable
/// without naming each one in advance.
///
/// # Errors
///
/// [`InitError`], carrying either half's failure. Nothing has been logged when
/// this returns `Err` — the subscriber was never installed — so the error
/// message is the only record and names what failed.
pub async fn init(service_name: impl Into<String>) -> Result<Bootstrap, InitError> {
    init_with(Options::new(service_name)).await
}

/// [`init`] with the stack spelled out.
///
/// # Errors
///
/// See [`init`].
pub async fn init_with(options: Options) -> Result<Bootstrap, InitError> {
    let store = build_store(&options).await?;

    // The settings block is optional; anything else wrong with it is not.
    // Distinguishing here is the whole reason `Settings::read` exposes the
    // NotFound case instead of folding it into one error.
    let settings = match Settings::read(&store, &options.logging_key) {
        Ok(settings) => Some(settings),
        Err(config::Error::NotFound(_)) => None,
        Err(e) => return Err(InitError::Logging(logging::Error::Settings(e))),
    };

    let builder = logging_builder(&settings, &store)?;
    let builder = apply_store_level(builder, &settings, &store)?;
    let builder = auto_app_insights(builder, &settings, &store, &options.service_name);

    let handle = builder.init()?;

    // The first record the subscriber carries explains where configuration
    // came from. Without this line a wrong precedence stack is invisible.
    tracing::info!(
        service = %options.service_name,
        config_file = %describe_file(&options.config_file),
        dotenv = options.dotenv,
        logging_key = %options.logging_key,
        "configuration loaded, logging installed"
    );

    Ok(Bootstrap {
        config: store,
        logging: handle,
    })
}

/// The builder the settings describe, or the console-only default when no
/// block exists.
///
/// `apply` resolves anything needing the store — the `app_insights` block
/// above all — so an explicit block is honored or fails loudly here.
fn logging_builder(
    settings: &Option<Settings>,
    store: &config::Store,
) -> Result<logging::Builder, InitError> {
    Ok(match settings {
        Some(settings) => settings.apply(store)?,
        None => logging::builder().console(logging::ConsoleConfig::default()),
    })
}

/// With no explicit level, fall back to `rust_log` *from the store* rather
/// than the process environment.
///
/// The distinction matters: the builder's own fallback reads `std::env`,
/// which a `.env` loaded as a source never reaches, so a `RUST_LOG` written
/// there would silently lose to a shell export — the exact inversion this
/// crate's `.env` precedence promises. Sourced from configuration, a typo is
/// a startup error, not a lenient best-effort parse.
fn apply_store_level(
    builder: logging::Builder,
    settings: &Option<Settings>,
    store: &config::Store,
) -> Result<logging::Builder, InitError> {
    if settings.as_ref().and_then(|s| s.level.as_deref()).is_some() {
        return Ok(builder);
    }
    let Some(directives) = store.get_str("rust_log") else {
        return Ok(builder);
    };
    let filter = logging::EnvFilter::builder()
        .parse(&directives)
        .map_err(|e| {
            InitError::Logging(logging::Error::InvalidSettings(format!("rust_log: {e}")))
        })?;
    Ok(builder.with_filter(filter))
}

/// Turn the exporter on when the conventional connection string is reachable
/// and no settings block claimed it.
///
/// An explicit `[logging.app_insights]` already resolved (or failed) inside
/// `Settings::apply`, and detecting on top of it would double-wire. Present
/// means on: the connection string is a secret, so it arrives through the
/// store (environment, `.env`, or a vault-backed source) and never sits in
/// the TOML itself. Its absence is a choice, not a failure.
#[cfg(feature = "appinsights")]
fn auto_app_insights(
    builder: logging::Builder,
    settings: &Option<Settings>,
    store: &config::Store,
    service_name: &str,
) -> logging::Builder {
    if settings.as_ref().is_some_and(|s| s.app_insights.is_some()) {
        return builder;
    }
    let lookup = |name: &str| store.get_str(&name.to_lowercase());
    match logging::appinsights::AppInsightsConfig::from_lookup(service_name, lookup) {
        Ok(config) => builder.app_insights(config),
        Err(_) => builder,
    }
}

#[cfg(not(feature = "appinsights"))]
fn auto_app_insights(
    builder: logging::Builder,
    _settings: &Option<Settings>,
    _store: &config::Store,
    _service_name: &str,
) -> logging::Builder {
    builder
}

/// Assemble the store for [`Options`]. Split out so the precedence numbers
/// live in one place.
async fn build_store(options: &Options) -> Result<config::Store, config::Error> {
    // Lower number wins. The file sits at the bottom on purpose: it is the
    // committed default, and both the environment and `.env` exist to beat it.
    const FILE_PRIORITY: u32 = 100;
    const ENV_PRIORITY: u32 = 50;
    const DOTENV_PRIORITY: u32 = 10;

    let mut builder = config::Builder::default().env(&options.env_prefix, "__", ENV_PRIORITY);

    if options.config_file.exists() {
        builder = builder.toml(&options.config_file, FILE_PRIORITY);
    }
    if options.dotenv && Path::new(".env").exists() {
        builder = builder.dotenv(".env", "", "__", DOTENV_PRIORITY)?;
    }

    builder.build().await
}

/// Render the config-file field for the install line: the path when it was
/// read, or an explicit absence.
fn describe_file(path: &Path) -> String {
    if path.exists() {
        path.display().to_string()
    } else {
        format!("{} (absent, skipped)", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_to_the_conventional_stack() {
        // Arrange / Act
        let options = Options::new("svc");

        // Assert
        assert_eq!(options.service_name, "svc");
        assert_eq!(options.config_file, PathBuf::from("config.toml"));
        assert!(options.dotenv);
        assert_eq!(options.logging_key, "logging");
        assert_eq!(options.env_prefix, "");
    }

    #[test]
    fn describe_file_marks_an_absent_path() {
        // Arrange
        let path = PathBuf::from("definitely-not-here-9f3a.toml");

        // Act
        let rendered = describe_file(&path);

        // Assert
        assert!(rendered.contains("absent"), "got: {rendered}");
    }

    #[tokio::test]
    async fn init_with_reports_an_invalid_logging_block() {
        // Arrange: a present-but-wrong block must fail loudly, unlike an
        // absent one. The store is built from a temp file only.
        let file = crate::config::source::test_helpers::write_temp("[logging]\nlevel = 3\n");
        let mut options = Options::new("svc");
        options.config_file = file.path().to_path_buf();
        options.dotenv = false;

        // Act
        let result = init_with(options).await;

        // Assert
        assert!(matches!(result, Err(InitError::Logging(_))));
    }
}
