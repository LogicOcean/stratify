pub mod builder;
pub mod error;
pub mod source;
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
