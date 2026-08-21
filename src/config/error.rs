use thiserror::Error;

/// Errors returned when loading, merging or reading configuration.
///
/// Marked `#[non_exhaustive]`: new sources may bring new failure modes, and
/// adding one should not be a breaking change for callers matching on this.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// A source file could not be read.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON source was not valid JSON.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// A YAML source was not valid YAML.
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_norway::Error),

    /// A TOML source was not valid TOML.
    #[error("TOML parse error: {0}")]
    Toml(String),

    /// A requested key is absent from the merged configuration.
    #[error("Configuration source not found: {0}")]
    NotFound(String),

    /// Two keys disagree about the shape of the tree: one requires a value
    /// where another requires an object, so no merge can satisfy both.
    #[error("Configuration merge conflict at key: {0}")]
    MergeConflict(String),

    /// A failure that does not fit the categories above, such as a network
    /// error from a remote source.
    #[error("{0}")]
    Other(String),
}
