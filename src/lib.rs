//! Layered configuration for Rust.
//!
//! Stack configuration from files, environment variables and Azure App
//! Configuration, merge them by declared precedence, and read the result as
//! typed values.
//!
//! ```rust,no_run
//! # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
//! use stratify::ConfigBuilder;
//!
//! let store = ConfigBuilder::default()
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
//! # Precedence
//!
//! **Lower priority number wins.** A source at priority 10 overrides one at
//! 100. That is the opposite of treating the number as a weight, and it is
//! deliberate: it lets a more specific source be added later without
//! renumbering the ones already there.
//!
//! Merging is deep. Nested objects combine key by key rather than the
//! higher-precedence source replacing a whole subtree.
//!
//! # Feature flags
//!
//! - `azure` — [`AzureAppConfigSource`](source::AzureAppConfigSource), reading
//!   from Azure App Configuration. Off by default, because it brings in an HTTP
//!   stack that a file-and-environment user should not pay for.

// This crate has no need for `unsafe`, and a configuration library is not where
// anyone should be reaching for it. `forbid` rather than `deny` on purpose:
// `deny` can be switched off by an inner `#[allow]` in the same change that
// introduces the problem.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::doc_markdown)]

/// Fluent builder for assembling sources into a [`ConfigStore`].
pub mod builder;
/// Error type returned by loading and lookup.
pub mod error;
/// Built-in configuration sources and the [`Source`] trait.
pub mod source;
/// The merged, cached configuration store.
pub mod store;

pub use builder::ConfigBuilder;
pub use error::ConfigError;
pub use source::Source;
pub use store::ConfigStore;

#[doc(hidden)]
pub mod reexport {
    //! Re-exports used by the `file_source!` macro so downstream crates do not
    //! need `async-trait` as a direct dependency to define their own sources.
    pub use async_trait::async_trait;
}
