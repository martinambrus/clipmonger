//! Pure, bounded derivation of searchable file names from stored URI lists.
//! This module never accesses the paths named by the list.

pub const URI_PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
pub const URI_SEARCH_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriSearchError {
    TooLarge,
    InvalidUtf8,
    InvalidEntry,
}

/// Derive names, rejecting an entire list if any non-comment entry cannot be
/// represented. A caller must leave such a list pending for repair or retry.
pub fn basenames(bytes: &[u8]) -> Result<String, UriSearchError> {
    if bytes.len() > URI_PAYLOAD_LIMIT {
        return Err(UriSearchError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| UriSearchError::InvalidUtf8)?;
    let mut output = String::new();
    for line in text.split(['\r', '\n']).map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (path, escapes) = if line
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
        {
            let rest = &line[5..];
            if !rest.starts_with('/') || rest.contains(['?', '#']) {
                return Err(UriSearchError::InvalidEntry);
            }
            if let Some(authority) = rest.strip_prefix("//") {
                if !authority.contains('/') {
                    return Err(UriSearchError::InvalidEntry);
                }
            }
            (rest, true)
        } else {
            let b = line.as_bytes();
            let drive = b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && matches!(b[2], b'/' | b'\\');
            if !drive && !line.starts_with("\\\\") && !line.starts_with("//") {
                return Err(UriSearchError::InvalidEntry);
            }
            (line, false)
        };
        // Validate every escape, not just the last segment, before accepting it.
        let mut decoded = Vec::with_capacity(path.len());
        let raw = path.as_bytes();
        let mut segment_start = 0;
        let mut i = 0;
        while i < raw.len() {
            match raw[i] {
                b'/' | b'\\' => {
                    decoded.push(raw[i]);
                    segment_start = decoded.len();
                    i += 1;
                }
                b'%' if escapes => {
                    let hex = |b: u8| match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    };
                    let h = raw
                        .get(i + 1)
                        .and_then(|b| hex(*b))
                        .ok_or(UriSearchError::InvalidEntry)?;
                    let l = raw
                        .get(i + 2)
                        .and_then(|b| hex(*b))
                        .ok_or(UriSearchError::InvalidEntry)?;
                    decoded.push((h << 4) | l);
                    i += 3;
                }
                b => {
                    decoded.push(b);
                    i += 1;
                }
            }
        }
        if decoded.contains(&0) {
            return Err(UriSearchError::InvalidEntry);
        }
        std::str::from_utf8(&decoded).map_err(|_| UriSearchError::InvalidUtf8)?;
        let name = std::str::from_utf8(&decoded[segment_start..])
            .map_err(|_| UriSearchError::InvalidUtf8)?;
        if name.is_empty() || name.contains('\0') {
            return Err(UriSearchError::InvalidEntry);
        }
        if !output.is_empty() {
            output.push('\n');
        }
        if output.len().saturating_add(name.len()) > URI_SEARCH_LIMIT {
            return Err(UriSearchError::TooLarge);
        }
        output.push_str(name);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_invalid_lists() {
        assert_eq!(basenames(b" # comment\r\nfile:///tmp/caf%C3%A9.txt\rfile://server/share/a%2Fb\nC:\\dir\\z.txt\n"), Ok("café.txt\na/b\nz.txt".into()));
        for invalid in [
            b"https://example/a".as_slice(),
            b"file:///bad%ZZ",
            b"file:///bad%FF",
            b"file:///dir/",
        ] {
            assert!(basenames(invalid).is_err());
        }
    }
    #[test]
    fn basename_beyond_one_mebibyte() {
        let mut list = format!("#{}\nfile:///tmp/late%20name.txt", "a".repeat(1024 * 1024));
        assert_eq!(basenames(list.as_bytes()).unwrap(), "late name.txt");
        list.push_str("\nfile:///bad%ZZ");
        assert!(basenames(list.as_bytes()).is_err());
    }
}
