//! History policy: dedupe, retention, oversize stubs (D-F3/F9/F10).

use uuid::Uuid;

/// Inline threshold per payload: 256 KiB (D-F3, KiB = 1024).
pub const INLINE_THRESHOLD_BYTES: u64 = 256 * 1024;

/// Payload storage class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    Inline,
    External,
}

/// Classify one payload by byte length.
#[must_use]
pub fn storage_class(len_bytes: u64) -> StorageClass {
    if len_bytes <= INLINE_THRESHOLD_BYTES {
        StorageClass::Inline
    } else {
        StorageClass::External
    }
}

/// True when a capture needs a metadata-only stub (user-visible
/// "too large" state, not silent loss) instead of stored payloads.
#[must_use]
pub fn needs_stub(largest_payload_bytes: u64, max_bytes: u64) -> bool {
    largest_payload_bytes > max_bytes
}

/// Minimal candidate view for the dedupe window scan.
pub struct DedupeCandidate {
    pub id: Uuid,
    pub content_hash: String,
    pub removed: bool,
    pub last_used_ms: i64,
}

/// Find the dedupe hit (D-F10): scan the last `window` non-removed clips
/// ordered by `last_used_ms` descending; the caller updates only
/// `last_used_ms` (move-to-top) on hit.
#[must_use]
pub fn find_dedupe(candidates: &[DedupeCandidate], hash: &str, window: usize) -> Option<Uuid> {
    let mut live: Vec<&DedupeCandidate> = candidates.iter().filter(|c| !c.removed).collect();
    live.sort_by_key(|a| std::cmp::Reverse(a.last_used_ms));
    live.into_iter()
        .take(window)
        .find(|c| c.content_hash == hash)
        .map(|c| c.id)
}

/// Effective retention caps after `privacy.strict` halving (D-F9):
/// floor division; `max_age_days == 0` (off) stays off.
#[must_use]
pub fn effective_limits(
    max_items: u64,
    max_store_bytes: u64,
    max_age_days: u64,
    strict: bool,
) -> (u64, u64, u64) {
    if !strict {
        return (max_items, max_store_bytes, max_age_days);
    }
    let age = if max_age_days == 0 {
        0
    } else {
        max_age_days / 2
    };
    (max_items / 2, max_store_bytes / 2, age)
}

/// Pinned clips and permanents are exempt from retention purges.
#[must_use]
pub fn retention_exempt(pinned: bool) -> bool {
    pinned
}
