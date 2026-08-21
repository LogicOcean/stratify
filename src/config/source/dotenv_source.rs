use crate::config::error::Error;
use crate::config::source::{EnvSource, Source};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

/// `.env` file configuration source.
///
/// Loads a `.env` file via [dotenvy], then captures matching environment variables
/// using the same prefix/separator semantics as [`EnvSource`].
///
/// # Example
/// ```rust,no_run
/// use stratify::config::source::DotEnvSource;
/// let source = DotEnvSource::new(".env", "APP_", "__", 10)?;
/// # Ok::<(), stratify::config::Error>(())
/// ```
pub struct DotEnvSource {
    inner: EnvSource,
}

impl DotEnvSource {
    /// Load a `.env` file, then capture matching environment variables.
    ///
    /// The file is read immediately so that a missing or unreadable path is an
    /// error here rather than a silently empty source at load time.
    ///
    /// `priority` follows the crate convention: lower numbers win.
    ///
    /// # Errors
    /// Returns [`Error::Other`] if the file cannot be read.
    pub fn new(
        path: impl AsRef<Path>,
        prefix: &str,
        separator: &str,
        priority: u32,
    ) -> Result<Self, Error> {
        dotenvy::from_path(path)
            .map_err(|e| Error::Other(format!("Failed to load .env file: {}", e)))?;
        Ok(Self {
            inner: EnvSource::new(prefix.to_string(), separator.to_string(), priority),
        })
    }
}

#[async_trait]
impl Source for DotEnvSource {
    fn name(&self) -> &str {
        "dotenv"
    }
    fn priority(&self) -> u32 {
        self.inner.priority()
    }
    async fn load(&self) -> Result<Value, Error> {
        self.inner.load().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::test_helpers::{write_temp, EnvGuard};
    use crate::config::source::Source;

    #[tokio::test]
    async fn loads_dotenv_file() {
        let f = write_temp("CK_DOT_T_KEY=from_dotfile\n");
        let source = DotEnvSource::new(f.path(), "CK_DOT_T_", "__", 0).unwrap();
        // RAII guard ensures cleanup even on panic (dotenvy already set the var)
        let _guard = EnvGuard::remove_on_drop("CK_DOT_T_KEY");
        let val = source.load().await.unwrap();
        assert_eq!(val["key"], "from_dotfile");
    }

    #[tokio::test]
    async fn missing_dotenv_file_is_error() {
        let result = DotEnvSource::new("/nonexistent/.env", "CK_DOT_T_", "__", 0);
        assert!(result.is_err());
    }
}
