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

/// Canonical dedupe key for one observation's payloads.
pub struct MergeKey;

impl MergeKey {
    /// Core canonical hash (sorted MIME keys, native `x-os/*` excluded).
    /// MIME labels are base-normalized first (`text/plain;charset=utf-8`
    /// and `text/plain` hash alike): the extension and the daemon watchers
    /// label identical bytes differently, and without this every bridged
    /// copy is stored twice. Stored `content_hash` values are untouched;
    /// only the merge-time key is normalized.
    #[must_use]
    pub fn of(payloads: &[(String, Vec<u8>)]) -> String {
        // The X11 watcher offers `text/plain` and `text/plain;charset=utf-8`;
        // after normalizing identical bytes are one entry. Its bare
        // `text/plain` is a legacy re-encoding (Latin-1 for non-ASCII text),
        // so where a UTF-8 variant exists only that one keys the text.
        let has_utf8 = payloads.iter().any(|(mime, _)| is_utf8_text(mime));
        let normalized: Vec<(String, Vec<u8>)> = payloads
            .iter()
            .filter(|(mime, _)| !(has_utf8 && is_bare_text(mime)))
            .map(|(mime, bytes)| (base_mime(mime), bytes.clone()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        content_hash(&normalized)
    }
}

/// `text/plain` with an explicit UTF-8 charset.
fn is_utf8_text(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase().replace(' ', "");
    base_mime(&lower) == "text/plain" && lower.contains("charset=utf-8")
}

/// `text/plain` without any parameters (the legacy X11 target).
fn is_bare_text(mime: &str) -> bool {
    !mime.contains(';') && base_mime(mime) == "text/plain"
}

/// Lowercased MIME type without parameters (`Text/Plain; charset=x` ->
/// `text/plain`).
fn base_mime(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase()
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
        let payloads: Vec<(String, Vec<u8>)> = vec![("text/plain".to_string(), b"hello".to_vec())];
        assert_eq!(MergeKey::of(&payloads), content_hash(&payloads));
        // MIME order does not change the key.
        let reordered: Vec<(String, Vec<u8>)> = vec![
            ("text/html".to_string(), b"<b>hi</b>".to_vec()),
            ("text/plain".to_string(), b"hi".to_vec()),
        ];
        let swapped: Vec<(String, Vec<u8>)> = vec![
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
    fn t603_charset_suffixed_mime_merges_with_bare_mime() {
        // Live gap (Step 6 Ubuntu): extension reports `text/plain`, the
        // daemon x11 watcher reports `text/plain;charset=utf-8` for the same
        // bytes. Same content must share one merge key or every copy lands
        // twice (gnome + x11 rows).
        let gnome: Vec<(String, Vec<u8>)> = vec![("text/plain".to_string(), b"dup".to_vec())];
        let x11: Vec<(String, Vec<u8>)> =
            vec![("text/plain;charset=utf-8".to_string(), b"dup".to_vec())];
        assert_eq!(MergeKey::of(&gnome), MergeKey::of(&x11));
        let mut merger = Merger::new(2000);
        assert_eq!(
            merger.offer("gnome", &MergeKey::of(&gnome), &mimes(&["text/plain"]), 0),
            MergeDecision::Emit
        );
        assert_eq!(
            merger.offer(
                "x11",
                &MergeKey::of(&x11),
                &mimes(&["text/plain;charset=utf-8"]),
                400
            ),
            MergeDecision::SuppressDuplicate
        );
    }

    #[test]
    fn t603_x11_charset_twins_merge_with_the_extension_single_text() {
        // Live gap (GNOME 46 Xorg): the X11 watcher stores text/plain plus
        // text/plain;charset=utf-8 with identical bytes, the extension
        // text/plain alone, 139 ms apart: one clip, not two rows.
        let x11: Vec<(String, Vec<u8>)> = vec![
            ("text/plain".to_string(), b"t703 test".to_vec()),
            (
                "text/plain;charset=utf-8".to_string(),
                b"t703 test".to_vec(),
            ),
        ];
        let gnome: Vec<(String, Vec<u8>)> = vec![("text/plain".to_string(), b"t703 test".to_vec())];
        assert_eq!(MergeKey::of(&x11), MergeKey::of(&gnome));
        let different: Vec<(String, Vec<u8>)> = vec![
            ("text/plain".to_string(), b"t703 test".to_vec()),
            ("text/plain;charset=utf-8".to_string(), b"other".to_vec()),
        ];
        assert_ne!(
            MergeKey::of(&different),
            MergeKey::of(&gnome),
            "different bytes stay apart"
        );
    }

    #[test]
    fn t603_x11_latin1_text_plain_does_not_split_a_utf8_copy() {
        // Live gap (GNOME 46 Xorg): for Hungarian text the X11 watcher's bare
        // text/plain is Latin-1, its charset target and the extension UTF-8.
        let utf8 = "Iktatószám".as_bytes().to_vec();
        let latin1: Vec<u8> = "Iktatószám".chars().map(|c| c as u8).collect();
        let x11: Vec<(String, Vec<u8>)> = vec![
            ("text/plain;charset=utf-8".to_string(), utf8.clone()),
            ("text/plain".to_string(), latin1.clone()),
        ];
        let gnome: Vec<(String, Vec<u8>)> = vec![("text/plain".to_string(), utf8)];
        assert_eq!(MergeKey::of(&x11), MergeKey::of(&gnome));
        let bare_only: Vec<(String, Vec<u8>)> = vec![("text/plain".to_string(), latin1)];
        assert_ne!(
            MergeKey::of(&bare_only),
            MergeKey::of(&gnome),
            "without a UTF-8 variant the bare text still keys"
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
