//! Error type for `ena-submit`.
//!
//! Library-internal code returns [`Error`]; the binary boundary (`main`) uses `anyhow` to render
//! these with context.

use std::path::PathBuf;

/// Errors produced by `ena-submit`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse TOML config {path}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("configuration error: {0}")]
    Config(String),

    /// One or more problems parsing/validating an input TSV. `message` may span multiple lines,
    /// one per offending row.
    #[error("{path}: {message}")]
    Input { path: PathBuf, message: String },

    /// Missing Webin credentials needed for a real (non-dry) operation.
    #[error(
        "missing Webin credentials: set WEBIN_USERNAME and WEBIN_PASSWORD \
         (or add them to ena-submit.toml)"
    )]
    MissingCredentials,

    #[error("refusing to overwrite existing file: {0}")]
    WouldOverwrite(PathBuf),

    /// A hard failure contacting the ENA taxonomy service (connection, HTTP status, malformed body).
    /// Distinct from per-row resolution problems (unknown/ambiguous names), which aggregate into
    /// [`Error::Input`].
    #[error("network error contacting {url}: {message}")]
    Network { url: String, message: String },

    /// One or more objects in a submission run failed to validate/submit. The per-object detail is
    /// in the history and Webin-CLI's own output; this drives a non-zero exit code.
    #[error("{failed} object(s) failed; see the history and the Webin-CLI output above")]
    SubmissionFailed { failed: usize },
}

/// Convenience alias for fallible library operations.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Build an [`Error::Io`] tagged with the offending path.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
