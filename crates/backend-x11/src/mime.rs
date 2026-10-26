//! X11 atom/MIME map (pure, no X connection).
//!
//! Canonical mapping is the seg-03 §2 X11 column; anything without a
//! canonical row passes through as `x-os/<atom-name>`.

/// Map a canonical MIME name to its X11 target atom name.
#[must_use]
pub fn canonical_to_x11(canonical: &str) -> Option<&'static str> {
    match canonical {
        "text/plain;charset=utf-8" => Some("UTF8_STRING"),
        "text/plain" => Some("STRING"),
        "text/html" => Some("text/html"),
        "text/rtf" => Some("text/rtf"),
        "image/png" => Some("image/png"),
        "text/uri-list" => Some("text/uri-list"),
        _ => None,
    }
}

/// Map an X11 target atom name to its canonical MIME name.
///
/// Known atoms map to their canonical row; `x-os/<name>` names map to
/// themselves; any other atom becomes the passthrough `x-os/<name>`.
#[must_use]
pub fn x11_to_canonical(name: &str) -> Option<String> {
    match name {
        "UTF8_STRING" => Some("text/plain;charset=utf-8".to_string()),
        "STRING" | "TEXT" => Some("text/plain".to_string()),
        "text/html" => Some("text/html".to_string()),
        "text/rtf" => Some("text/rtf".to_string()),
        "image/png" => Some("image/png".to_string()),
        "text/uri-list" => Some("text/uri-list".to_string()),
        _ if name.starts_with("x-os/") => Some(name.to_string()),
        _ => Some(format!("x-os/{name}")),
    }
}

/// Report whether `name` is a passthrough target (served/stored as
/// `x-os/*`, never mapped to a canonical row).
#[must_use]
pub fn is_passthrough_target(name: &str) -> bool {
    if name.starts_with("x-os/") {
        return true;
    }
    !matches!(
        name,
        "TARGETS"
            | "TIMESTAMP"
            | "UTF8_STRING"
            | "STRING"
            | "TEXT"
            | "text/html"
            | "text/rtf"
            | "image/png"
            | "text/uri-list"
    )
}

/// Build the `TARGETS` reply for canonical MIME payloads: `TARGETS` and
/// `TIMESTAMP` first, then the mapped atoms in input order (deduped), plus
/// legacy `TEXT` whenever a text mapping is present.
#[must_use]
pub fn targets_for(canonical_mimes: &[String]) -> Vec<String> {
    let mut out = vec!["TARGETS".to_string(), "TIMESTAMP".to_string()];
    let mut has_text = false;
    for mime in canonical_mimes {
        let atom = match canonical_to_x11(mime) {
            Some(mapped) => mapped.to_string(),
            None if mime.starts_with("x-os/") => mime.clone(),
            None => format!("x-os/{mime}"),
        };
        if atom == "UTF8_STRING" || atom == "STRING" || atom == "TEXT" {
            has_text = true;
        }
        if !out.contains(&atom) {
            out.push(atom);
        }
    }
    if has_text && !out.contains(&"TEXT".to_string()) {
        out.push("TEXT".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_utf8_roundtrip() {
        assert_eq!(
            canonical_to_x11("text/plain;charset=utf-8"),
            Some("UTF8_STRING")
        );
        assert_eq!(
            x11_to_canonical("UTF8_STRING"),
            Some("text/plain;charset=utf-8".to_string())
        );
    }

    #[test]
    fn string_does_not_map_to_utf8() {
        assert_eq!(canonical_to_x11("text/plain"), Some("STRING"));
        assert_eq!(x11_to_canonical("STRING"), Some("text/plain".to_string()));
        assert_ne!(
            x11_to_canonical("STRING"),
            Some("text/plain;charset=utf-8".to_string())
        );
    }

    #[test]
    fn rich_and_files_roundtrip() {
        for (canonical, atom) in [
            ("text/html", "text/html"),
            ("text/rtf", "text/rtf"),
            ("image/png", "image/png"),
            ("text/uri-list", "text/uri-list"),
        ] {
            assert_eq!(canonical_to_x11(canonical), Some(atom));
            assert_eq!(x11_to_canonical(atom), Some(canonical.to_string()));
        }
    }

    #[test]
    fn unknown_canonical_has_no_mapping() {
        assert_eq!(canonical_to_x11("application/x-foo"), None);
    }

    #[test]
    fn unknown_atom_is_passthrough_both_ways() {
        assert_eq!(x11_to_canonical("FooBar"), Some("x-os/FooBar".to_string()));
        assert_eq!(
            x11_to_canonical("x-os/FooBar"),
            Some("x-os/FooBar".to_string())
        );
        assert!(is_passthrough_target("FooBar"));
        assert!(is_passthrough_target("x-os/FooBar"));
    }

    #[test]
    fn known_targets_are_not_passthrough() {
        for name in [
            "TARGETS",
            "TIMESTAMP",
            "UTF8_STRING",
            "STRING",
            "TEXT",
            "text/html",
            "text/rtf",
            "image/png",
            "text/uri-list",
        ] {
            assert!(!is_passthrough_target(name), "{name}");
        }
    }

    #[test]
    fn targets_list_shape() {
        let targets = targets_for(
            &["text/plain;charset=utf-8", "image/png", "text/uri-list"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            targets,
            vec![
                "TARGETS",
                "TIMESTAMP",
                "UTF8_STRING",
                "image/png",
                "text/uri-list",
                "TEXT"
            ]
        );
    }

    #[test]
    fn empty_payload_has_no_targets() {
        assert_eq!(targets_for(&[]), vec!["TARGETS", "TIMESTAMP"]);
    }

    #[test]
    fn targets_dedup_and_passthrough() {
        let targets = targets_for(
            &["text/plain", "text/plain", "application/x-foo"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            targets,
            vec![
                "TARGETS",
                "TIMESTAMP",
                "STRING",
                "x-os/application/x-foo",
                "TEXT"
            ]
        );
    }
}
