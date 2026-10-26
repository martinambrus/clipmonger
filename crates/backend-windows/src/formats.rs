//! Canonical/MIME format map (pure).

/// Map a canonical MIME name to its Windows clipboard format name.
#[must_use]
pub fn canonical_to_windows(canonical: &str) -> Option<&'static str> {
    match canonical {
        "text/plain;charset=utf-8" => Some("CF_UNICODETEXT"),
        "text/html" => Some("HTML Format"),
        "text/rtf" => Some("Rich Text Format"),
        "image/png" => Some("CF_DIB"),
        "text/uri-list" => Some("CF_HDROP"),
        _ => None,
    }
}

/// Map a Windows clipboard format name to its canonical MIME name.
#[must_use]
pub fn windows_to_canonical(name: &str) -> Option<&'static str> {
    match name {
        "CF_UNICODETEXT" => Some("text/plain;charset=utf-8"),
        "HTML Format" => Some("text/html"),
        "Rich Text Format" => Some("text/rtf"),
        "CF_DIB" | "CF_DIBV5" | "PNG" => Some("image/png"),
        "CF_HDROP" => Some("text/uri-list"),
        _ => None,
    }
}

/// Report whether `name` passes through as `x-os/<name>`.
#[must_use]
pub fn is_native_passthrough(name: &str) -> bool {
    windows_to_canonical(name).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_maps_unicode() {
        assert_eq!(
            canonical_to_windows("text/plain;charset=utf-8"),
            Some("CF_UNICODETEXT")
        );
        assert_eq!(
            windows_to_canonical("CF_UNICODETEXT"),
            Some("text/plain;charset=utf-8")
        );
    }

    #[test]
    fn html_maps() {
        assert_eq!(canonical_to_windows("text/html"), Some("HTML Format"));
        assert_eq!(windows_to_canonical("HTML Format"), Some("text/html"));
    }

    #[test]
    fn rtf_maps() {
        assert_eq!(canonical_to_windows("text/rtf"), Some("Rich Text Format"));
        assert_eq!(windows_to_canonical("Rich Text Format"), Some("text/rtf"));
    }

    #[test]
    fn png_maps_dib() {
        assert_eq!(canonical_to_windows("image/png"), Some("CF_DIB"));
        assert_eq!(windows_to_canonical("CF_DIB"), Some("image/png"));
        assert_eq!(windows_to_canonical("CF_DIBV5"), Some("image/png"));
        assert_eq!(windows_to_canonical("PNG"), Some("image/png"));
    }

    #[test]
    fn urilist_maps_hdrop() {
        assert_eq!(canonical_to_windows("text/uri-list"), Some("CF_HDROP"));
        assert_eq!(windows_to_canonical("CF_HDROP"), Some("text/uri-list"));
    }

    #[test]
    fn unknown_is_passthrough() {
        assert_eq!(canonical_to_windows("application/x-foo"), None);
        assert_eq!(windows_to_canonical("FooBarBaz"), None);
        assert!(is_native_passthrough("FooBarBaz"));
        assert!(is_native_passthrough("application/x-foo"));
    }

    #[test]
    fn known_is_not_passthrough() {
        for name in [
            "CF_UNICODETEXT",
            "HTML Format",
            "Rich Text Format",
            "PNG",
            "CF_DIB",
            "CF_DIBV5",
            "CF_HDROP",
        ] {
            assert!(!is_native_passthrough(name), "{name}");
        }
    }
}
