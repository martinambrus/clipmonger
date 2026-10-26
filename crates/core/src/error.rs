//! Typed errors for `clipmonger-core`. Never carry clip payloads.

use thiserror::Error;

/// Backend failure kind (seg-02 S5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackendKind {
    Unavailable,
    Denied,
    Timeout,
    TooLarge,
    Unsupported,
    Io,
    Internal,
}

/// Core error type.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Backend-mapped failure. `message` holds metadata only, never payloads.
    #[error("backend {backend} {kind:?}: {message}")]
    Backend {
        backend: String,
        kind: BackendKind,
        message: String,
    },
    /// Newer schema opened by older code: clean refusal, zero writes.
    #[error("downgrade refused: found schema {found}, code supports {expected}")]
    DowngradeRefused { found: i64, expected: i64 },
    /// Migration failure: rolled back into read-only recovery.
    #[error("migration to version {version} failed: {message}")]
    MigrationFailed { version: u32, message: String },
    /// Verified corruption: DB preserved aside, never silently dropped.
    #[error("corrupt store: {message}")]
    Corrupt { message: String },
    /// Payload over the applicable cap; stored as metadata-only stub.
    #[error("too large: {message}")]
    TooLarge { message: String },
    /// Format or operation not supported on this backend.
    #[error("unsupported: {message}")]
    Unsupported { message: String },
    /// Config parse/validation failure.
    #[error("config: {message}")]
    Config { message: String },
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl CoreError {
    /// Machine-readable code for IPC serialization.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Backend { .. } => "backend",
            Self::DowngradeRefused { .. } => "downgrade_refused",
            Self::MigrationFailed { .. } => "migration_failed",
            Self::Corrupt { .. } => "corrupt",
            Self::TooLarge { .. } => "too_large",
            Self::Unsupported { .. } => "unsupported",
            Self::Config { .. } => "config",
            Self::Io(_) => "io",
        }
    }
}
