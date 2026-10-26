//! IPC protocol envelope, frame kinds, and the wire error shape.

use serde::{Deserialize, Serialize};

pub mod methods;

pub use crate::IPC_VERSION;

/// Max JSONL line length in bytes, measured excluding the newline.
pub const MAX_LINE_BYTES: usize = 1_048_576;
/// Raw payload bytes per base64 chunk (256 KiB).
pub const CHUNK_RAW_BYTES: usize = 262_144;

/// Frame discriminator; wire values are lowercase per D-F17.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    Req,
    Res,
    Event,
    Cancel,
}

/// The only error shape allowed on the wire (D-F17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct IpcError {
    /// Machine-readable code (`unknown_method`, `auth_failed`, ...).
    pub code: String,
    /// Human-readable detail; never carries payload bytes or tokens.
    pub message: String,
    /// Whether the caller may usefully retry the same request.
    pub retryable: bool,
    /// Backend tag when the failure came from a platform backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

impl IpcError {
    /// Build a non-retryable error without a backend tag.
    #[must_use]
    pub fn new(code: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            message,
            retryable: false,
            backend: None,
        }
    }
}

/// JSONL envelope; every frame carries `v` + `id` + `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Protocol version, always `ipc/v1`.
    pub v: String,
    /// Client-chosen id; responses echo it, cancels reference it.
    pub id: String,
    /// Frame discriminator.
    pub kind: FrameKind,
    /// Method name on requests (and hello), e.g. `history.list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Method arguments on requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Method output on successful responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Failure detail on failed responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
    /// Per-connection sequence number, informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}
