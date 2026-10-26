//! Paste policy: self-write suppression, guarded restore, target revalidation.

use crate::types::{Error, PasteTarget, Result};

/// Self-write expectation TTL: 2 s (D-F15).
pub const SUPPRESSION_TTL_MS: u64 = 2000;

/// One outstanding daemon-initiated write expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Expectation {
    /// Canonical content hash registered before the write.
    hash: String,
    /// Backend pre-state (sequence number / changeCount / ...).
    pre_state: Option<String>,
    /// Expiry time in ms (injected clock).
    expires_ms: u64,
}

/// Register-before-write suppression registry.
///
/// The daemon registers an expectation FIRST (content hash + backend
/// pre-state), then writes. The monitor consults the registry per event:
/// exact match suppresses once and consumes; anything else (including a
/// genuine user copy racing the write) is captured normally. One registry
/// is shared by dual X11+Wayland monitors. All times are caller-supplied
/// `now_ms`; no wall-clock reads inside.
#[derive(Debug, Default)]
pub struct SuppressionRegistry {
    /// Outstanding expectations.
    expectations: Vec<Expectation>,
    /// Suppressed-event counter for diagnostics.
    suppressed: u64,
}

impl SuppressionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an expectation before writing; call first, then write.
    pub fn register(&mut self, content_hash: String, pre_state: Option<String>, now_ms: u64) {
        self.prune(now_ms);
        self.expectations.push(Expectation {
            hash: content_hash,
            pre_state,
            expires_ms: now_ms.saturating_add(SUPPRESSION_TTL_MS),
        });
    }

    /// Drop an expectation after a failed write (failed writes must not
    /// suppress later user copies).
    pub fn cancel(&mut self, content_hash: &str, pre_state: Option<&str>) {
        self.expectations
            .retain(|e| !(e.hash == content_hash && e.pre_state.as_deref() == pre_state));
    }

    /// Consult on each monitor event: exact match suppresses once and
    /// consumes; anything else is captured normally. Expired entries never
    /// match.
    pub fn should_suppress(
        &mut self,
        content_hash: &str,
        pre_state: Option<&str>,
        now_ms: u64,
    ) -> bool {
        self.prune(now_ms);
        if let Some(pos) = self.expectations.iter().position(|e| {
            e.hash == content_hash && (e.pre_state.is_none() || e.pre_state.as_deref() == pre_state)
        }) {
            self.expectations.remove(pos);
            self.suppressed += 1;
            return true;
        }
        false
    }

    /// Suppressed-event count for diagnostics (events are counted, not stored).
    #[must_use]
    pub fn suppressed_count(&self) -> u64 {
        self.suppressed
    }

    /// Outstanding unexpired expectations (diagnostics).
    #[must_use]
    pub fn pending(&self, now_ms: u64) -> usize {
        self.expectations
            .iter()
            .filter(|e| e.expires_ms > now_ms)
            .count()
    }

    /// Drop expired expectations.
    fn prune(&mut self, now_ms: u64) {
        self.expectations.retain(|e| e.expires_ms > now_ms);
    }
}

/// Guarded-restore predicate: restore the saved clipboard ONLY when it
/// still equals our temporary value (unchanged-indicator AND content-hash
/// match). User copies during the dwell always win.
#[must_use]
pub fn guarded_restore_decision(indicator_same: bool, hash_same: bool) -> bool {
    indicator_same && hash_same
}

/// Paste-target validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    /// Target moved: different app, handle, or title.
    #[error("stale target: {0}")]
    Stale(String),
    /// Target no longer exists.
    #[error("dead target")]
    Dead,
}

/// Revalidate a captured target against the current one.
///
/// # Errors
///
/// Returns [`TargetError::Dead`] when there is no current target and
/// [`TargetError::Stale`] when app, handle, or title changed.
pub fn validate_target(
    captured: &PasteTarget,
    current: Option<&PasteTarget>,
) -> std::result::Result<(), TargetError> {
    let Some(cur) = current else {
        return Err(TargetError::Dead);
    };
    if cur.app.exe != captured.app.exe {
        return Err(TargetError::Stale(format!(
            "app changed: {} -> {}",
            captured.app.exe, cur.app.exe
        )));
    }
    if cur.handle != captured.handle {
        return Err(TargetError::Stale("handle changed".to_string()));
    }
    if cur.window_title_hash != captured.window_title_hash {
        return Err(TargetError::Stale("window title changed".to_string()));
    }
    Ok(())
}

/// Map target validation into the platform error space.
impl From<TargetError> for Error {
    fn from(e: TargetError) -> Self {
        match e {
            TargetError::Stale(msg) => Error::Denied(msg),
            TargetError::Dead => Error::Unavailable("paste target dead".to_string()),
        }
    }
}

/// Revalidate and convert into [`Error`] in one step.
///
/// # Errors
///
/// Returns [`Error::Unavailable`] for dead targets and [`Error::Denied`]
/// for stale ones.
pub fn check_target(captured: &PasteTarget, current: Option<&PasteTarget>) -> Result<()> {
    validate_target(captured, current).map_err(Error::from)
}
