//! X11+Wayland dual-backend merge dedupe (pure; seg-06 S8, T-603, W-F5).
//!
//! When `DISPLAY` and `WAYLAND_DISPLAY` both work, both monitors run and
//! each primarily sees its own clients — but compositors MAY bridge
//! X↔Wayland selections, so both monitors can observe bridged copies.
//! Dedupe key is core `content_hash` (seg-03 S8); window and tie-break per
//! W-F5: 2000 ms, first delivery wins, Wayland wins ties on equal/subset
//! payloads (the X11 side lost formats to the `XWayland` bridge).

use std::collections::HashMap;

use clipmonger_core::hash::content_hash;
use clipmonger_platform::types::PayloadMap;

/// Canonical dedupe key for one observation's payloads.
pub struct MergeKey;

impl MergeKey {
    /// Core canonical hash (sorted MIME keys, native `x-os/*` excluded).
    #[must_use]
    pub fn of(payloads: &PayloadMap) -> String {
        content_hash(payloads)
    }
}

/// What the daemon should do with a merge observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDecision {
    /// New row (or move-to-top outside the window).
    Emit,
    /// Bridged duplicate of a row already emitted: drop.
    SuppressDuplicate,
    /// Same content arrived via Wayland after X11 with equal-or-subset X11
    /// formats (bridge loss): re-emit attributed to Wayland.
    EmitWaylandWins,
}

/// Windowed first-wins merger with the Wayland tie-break.
#[derive(Debug, Default)]
pub struct Merger {
    window_ms: u64,
    last: HashMap<String, (String, Vec<String>, u64)>,
}

impl Merger {
    /// Create a merger with the given dedupe window (W-F5: 2000 ms).
    #[must_use]
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms,
            last: HashMap::new(),
        }
    }

    /// Offer one observation: `origin` is `"x11"`/`"wayland"`, `key` is
    /// [`MergeKey::of`] the payloads, `payload_mimes` the offered MIME list,
    /// `at_ms` the observation clock.
    ///
    /// # Panics
    ///
    /// Never panics: the prior-row lookup only runs when the row exists
    /// (the `fresh` branch returns early otherwise).
    pub fn offer(
        &mut self,
        origin: &str,
        key: &str,
        payload_mimes: &[String],
        at_ms: u64,
    ) -> MergeDecision {
        let fresh = match self.last.get(key) {
            None => true,
            Some((_, _, prev_at)) => at_ms.saturating_sub(*prev_at) > self.window_ms,
        };
        if fresh {
            self.last.insert(
                key.to_string(),
                (origin.to_string(), payload_mimes.to_vec(), at_ms),
            );
            return MergeDecision::Emit;
        }
        let (prev_origin, prev_mimes, _) = self.last.get(key).expect("checked above");
        if origin == "wayland"
            && prev_origin == "x11"
            && prev_mimes.iter().all(|mime| payload_mimes.contains(mime))
        {
            self.last.insert(
                key.to_string(),
                (origin.to_string(), payload_mimes.to_vec(), at_ms),
            );
            return MergeDecision::EmitWaylandWins;
        }
        MergeDecision::SuppressDuplicate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn t603_key_is_core_content_hash() {
        let payloads: PayloadMap = vec![("text/plain".to_string(), b"hello".to_vec())];
        assert_eq!(MergeKey::of(&payloads), content_hash(&payloads));
        // MIME order does not change the key.
        let reordered: PayloadMap = vec![
            ("text/html".to_string(), b"<b>hi</b>".to_vec()),
            ("text/plain".to_string(), b"hi".to_vec()),
        ];
        let swapped: PayloadMap = vec![
            ("text/plain".to_string(), b"hi".to_vec()),
            ("text/html".to_string(), b"<b>hi</b>".to_vec()),
        ];
        assert_eq!(MergeKey::of(&reordered), MergeKey::of(&swapped));
    }

    #[test]
    fn t603_same_payloads_within_window_emit_once_first_wins() {
        let mut merger = Merger::new(2000);
        assert_eq!(
            merger.offer("x11", "k", &mimes(&["text/plain"]), 0),
            MergeDecision::Emit
        );
        assert_eq!(
            merger.offer("wayland", "k", &mimes(&["text/plain", "text/html"]), 500),
            MergeDecision::EmitWaylandWins
        );
        // Third sighting is a plain duplicate.
        assert_eq!(
            merger.offer("x11", "k", &mimes(&["text/plain"]), 900),
            MergeDecision::SuppressDuplicate
        );
    }

    #[test]
    fn t603_wayland_subset_does_not_steal_x11_row() {
        let mut merger = Merger::new(2000);
        assert_eq!(
            merger.offer("x11", "k", &mimes(&["text/plain", "text/html"]), 0),
            MergeDecision::Emit
        );
        // Wayland offers FEWER formats: not the bridge-loss direction.
        assert_eq!(
            merger.offer("wayland", "k", &mimes(&["text/plain"]), 100),
            MergeDecision::SuppressDuplicate
        );
    }

    #[test]
    fn t603_outside_window_moves_to_top() {
        let mut merger = Merger::new(2000);
        assert_eq!(
            merger.offer("x11", "k", &mimes(&["text/plain"]), 0),
            MergeDecision::Emit
        );
        assert_eq!(
            merger.offer("x11", "k", &mimes(&["text/plain"]), 2001),
            MergeDecision::Emit
        );
    }
}
