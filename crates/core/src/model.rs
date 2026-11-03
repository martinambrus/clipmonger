//! Clip model types (seg-03 S1). Serde roundtrippable; unknown fields ignored.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Validate a clip content identity used by thumbnail lookup. Captures use
/// 16 lowercase hexadecimal digits; imported identities can use 64.
#[must_use]
pub fn valid_clip_content_hash(hash: &str) -> bool {
    matches!(hash.len(), 16 | 64)
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lamport-style last-writer-wins key for mutable annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rev {
    pub wall_ms: i64,
    pub dev: String,
    #[serde(default)]
    pub ctr: u64,
}

/// A single clipboard history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    /// v7 UUID: THE replication identity, stable forever.
    pub id: Uuid,
    /// Unix millis, UTC.
    pub created_ms: i64,
    /// Capturing app, when `ForegroundApp` available.
    #[serde(default)]
    pub source_app: Option<String>,
    /// Capture OS: "windows"|"linux"|"macos".
    pub source_os: String,
    #[serde(default)]
    pub pinned: bool,
    /// Soft-delete flag (storage v1). No `sensitive` flag by design.
    #[serde(default)]
    pub removed: bool,
    /// Display snippet, re-renderable from payload.
    #[serde(default)]
    pub preview: String,
    /// Stable searchable text, width-independent.
    #[serde(default)]
    pub search_text: String,
    /// Canonical MIME list (seg-03 S2).
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub bytes_total: u64,
    /// Hex SHA-256, LOCAL dedupe only, never a distributed key.
    #[serde(default)]
    pub content_hash: String,
    /// LWW key for mutable annotations.
    pub rev: Rev,
    /// Move-to-top ordering.
    #[serde(default)]
    pub last_used_ms: i64,
    /// Capturing device id (v3 provenance).
    #[serde(default)]
    pub origin_dev: String,
    /// Capture backend: windows|x11|wayland|gnome|macos.
    #[serde(default)]
    pub origin: String,
}

/// Permanent clip kind. `System` = `_System` group (hotkey-only, never popup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermanentKind {
    Paste,
    Macro,
    System,
}

/// A permanent (pinned-by-user) clip: paste text, macro, or system action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentClip {
    pub id: Uuid,
    pub group: String,
    pub name: String,
    /// Chord string, e.g. "ctrl+alt+1".
    #[serde(default)]
    pub hotkey: Option<String>,
    pub kind: PermanentKind,
    /// Text or macro source.
    pub body: String,
    /// v2: JS stage source.
    #[serde(default)]
    pub js: Option<String>,
    /// LWW key for sync.
    pub rev: Rev,
}

#[cfg(test)]
mod content_hash_tests {
    use super::valid_clip_content_hash;
    #[test]
    fn thumbnail_identity_accepts_only_supported_lowercase_hex_lengths() {
        for len in [16, 64] {
            assert!(valid_clip_content_hash(&"a".repeat(len)));
            assert!(!valid_clip_content_hash(&"A".repeat(len)));
            assert!(!valid_clip_content_hash(&"g".repeat(len)));
        }
        for len in [0, 15, 17, 63, 65] {
            assert!(!valid_clip_content_hash(&"a".repeat(len)));
        }
    }
}
