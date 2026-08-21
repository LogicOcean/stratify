//! Layered configuration: pluggable sources, priority merging, typed access.
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
//! # Precedence
//!
//! **Lower priority number wins.** A source at priority 10 overrides one at
//! 100. That is the opposite of treating the number as a weight, and it is
//! deliberate: it lets a more specific source be added later without
//! renumbering the ones already there.
//!
//! Merging is deep. Nested objects combine key by key rather than the
//! higher-precedence source replacing a whole subtree.

/// Fluent builder for assembling sources into a [`Store`].
pub mod builder;
/// Error type returned by loading and lookup.
pub mod error;
/// Built-in configuration sources and the [`Source`] trait.
pub mod source;
/// The merged, cached configuration store.
pub mod store;

pub use builder::Builder;
pub use error::Error;
pub use source::Source;
pub use store::Store;
