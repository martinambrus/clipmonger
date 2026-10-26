//! Primary-selection debounce (pure; seg-05 §1, seg-03 §8).
//!
//! Primary churn coalesces: rapid offers reset the quiet-period timer so
//! only the latest value is emitted once, after `debounce_ms` of quiet.
//! Clipboard-origin events are never debounced (the caller routes only
//! primary events here). Clocks are injected (`now_ms`).

/// Debouncer for primary-selection capture events.
#[derive(Debug)]
pub struct PrimaryDebouncer {
    debounce_ms: u64,
    min_len: u64,
    pending_at_ms: Option<u64>,
}

impl PrimaryDebouncer {
    /// Create a debouncer emitting at most once per `debounce_ms` of
    /// quiet, dropping values shorter than `min_len`.
    #[must_use]
    pub fn new(debounce_ms: u64, min_len: u64) -> Self {
        Self {
            debounce_ms,
            min_len,
            pending_at_ms: None,
        }
    }

    /// Offer a primary value of `text_len` observed at `now_ms`.
    ///
    /// Returns `true` when the value is emitted immediately (only with a
    /// zero debounce); otherwise the value is stored and [`Self::poll`]
    /// emits it after the quiet period. Values under `min_len` are
    /// dropped (`false`).
    pub fn offer(&mut self, text_len: u64, now_ms: u64) -> bool {
        if text_len < self.min_len {
            return false;
        }
        if self.debounce_ms == 0 {
            self.pending_at_ms = None;
            return true;
        }
        self.pending_at_ms = Some(now_ms);
        false
    }

    /// Emit once after a full quiet period elapsed. Returns `Some(())`
    /// exactly once per coalesced burst.
    pub fn poll(&mut self, now_ms: u64) -> Option<()> {
        let at = self.pending_at_ms?;
        if now_ms.saturating_sub(at) >= self.debounce_ms {
            self.pending_at_ms = None;
            Some(())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_coalesces_to_single_emission() {
        let mut debouncer = PrimaryDebouncer::new(500, 0);
        for at in [0, 100, 200, 300, 400] {
            assert!(!debouncer.offer(10, at));
            assert_eq!(debouncer.poll(at + 100), None);
        }
        // Quiet since 400: emit once at 900, then silence.
        assert_eq!(debouncer.poll(899), None);
        assert_eq!(debouncer.poll(900), Some(()));
        assert_eq!(debouncer.poll(901), None);
    }

    #[test]
    fn short_values_are_dropped() {
        let mut debouncer = PrimaryDebouncer::new(500, 4);
        assert!(!debouncer.offer(3, 0));
        assert_eq!(debouncer.poll(10_000), None);
        assert!(!debouncer.offer(4, 10_000));
        assert_eq!(debouncer.poll(10_500), Some(()));
    }

    #[test]
    fn zero_debounce_emits_every_change() {
        let mut debouncer = PrimaryDebouncer::new(0, 0);
        assert!(debouncer.offer(1, 0));
        assert!(debouncer.offer(1, 1));
        assert_eq!(debouncer.poll(2), None);
    }
}
