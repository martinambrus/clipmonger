//! Paste policy: self-write suppression, guarded restore, target revalidation.

use crate::types::{Error, PasteTarget, Result};

/// Self-write expectation TTL: 2 s (D-F15).
pub const SUPPRESSION_TTL_MS: u64 = 2000;

/// One outstanding daemon-initiated write expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Expectation {
    id: u64,
    ready: bool,
    bridge: bool,
    primary_origin: Option<String>,
    alternate_hash: Option<String>,
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
    next_id: u64,
    companions: Vec<(u64, String, String, u64)>,
}

impl SuppressionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an expectation before writing; call first, then write.
    pub fn register(&mut self, content_hash: String, pre_state: Option<String>, now_ms: u64) {
        self.register_tracked(content_hash, pre_state, now_ms);
    }

    /// Register an individually cancellable write expectation.
    ///
    /// # Panics
    /// Panics if the process exhausts all 64-bit expectation identifiers.
    pub fn register_tracked(
        &mut self,
        content_hash: String,
        pre_state: Option<String>,
        now_ms: u64,
    ) -> u64 {
        self.prune(now_ms);
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("suppression identifier exhausted");
        self.expectations.push(Expectation {
            id,
            ready: true,
            bridge: false,
            primary_origin: None,
            alternate_hash: None,
            hash: content_hash,
            pre_state,
            expires_ms: now_ms.saturating_add(SUPPRESSION_TTL_MS),
        });
        id
    }

    /// Cancel only the expectation belonging to one failed write.
    pub fn cancel_tracked(&mut self, id: u64) {
        self.expectations.retain(|entry| entry.id != id);
        self.companions
            .retain(|(entry_id, _, _, _)| *entry_id != id);
    }

    /// Register before a write whose event sequence is known only at completion.
    pub fn register_pending(&mut self, content_hash: String, now_ms: u64) -> u64 {
        let id = self.register_tracked(content_hash, None, now_ms);
        if let Some(entry) = self.expectations.iter_mut().find(|entry| entry.id == id) {
            entry.ready = false;
        }
        id
    }

    /// Register a write that can be observed by both Linux monitors.
    /// Without event-bound provenance, an identical user copy on the other
    /// backend during the TTL is indistinguishable from a bridged observation.
    pub fn register_bridged(&mut self, content_hash: String, now_ms: u64) -> u64 {
        self.register_bridged_variants(content_hash, None, now_ms)
    }

    /// Register two complete capture representations of the same daemon write.
    pub fn register_bridged_variants(
        &mut self,
        content_hash: String,
        alternate_hash: Option<String>,
        now_ms: u64,
    ) -> u64 {
        let alternate_hash = alternate_hash.or_else(|| Some(content_hash.clone()));
        self.register_bridged_for("x11", content_hash, alternate_hash, now_ms)
    }

    /// Register complete representations bound to their respective backends.
    pub fn register_bridged_for(
        &mut self,
        origin: &str,
        content_hash: String,
        alternate_hash: Option<String>,
        now_ms: u64,
    ) -> u64 {
        let id = self.register_tracked(content_hash, None, now_ms);
        if let Some(entry) = self.expectations.iter_mut().find(|entry| entry.id == id) {
            entry.bridge = true;
            entry.primary_origin = Some(origin.to_string());
            entry.alternate_hash = alternate_hash;
        }
        id
    }

    /// Bind a pending write to the event sequence measured inside the write.
    pub fn finalize_pending(&mut self, id: u64, state: String) {
        if let Some(entry) = self.expectations.iter_mut().find(|entry| entry.id == id) {
            entry.pre_state = Some(state);
            entry.ready = true;
        }
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
        self.should_suppress_from(content_hash, pre_state, now_ms, None)
    }

    /// Consume an expectation or its one exact observation from the other Linux monitor.
    pub fn should_suppress_from(
        &mut self,
        content_hash: &str,
        pre_state: Option<&str>,
        now_ms: u64,
        origin: Option<&str>,
    ) -> bool {
        self.prune(now_ms);
        if let Some(pos) = self.expectations.iter().position(|e| {
            e.ready
                && (if e.bridge {
                    match (origin, e.primary_origin.as_deref()) {
                        (Some(actual @ ("x11" | "wayland")), Some(primary))
                            if actual == primary =>
                        {
                            e.hash == content_hash
                        }
                        (Some("x11" | "wayland"), Some(_)) => {
                            e.alternate_hash.as_deref() == Some(content_hash)
                        }
                        _ => false,
                    }
                } else {
                    e.hash == content_hash
                })
                && (e.pre_state.is_none() || e.pre_state.as_deref() == pre_state)
        }) {
            let entry = self.expectations.remove(pos);
            if entry.bridge {
                if let Some(origin @ ("x11" | "wayland")) = origin {
                    let primary = entry.primary_origin.as_deref();
                    let (other_origin, other_hash) = if Some(origin) == primary {
                        (
                            if origin == "x11" { "wayland" } else { "x11" },
                            entry.alternate_hash,
                        )
                    } else {
                        (primary.unwrap_or(""), Some(entry.hash))
                    };
                    if let Some(hash) = other_hash {
                        self.companions.push((
                            entry.id,
                            hash,
                            other_origin.to_string(),
                            entry.expires_ms,
                        ));
                    }
                }
            }
            self.suppressed += 1;
            return true;
        }
        if let Some(origin @ ("x11" | "wayland")) = origin {
            if let Some(pos) = self
                .companions
                .iter()
                .position(|(_, hash, expected_origin, _)| {
                    hash == content_hash && expected_origin == origin
                })
            {
                self.companions.remove(pos);
                self.suppressed += 1;
                return true;
            }
            // A subsequent unmatched observation may be a new user copy.
            self.companions.clear();
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
        self.companions
            .retain(|(_, _, _, expires)| *expires > now_ms);
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
