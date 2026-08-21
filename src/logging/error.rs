//! The logging half's error type.

use thiserror::Error as ThisError;

/// Everything that can go wrong building, installing, or driving the logging
/// stack.
///
/// Startup errors carry enough to act on: a bad filter names the sink that
/// owns it, and a bad settings block names the key. Runtime methods that can
/// fail (`reload`, `flush`) return rather than panic, because a logging
/// failure must never take the process with it.
#[derive(Debug, ThisError)]
#[non_exhaustive]
pub enum Error {
    /// A log file or directory could not be created or written.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// `init` was called while a subscriber from this facade is already
    /// installed.
    #[error("logging is already initialized")]
    AlreadyInitialized,

    /// A handle method needing the installed subscriber ran before `init`.
    #[error("logging is not initialized")]
    NotInitialized,

    /// `reload` was called on a stack built without `.reloadable(true)`.
    #[error("runtime filter reload was not enabled during init")]
    ReloadNotEnabled,

    /// A lock inside the facade was poisoned by a panicking thread.
    #[error("the logging state lock is poisoned")]
    LockPoisoned,

    /// A per-sink filter directive failed to parse; the message names the
    /// sink.
    #[error("invalid filter directives for {0}")]
    InvalidFilter(String),

    /// Application Insights configuration or export failed.
    #[error("Application Insights: {0}")]
    AppInsights(String),

    /// Installing the global subscriber failed outside this facade's control.
    #[error("failed to install the global tracing subscriber: {0}")]
    SetGlobalDefault(#[from] tracing::subscriber::SetGlobalDefaultError),

    /// The reload handle rejected the new filter.
    #[error("failed to reload the runtime filter: {0}")]
    FilterReload(#[from] tracing_subscriber::reload::Error),

    /// A filter string failed to parse.
    #[error("invalid filter directive: {0}")]
    FilterParse(#[from] tracing_subscriber::filter::ParseError),

    /// The `[logging]` settings block could not be read from the
    /// configuration store.
    #[error("could not read logging settings: {0}")]
    Settings(#[from] crate::config::Error),

    /// The settings block deserialized but asks for something invalid or
    /// unavailable: a bad level, an unknown facility, a feature that is not
    /// compiled in. The message names the key.
    #[error("invalid logging settings: {0}")]
    InvalidSettings(String),
}
