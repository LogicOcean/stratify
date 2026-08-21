//! Shared test utilities for stratify source tests.
//!
//! Items carry `#[allow(dead_code)]` because not every source test module uses
//! every helper.

#[allow(dead_code)]
pub(crate) fn write_temp(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("failed to create temp file");
    std::io::Write::write_all(&mut f, content.as_bytes()).expect("failed to write test data");
    f
}

/// RAII guard that removes an environment variable on drop.
/// Prevents test pollution even when a test panics.
#[allow(dead_code)]
pub(crate) struct EnvGuard {
    key: String,
}

#[allow(dead_code)]
impl EnvGuard {
    /// Set an environment variable and remove it on drop.
    pub fn set(key: impl Into<String>, val: &str) -> Self {
        let key = key.into();
        std::env::set_var(&key, val);
        EnvGuard { key }
    }

    /// Register a key for removal on drop, without setting it first.
    /// Useful when another mechanism (e.g. dotenvy) has already set the variable.
    pub fn remove_on_drop(key: impl Into<String>) -> Self {
        EnvGuard { key: key.into() }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(&self.key);
    }
}
