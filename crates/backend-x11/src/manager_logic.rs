//! Clipboard-manager role + `SAVE_TARGETS` session logic (pure; seg-05 §1).
//!
//! History capture and manager ownership are separable: capture runs in
//! both roles, while only the `Serving` role answers `SAVE_TARGETS`.
//! Coexistence (X-F4): when another `CLIPBOARD_MANAGER` owner exists we
//! stay capture-only and never fight for the selection.

use std::collections::HashSet;

/// Manager role of this backend instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerState {
    /// Record history only; never claim `CLIPBOARD_MANAGER`.
    CaptureOnly,
    /// Own `CLIPBOARD_MANAGER` and serve `SAVE_TARGETS`.
    Serving,
}

/// Role at startup given whether a manager owner already exists.
///
/// An existing owner means capture-only (X-F4 coexistence); otherwise we
/// claim the manager selection and serve.
#[must_use]
pub fn on_start(existing_owner: bool) -> ManagerState {
    if existing_owner {
        ManagerState::CaptureOnly
    } else {
        ManagerState::Serving
    }
}

/// Role after losing a selection.
///
/// Losing the manager selection falls back to capture-only; losing any
/// other selection leaves a serving manager in place.
#[must_use]
pub fn on_selection_clear(is_manager: bool) -> ManagerState {
    if is_manager {
        ManagerState::CaptureOnly
    } else {
        ManagerState::Serving
    }
}

/// Role after the manager selection is vacated: claim it and serve.
#[must_use]
pub fn on_manager_vacated() -> ManagerState {
    ManagerState::Serving
}

/// Reply decision for a `SAVE_TARGETS` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// All transfers finished: send `SelectionNotify` so the dying owner
    /// may exit (X-F5: never notify early, the owner may abort slow INCR).
    Notify,
    /// Refuse: the caller keeps waiting before the deadline, or discards
    /// the partial data at/after it (X-F5).
    Refuse,
}

/// Tracks one dying-owner `SAVE_TARGETS` handoff until every offered
/// target finished converting (including INCR to the zero-length chunk).
#[derive(Debug)]
pub struct SaveSession {
    pending: HashSet<String>,
}

impl SaveSession {
    /// Start a session waiting for `targets` with a caller-held
    /// `deadline_ms` (5 s per transfer, seg-05 §2; the caller enforces it
    /// via [`SaveSession::finish`]).
    #[must_use]
    pub fn new(targets: &[String], deadline_ms: u64) -> Self {
        let _ = deadline_ms;
        Self {
            pending: targets.iter().cloned().collect(),
        }
    }

    /// Record one target's transfer result (`ok` = bytes stored).
    pub fn add_target_result(&mut self, mime: &str, ok: bool) {
        let _ = ok;
        self.pending.remove(mime);
    }

    /// Report whether every offered target resolved (ok or refused).
    #[must_use]
    pub fn complete(&self) -> bool {
        self.pending.is_empty()
    }

    /// Decide the `SAVE_TARGETS` reply (`now_ms` against `deadline_ms`).
    ///
    /// Notifies only when [`SaveSession::complete`] holds: the owner exits
    /// on receipt and an early notify can abort slow INCR transfers (X-F5).
    /// An unfinished session always refuses; `now_ms`/`deadline_ms` tell
    /// the caller what to do next — keep waiting before the deadline, or
    /// discard the partial data at/after it.
    #[must_use]
    pub fn finish(&self, _now_ms: u64, _deadline_ms: u64) -> SaveOutcome {
        if self.complete() {
            SaveOutcome::Notify
        } else {
            SaveOutcome::Refuse
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn existing_owner_means_capture_only() {
        assert_eq!(on_start(true), ManagerState::CaptureOnly);
        assert_eq!(on_start(false), ManagerState::Serving);
    }

    #[test]
    fn losing_manager_selection_falls_back() {
        assert_eq!(on_selection_clear(true), ManagerState::CaptureOnly);
        // Losing any other selection keeps a serving manager serving.
        assert_eq!(on_selection_clear(false), ManagerState::Serving);
    }

    #[test]
    fn vacated_manager_is_claimed() {
        assert_eq!(on_manager_vacated(), ManagerState::Serving);
    }

    #[test]
    fn save_notifies_only_when_all_complete() {
        let mut session = SaveSession::new(&targets(&["text", "image/png"]), 5000);
        assert!(!session.complete());
        assert_eq!(session.finish(0, 5000), SaveOutcome::Refuse);
        session.add_target_result("text", true);
        assert!(!session.complete());
        assert_eq!(session.finish(4999, 5000), SaveOutcome::Refuse);
        session.add_target_result("image/png", true);
        assert!(session.complete());
        assert_eq!(session.finish(4999, 5000), SaveOutcome::Notify);
    }

    #[test]
    fn save_refuses_at_deadline_with_partial_data() {
        let mut session = SaveSession::new(&targets(&["text", "image/png"]), 5000);
        session.add_target_result("text", true);
        assert_eq!(session.finish(5000, 5000), SaveOutcome::Refuse);
        assert_eq!(session.finish(9999, 5000), SaveOutcome::Refuse);
    }

    #[test]
    fn failed_targets_still_complete() {
        let mut session = SaveSession::new(&targets(&["text"]), 5000);
        session.add_target_result("text", false);
        assert!(session.complete());
        assert_eq!(session.finish(0, 5000), SaveOutcome::Notify);
    }
}
