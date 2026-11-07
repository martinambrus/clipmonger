//! `text/uri-list` <-> pasteboard file URLs (pure). Entries are `file://`
//! URIs (percent-encoded, as `NSURL.absoluteString` gives them), CRLF
//! separated per RFC 2483 like the other backends.

/// Join URIs into a `text/uri-list` (no trailing CRLF, as Windows).
#[must_use]
pub fn join(uris: &[String]) -> String {
    uris.join("\r\n")
}

/// The `file://` URIs of a `text/uri-list`; comments, blanks and other
/// schemes are skipped.
#[must_use]
pub fn file_uris(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| {
            line.get(..7)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_roundtrip_and_filtering() {
        let uris = vec![
            "file:///Users/a/My%20File.txt".to_string(),
            "file:///Users/a/Folder/".to_string(),
        ];
        let list = join(&uris);
        assert_eq!(
            list,
            "file:///Users/a/My%20File.txt\r\nfile:///Users/a/Folder/"
        );
        assert_eq!(file_uris(&list), uris);
        assert_eq!(
            file_uris("# x\nhttps://e.com\n\nFILE:///tmp/a\r\nfile:///tmp/b\n"),
            ["FILE:///tmp/a", "file:///tmp/b"]
        );
        assert_eq!(file_uris(""), Vec::<String>::new());
    }
}
