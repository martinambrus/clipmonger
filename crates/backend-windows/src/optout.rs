//! Privacy opt-out filter (pure).

/// Skip capture when the source offers this format.
pub const EXCLUDE_FORMAT: &str = "ExcludeClipboardContentFromMonitorProcessing";
/// Skip capture only when this format carries value "0".
pub const HISTORY_OPT_OUT: &str = "CanIncludeInClipboardHistory";
/// Skip capture only when this format carries value "0".
pub const CLOUD_OPT_OUT: &str = "CanUploadToCloudClipboard";
/// Skip capture when the source offers this format.
pub const VIEWER_IGNORE: &str = "Clipboard Viewer Ignore";

/// Decode a privacy marker of at most 64 bytes. A little-endian DWORD
/// zero or one with optional zero padding is returned in decimal; all
/// other payloads use the legacy text decoding and trimming.
#[must_use]
pub fn decode_marker(bytes: &[u8]) -> Option<String> {
    if bytes.len() > 64 {
        return None;
    }
    if bytes.len() >= 4 && bytes[4..].iter().all(|&byte| byte == 0) {
        let dword = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if matches!(dword, 0 | 1) {
            return Some(dword.to_string());
        }
    }
    let text = String::from_utf8_lossy(bytes).into_owned();
    Some(text.trim_matches('\0').trim().to_string())
}

/// Report whether capture must be skipped for `offers`.
#[must_use]
pub fn should_skip(offers: &[(String, Option<String>)]) -> bool {
    skip_reason(offers).is_some()
}

/// Report whether `name` is a privacy opt-out format (never advertised
/// in transfer format lists and never stored).
#[must_use]
pub fn is_optout_name(name: &str) -> bool {
    matches!(
        name,
        EXCLUDE_FORMAT | HISTORY_OPT_OUT | CLOUD_OPT_OUT | VIEWER_IGNORE
    )
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

    fn marker_skips(name: &str, bytes: &[u8]) -> bool {
        should_skip(&[(name.to_string(), decode_marker(bytes))])
    }

    #[test]
    fn history_dword_zero_skips() {
        assert!(marker_skips(HISTORY_OPT_OUT, &0_u32.to_le_bytes()));
    }

    #[test]
    fn cloud_dword_zero_skips() {
        assert!(marker_skips(CLOUD_OPT_OUT, &0_u32.to_le_bytes()));
    }

    #[test]
    fn marker_one_text_and_empty_keep_existing_policy() {
        for name in [HISTORY_OPT_OUT, CLOUD_OPT_OUT] {
            assert!(!marker_skips(name, &1_u32.to_le_bytes()));
            assert!(marker_skips(name, b"0"));
            assert!(marker_skips(name, b" 0\0"));
            assert!(!marker_skips(name, b""));
        }
        assert_eq!(decode_marker(&[b'x'; 65]), None);
        assert_eq!(decode_marker(b"hello\0"), Some("hello".to_string()));
    }

    #[test]
    fn marker_one_decodes_and_other_dwords_do_not_skip() {
        assert_eq!(decode_marker(&1_u32.to_le_bytes()).as_deref(), Some("1"));
        for name in [HISTORY_OPT_OUT, CLOUD_OPT_OUT] {
            for bytes in [
                [0, 1, 0, 0],
                [0, 0, 0, 1],
                [2, 0, 0, 0],
                u32::MAX.to_le_bytes(),
            ] {
                assert!(!marker_skips(name, &bytes));
            }
        }
    }

    #[test]
    fn marker_four_byte_text_zero_skips() {
        for name in [HISTORY_OPT_OUT, CLOUD_OPT_OUT] {
            assert!(marker_skips(name, b"0\0\0\0"));
        }
    }

    #[test]
    fn marker_padded_dword_zero_skips() {
        for name in [HISTORY_OPT_OUT, CLOUD_OPT_OUT] {
            assert!(marker_skips(name, &[0; 8]));
            assert!(marker_skips(name, &[0; 16]));
        }
    }

    #[test]
    fn marker_padded_dword_one_decodes_and_does_not_skip() {
        let bytes = [1, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_marker(&bytes).as_deref(), Some("1"));
        for name in [HISTORY_OPT_OUT, CLOUD_OPT_OUT] {
            assert!(!marker_skips(name, &bytes));
        }
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
