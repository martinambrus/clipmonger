//! Privacy opt-out filter (pure).

/// Skip capture when the source offers this format.
pub const EXCLUDE_FORMAT: &str = "ExcludeClipboardContentFromMonitorProcessing";
/// Skip capture only when this format carries value "0".
pub const HISTORY_OPT_OUT: &str = "CanIncludeInClipboardHistory";
/// Skip capture only when this format carries value "0".
pub const CLOUD_OPT_OUT: &str = "CanUploadToCloudClipboard";
/// Skip capture when the source offers this format.
pub const VIEWER_IGNORE: &str = "Clipboard Viewer Ignore";

/// Report whether capture must be skipped for `offers`.
#[must_use]
pub fn should_skip(offers: &[(String, Option<String>)]) -> bool {
    skip_reason(offers).is_some()
}

/// Name the matched opt-out format, if any.
#[must_use]
pub fn skip_reason(offers: &[(String, Option<String>)]) -> Option<&'static str> {
    for (name, value) in offers {
        if name == EXCLUDE_FORMAT {
            return Some(EXCLUDE_FORMAT);
        }
        if name == HISTORY_OPT_OUT && value.as_deref() == Some("0") {
            return Some(HISTORY_OPT_OUT);
        }
        if name == CLOUD_OPT_OUT && value.as_deref() == Some("0") {
            return Some(CLOUD_OPT_OUT);
        }
        if name == VIEWER_IGNORE {
            return Some(VIEWER_IGNORE);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(name: &str, value: Option<&str>) -> (String, Option<String>) {
        (name.to_string(), value.map(str::to_string))
    }

    #[test]
    fn empty_offers_no_skip() {
        assert!(!should_skip(&[]));
        assert_eq!(skip_reason(&[]), None);
    }

    #[test]
    fn normal_formats_no_skip() {
        let offers = [offer("CF_UNICODETEXT", None), offer("HTML Format", None)];
        assert!(!should_skip(&offers));
        assert_eq!(skip_reason(&offers), None);
    }

    #[test]
    fn exclude_present_skips() {
        let offers = [offer(EXCLUDE_FORMAT, None), offer("CF_UNICODETEXT", None)];
        assert!(should_skip(&offers));
        assert_eq!(skip_reason(&offers), Some(EXCLUDE_FORMAT));
    }

    #[test]
    fn history_zero_skips() {
        let offers = [offer(HISTORY_OPT_OUT, Some("0"))];
        assert!(should_skip(&offers));
        assert_eq!(skip_reason(&offers), Some(HISTORY_OPT_OUT));
    }

    #[test]
    fn history_one_does_not_skip() {
        let offers = [offer(HISTORY_OPT_OUT, Some("1"))];
        assert!(!should_skip(&offers));
    }

    #[test]
    fn history_absent_value_does_not_skip() {
        let offers = [offer(HISTORY_OPT_OUT, None)];
        assert!(!should_skip(&offers));
    }

    #[test]
    fn cloud_zero_skips() {
        let offers = [offer(CLOUD_OPT_OUT, Some("0"))];
        assert!(should_skip(&offers));
        assert_eq!(skip_reason(&offers), Some(CLOUD_OPT_OUT));
    }

    #[test]
    fn cloud_one_does_not_skip() {
        let offers = [offer(CLOUD_OPT_OUT, Some("1"))];
        assert!(!should_skip(&offers));
    }

    #[test]
    fn viewer_ignore_present_skips() {
        let offers = [offer(VIEWER_IGNORE, Some("1"))];
        assert!(should_skip(&offers));
        assert_eq!(skip_reason(&offers), Some(VIEWER_IGNORE));
    }
}
