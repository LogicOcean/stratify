use thiserror::Error;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_norway::Error),

    #[error("TOML parse error: {0}")]
    Toml(String),

    #[error("Configuration source not found: {0}")]
    NotFound(String),

    #[error("Configuration merge conflict at key: {0}")]
    MergeConflict(String),

    #[error("{0}")]
    Other(String),
}
