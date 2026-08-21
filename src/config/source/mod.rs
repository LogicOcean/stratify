pub(crate) mod nesting;

#[cfg(feature = "azure")]
mod azure_source;
#[cfg(feature = "azure")]
pub use azure_source::AzureAppConfigSource;

use crate::config::error::Error;
use async_trait::async_trait;
use serde_json::Value;

/// A source of configuration data, identified by a priority level.
///
/// Lower priority numbers = higher precedence (loaded later, overrides earlier sources).
/// Implementations must be `Send + Sync` for thread-safe use in [`Store`](crate::config::Store).
///
/// `load` is asynchronous so that sources can perform I/O over the network, such
/// as the Azure App Configuration source, without blocking a runtime thread.
/// File and environment sources simply do not await anything.
#[async_trait]
pub trait Source: Send + Sync {
    /// Human-readable name for debugging and logging (e.g., "json", "yaml", "env").
    fn name(&self) -> &str;

    /// Priority level. Lower numbers = higher priority when merging.
    fn priority(&self) -> u32;

    /// Load this source's configuration as a `serde_json::Value`.
    async fn load(&self) -> Result<Value, Error>;
}

/// Macro to generate a file-based configuration source.
///
/// Creates a struct that impls `Source` by reading a file and parsing its contents
/// with the given serde parser. Used internally to avoid duplicating JsonSource/YamlSource.
///
/// # Arguments
/// * `$name` — struct name (e.g. `JsonSource`)
/// * `$label` — human-readable name string (e.g. `"json"`)
/// * `$parser` — path to a deserializer function (e.g. `serde_json::from_str`)
#[macro_export]
macro_rules! file_source {
    ($name:ident, $label:expr, $parser:path) => {
        /// Configuration source that loads settings from a
        #[doc = $label]
        /// file.
        ///
        /// Lower priority numbers = higher precedence when merging.
        ///
        /// # Example
        /// ```
        #[doc = concat!("use stratify::config::source::", stringify!($name), ";\n")]
        #[doc = concat!("let source = ", stringify!($name), "::new(\"config.", $label, "\", 50);\n")]
        /// ```
        pub struct $name {
            path: std::path::PathBuf,
            priority: u32,
        }

        impl $name {
            /// Create a new source from a file path with the given priority.
            ///
            /// `priority` — lower numbers = higher precedence when merging.
            pub fn new(path: impl AsRef<std::path::Path>, priority: u32) -> Self {
                Self {
                    path: path.as_ref().to_path_buf(),
                    priority,
                }
            }
        }

        #[$crate::reexport::async_trait]
        impl $crate::config::source::Source for $name {
            fn name(&self) -> &str {
                $label
            }

            fn priority(&self) -> u32 {
                self.priority
            }

            async fn load(&self) -> Result<serde_json::Value, $crate::config::error::Error> {
                let content = std::fs::read_to_string(&self.path)?;
                let val: serde_json::Value = $parser(&content)?;
                Ok(val)
            }
        }
    };
}

mod dotenv_source;
mod env_source;
mod json_source;
mod toml_source;
mod yaml_source;

#[cfg(test)]
pub(crate) mod test_helpers;

pub use dotenv_source::DotEnvSource;
pub use env_source::EnvSource;
pub use json_source::JsonSource;
pub use toml_source::TomlSource;
pub use yaml_source::YamlSource;
