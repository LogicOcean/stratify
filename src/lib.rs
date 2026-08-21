//! Layered configuration and structured logging for Rust services.
//!
//! Two halves behind one crate:
//!
//! - [`config`] — stack configuration from files, environment variables and
//!   Azure App Configuration, merge by declared precedence, read typed values.
//!   Always available.
//! - [`logging`] — a non-blocking `tracing` facade with console, JSON, file
//!   and syslog sinks, per-sink filters, runtime reload, and optional Azure
//!   Application Insights export. Behind the `logging` feature, so a
//!   config-only user compiles none of it.
//!
//! The two meet in one direction only: logging can be *described* in
//! configuration ([`logging::settings::Settings`] reads a `[logging]` block
//! from a [`config::Store`]), and [`init`] stands both up in a single call.
//! The config half never depends on the logging half.
//!
//! ```rust,no_run
//! # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
//! use stratify::config;
//!
//! let store = config::Builder::default()
//!     .json("config/base.json", 100)
//!     .yaml("config/override.yaml", 50)
//!     .env("APP_", "__", 10)
//!     .build()
//!     .await?;
//!
//! let host = store.get_str("database.host");
//! # Ok(()) }
//! ```
//!
//! # Feature flags
//!
//! - `azure` — [`config::source::AzureAppConfigSource`], reading from Azure
//!   App Configuration. Off by default, because it brings in an HTTP stack
//!   that a file-and-environment user should not pay for.
//! - `logging` — the [`logging`] module and [`init`].
//! - `compression` — gzip retired log files (implies `logging`).
//! - `appinsights` — export to Azure Application Insights with trace
//!   correlation (implies `logging`).

// The config half has no need for `unsafe`, and neither does the logging half:
// even reading the Application Insights connection string goes through an
// injectable lookup rather than mutating the process environment. `forbid`
// rather than `deny` on purpose: `deny` can be switched off by an inner
// `#[allow]` in the same change that introduces the problem.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::doc_markdown)]

/// Layered configuration: pluggable sources, priority merging, typed access.
pub mod config;

/// Structured logging: a non-blocking `tracing` facade with pluggable sinks.
#[cfg(feature = "logging")]
pub mod logging;

#[cfg(feature = "logging")]
mod bootstrap;
#[cfg(feature = "logging")]
pub use bootstrap::{init, init_with, Bootstrap, InitError, Options};

#[doc(hidden)]
pub mod reexport {
    //! Re-exports used by the `file_source!` macro so downstream crates do not
    //! need `async-trait` as a direct dependency to define their own sources.
    pub use async_trait::async_trait;
}
