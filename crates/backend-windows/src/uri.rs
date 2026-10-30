//! `text/uri-list` <-> Windows file paths (pure). Entries are RFC 8089
//! `file:` URIs (`file:///C:/dir/a%20b.txt`, UNC `file://server/share/x`),
//! one per line, CRLF separated as RFC 2483 asks.

/// Build a `text/uri-list` from Windows paths.
#[must_use]
pub fn paths_to_uri_list(paths: &[String]) -> String {
    paths
        .iter()
        .map(|path| path_to_uri(path))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Parse a `text/uri-list` into Windows paths. Comment lines and URIs that
/// do not name a Windows path (other schemes, Unix paths) are skipped; the
/// older `file://C:/x` form and bare paths are accepted too.
#[must_use]
pub fn uri_list_to_paths(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(uri_to_path)
        .collect()
}

fn path_to_uri(path: &str) -> String {
    let forward = path.replace('\\', "/");
    match forward.strip_prefix("//") {
        Some(unc) => format!("file://{}", percent_encode(unc)),
        None => format!("file:///{}", percent_encode(&forward)),
    }
}

fn uri_to_path(line: &str) -> Option<String> {
    let Some(rest) = line.strip_prefix("file://") else {
        return (!line.contains("://") && is_drive_path(line)).then(|| line.replace('/', "\\"));
    };
    let decoded = percent_decode(rest)?;
    let path = match decoded.strip_prefix('/') {
        Some(local) => local.to_string(),
        None if is_drive_path(&decoded) => decoded,
        None => format!("//{decoded}"),
    };
    (is_drive_path(&path) || path.starts_with("//")).then(|| path.replace('/', "\\"))
}

/// `X:` drive-letter prefix.
fn is_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn percent_encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_paths_roundtrip_with_escaping() {
        let paths = vec![
            r"C:\Users\A B\report (1).txt".to_string(),
            r"D:\Fotky\léto.png".to_string(),
        ];
        let list = paths_to_uri_list(&paths);
        assert_eq!(
            list,
            "file:///C:/Users/A%20B/report%20%281%29.txt\r\nfile:///D:/Fotky/l%C3%A9to.png"
        );
        assert_eq!(uri_list_to_paths(&list), paths);
    }

    #[test]
    fn unc_paths_roundtrip() {
        let paths = vec![r"\\server\share\dir\x.doc".to_string()];
        let list = paths_to_uri_list(&paths);
        assert_eq!(list, "file://server/share/dir/x.doc");
        assert_eq!(uri_list_to_paths(&list), paths);
    }

    #[test]
    fn legacy_forms_comments_and_foreign_entries() {
        let text = "# copied files\nfile://C:/old/form.txt\nC:\\bare\\path.txt\n\
                    file:///home/user/linux.txt\nhttps://example.com/x\n";
        assert_eq!(
            uri_list_to_paths(text),
            vec![
                r"C:\old\form.txt".to_string(),
                r"C:\bare\path.txt".to_string()
            ]
        );
    }

    #[test]
    fn broken_escapes_are_skipped() {
        assert_eq!(
            uri_list_to_paths("file:///C:/bad%zz.txt"),
            Vec::<String>::new()
        );
        assert_eq!(uri_list_to_paths("file:///C:/cut%4"), Vec::<String>::new());
    }
}
