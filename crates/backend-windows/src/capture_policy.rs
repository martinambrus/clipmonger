//! Capture-only owner and sequence checks. No policy applies to clipboard restore.
use std::sync::Arc;

/// Live denylist, or None when configuration could not be read.
pub type CapturePolicy = Arc<dyn Fn() -> Option<Vec<String>> + Send + Sync>;

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Allow,
    OwnerDenied,
    Stale,
}

#[cfg(any(windows, test))]
pub(crate) fn admission(
    expected: Option<u32>,
    actual: Option<u32>,
    owner: Option<&str>,
    entries: Option<&[String]>,
) -> Admission {
    if expected.is_none() || actual != expected {
        return Admission::Stale;
    }
    let Some(entries) = entries else {
        return Admission::OwnerDenied;
    };
    let Some(owner) = owner else {
        return Admission::Allow;
    };
    let owner = owner.to_lowercase();
    if entries.iter().any(|entry| {
        let entry = entry.trim().to_lowercase();
        !entry.is_empty() && owner.contains(&entry)
    }) {
        Admission::OwnerDenied
    } else {
        Admission::Allow
    }
}

/// Gate the actual byte-producing operation after the caller opens the clipboard.
#[cfg(any(windows, test))]
pub(crate) fn guarded<T>(
    expected: Option<u32>,
    actual: Option<u32>,
    owner: Option<&str>,
    entries: Option<&[String]>,
    read: impl FnOnce() -> T,
) -> Result<T, Admission> {
    match admission(expected, actual, owner, entries) {
        Admission::Allow => Ok(read()),
        denied => Err(denied),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_or_changed_clipboard_never_calls_reader() {
        let reads = std::cell::Cell::new(0);
        let entries = vec!["secret.exe".to_string()];
        for (seq, owner) in [(7, "SECRET.EXE"), (8, "safe.exe")] {
            let result = guarded(Some(7), Some(seq), Some(owner), Some(&entries), || {
                reads.set(reads.get() + 1);
            });
            assert!(result.is_err());
        }
        assert_eq!(reads.get(), 0);
        assert!(
            guarded(Some(7), Some(7), Some("safe.exe"), Some(&entries), || reads
                .set(reads.get() + 1))
            .is_ok()
        );
        assert_eq!(reads.get(), 1);
    }
    #[test]
    fn capture_guard_rejects_blocked_owner_and_stale_sequence() {
        let list = vec!["secret.exe".to_string()];
        assert_eq!(
            admission(Some(7), Some(7), Some(r"C:\\Apps\\SECRET.EXE"), Some(&list)),
            Admission::OwnerDenied
        );
        assert_eq!(
            admission(Some(7), Some(8), Some("other.exe"), Some(&list)),
            Admission::Stale
        );
        assert_eq!(
            admission(Some(7), Some(7), None, Some(&list)),
            Admission::Allow
        );
        assert_eq!(
            admission(Some(7), Some(7), None, None),
            Admission::OwnerDenied
        );
    }
}
