//! INCR transfer state machines (pure, no X connection; seg-05 §2).
//!
//! The receiver accumulates `PropertyNotify` chunks until the zero-length
//! terminator; the sender splits payloads at the server request limit.
//! Clocks are injected (`now_ms`) so tests never sleep.

/// One step of the INCR receive state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvStep {
    /// More chunks outstanding.
    NeedMore,
    /// Zero-length terminator arrived; carries the reassembled bytes.
    Done(Vec<u8>),
    /// Transfer failed; carries a human-readable reason.
    Failed(String),
}

/// INCR receiver: accumulate chunks until the terminator or the cap.
#[derive(Debug)]
pub struct IncrReceiver {
    buf: Vec<u8>,
    max_bytes: u64,
    deadline_ms: u64,
}

impl IncrReceiver {
    /// Create a receiver accepting at most `max_bytes` before `deadline_ms`.
    #[must_use]
    pub fn new(max_bytes: u64, deadline_ms: u64) -> Self {
        Self {
            buf: Vec::new(),
            max_bytes,
            deadline_ms,
        }
    }

    /// Feed one property chunk observed at `now_ms`. A zero-length chunk
    /// ends the transfer; chunks past the deadline or over the cap fail.
    /// A past-deadline call never returns partial `Done`.
    pub fn feed(&mut self, chunk: &[u8], now_ms: u64) -> RecvStep {
        if now_ms >= self.deadline_ms {
            return RecvStep::Failed("Timeout: INCR transfer deadline reached".to_string());
        }
        if chunk.is_empty() {
            return RecvStep::Done(std::mem::take(&mut self.buf));
        }
        let used = u64::try_from(self.buf.len()).unwrap_or(u64::MAX);
        let incoming = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if used.saturating_add(incoming) > self.max_bytes {
            return RecvStep::Failed(format!(
                "TooLarge: INCR payload exceeds {max} bytes",
                max = self.max_bytes
            ));
        }
        self.buf.extend_from_slice(chunk);
        RecvStep::NeedMore
    }
}

/// INCR sender: split a payload at the server request limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncrSender;

impl IncrSender {
    /// Plan chunk sizes for `bytes_len` payload bytes against a
    /// `max_request` server limit. Payloads at or under the limit go in a
    /// single property (no INCR); larger ones split into `max_request`
    /// chunks with a smaller tail.
    #[must_use]
    pub fn plan(bytes_len: usize, max_request: usize) -> Vec<usize> {
        let limit = max_request.max(1);
        if bytes_len <= limit {
            return vec![bytes_len];
        }
        let mut out = Vec::new();
        let mut rest = bytes_len;
        while rest > 0 {
            let take = rest.min(limit);
            out.push(take);
            rest -= take;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_property_needs_no_incr() {
        assert_eq!(IncrSender::plan(10, 100), vec![10]);
        assert_eq!(IncrSender::plan(100, 100), vec![100]);
    }

    #[test]
    fn over_limit_splits_at_max_request() {
        assert_eq!(IncrSender::plan(101, 100), vec![100, 1]);
        assert_eq!(IncrSender::plan(250, 100), vec![100, 100, 50]);
    }

    #[test]
    fn chunk_sizes_sum_to_payload() {
        let plan = IncrSender::plan(5 * 1024 * 1024 + 7, 4096);
        assert!(plan.iter().all(|&c| c <= 4096));
        assert_eq!(plan.iter().sum::<usize>(), 5 * 1024 * 1024 + 7);
    }

    #[test]
    fn receive_reassembles_chunks() {
        let mut rx = IncrReceiver::new(1024, 5000);
        assert_eq!(rx.feed(b"hello ", 0), RecvStep::NeedMore);
        assert_eq!(rx.feed(b"world", 1), RecvStep::NeedMore);
        assert_eq!(rx.feed(b"", 2), RecvStep::Done(b"hello world".to_vec()));
    }

    #[test]
    fn empty_stream_terminates_immediately() {
        let mut rx = IncrReceiver::new(1024, 5000);
        assert_eq!(rx.feed(b"", 0), RecvStep::Done(Vec::new()));
    }

    #[test]
    fn over_cap_fails_too_large() {
        let mut rx = IncrReceiver::new(8, 5000);
        assert_eq!(rx.feed(b"12345678", 0), RecvStep::NeedMore);
        match rx.feed(b"9", 1) {
            RecvStep::Failed(reason) => assert!(reason.contains("TooLarge"), "{reason}"),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn truncated_stream_times_out_without_partial_done() {
        let mut rx = IncrReceiver::new(1024, 100);
        assert_eq!(rx.feed(b"partial", 0), RecvStep::NeedMore);
        match rx.feed(b"more", 100) {
            RecvStep::Failed(reason) => assert!(reason.contains("Timeout"), "{reason}"),
            other => panic!("expected Timeout, got {other:?}"),
        }
        // Even the terminator past the deadline must not yield partial data.
        match rx.feed(b"", 101) {
            RecvStep::Failed(reason) => assert!(reason.contains("Timeout"), "{reason}"),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }
}
