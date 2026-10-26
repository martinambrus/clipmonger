//! `CF_HTML` builder/parser (pure).

use std::fmt;

/// `CF_HTML` parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlError {
    /// Input is not valid UTF-8.
    NotUtf8,
    /// Required header field missing.
    MissingField(&'static str),
    /// Offset value malformed.
    BadOffset(&'static str),
    /// Offsets out of range or inverted.
    BadRange,
    /// Fragment slice not valid UTF-8.
    BadFragment,
}

impl fmt::Display for HtmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 => write!(f, "not utf-8"),
            Self::MissingField(k) => write!(f, "missing field {k}"),
            Self::BadOffset(k) => write!(f, "bad offset {k}"),
            Self::BadRange => write!(f, "bad range"),
            Self::BadFragment => write!(f, "bad fragment"),
        }
    }
}

impl std::error::Error for HtmlError {}

/// Build a `CF_HTML` byte blob wrapping `fragment_html`.
#[must_use]
pub fn build_cf_html(fragment_html: &str) -> Vec<u8> {
    let pre = "<html>\r\n<body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body>\r\n</html>";
    let probe = format_header(0, 0, 0, 0);
    let base = probe.len() + pre.len();
    let start_fragment = base;
    let end_fragment = base + fragment_html.len();
    let end_html = end_fragment + post.len();
    let header = format_header(0, end_html, start_fragment, end_fragment);
    let mut out = Vec::with_capacity(header.len() + pre.len() + fragment_html.len() + post.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(pre.as_bytes());
    out.extend_from_slice(fragment_html.as_bytes());
    out.extend_from_slice(post.as_bytes());
    out
}

/// Parse a `CF_HTML` blob back to its fragment.
///
/// # Errors
///
/// Returns [`HtmlError`] on empty/garbage input, bad offsets, or truncation.
pub fn parse_cf_html(bytes: &[u8]) -> Result<String, HtmlError> {
    let text = std::str::from_utf8(bytes).map_err(|_| HtmlError::NotUtf8)?;
    if text.is_empty() {
        return Err(HtmlError::MissingField("header"));
    }
    let start_fragment = required_offset(text, "StartFragment")?;
    let end_fragment = required_offset(text, "EndFragment")?;
    if start_fragment > end_fragment || end_fragment > bytes.len() {
        return Err(HtmlError::BadRange);
    }
    if let (Some(start_html), Some(end_html)) = (
        optional_offset(text, "StartHTML")?,
        optional_offset(text, "EndHTML")?,
    ) {
        if start_html > end_html || end_html > bytes.len() {
            return Err(HtmlError::BadRange);
        }
    }
    let fragment = std::str::from_utf8(&bytes[start_fragment..end_fragment])
        .map_err(|_| HtmlError::BadFragment)?;
    Ok(fragment.to_string())
}

/// Format the `CF_HTML` header with fixed-width offsets.
fn format_header(
    start_html: usize,
    end_html: usize,
    start_fragment: usize,
    end_fragment: usize,
) -> String {
    format!(
        "Version:0.9\r\nStartHTML:{start_html:08}\r\nEndHTML:{end_html:08}\r\nStartFragment:{start_fragment:08}\r\nEndFragment:{end_fragment:08}\r\n"
    )
}

/// Read a header field value from the leading header lines.
fn header_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.split(['\r', '\n']) {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('<') {
            break;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.strip_prefix(':') {
                return Some(value.trim());
            }
        }
    }
    None
}

/// Parse a present header field as a byte offset.
fn parse_offset(raw: &str, key: &'static str) -> Result<usize, HtmlError> {
    raw.parse::<usize>().map_err(|_| HtmlError::BadOffset(key))
}

/// Read a required fragment offset field.
fn required_offset(text: &str, key: &'static str) -> Result<usize, HtmlError> {
    let raw = header_field(text, key).ok_or(HtmlError::MissingField(key))?;
    parse_offset(raw, key)
}

/// Read an optional envelope offset field.
fn optional_offset(text: &str, key: &'static str) -> Result<Option<usize>, HtmlError> {
    header_field(text, key)
        .map(|raw| parse_offset(raw, key))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ascii() {
        let frag = "<p>hello <b>world</b></p>";
        let blob = build_cf_html(frag);
        assert_eq!(parse_cf_html(&blob).unwrap(), frag);
    }

    #[test]
    fn roundtrip_utf8() {
        let frag = "<p>héllo 🌍 日本語</p>";
        let blob = build_cf_html(frag);
        assert_eq!(parse_cf_html(&blob).unwrap(), frag);
    }

    #[test]
    fn roundtrip_empty_fragment() {
        let blob = build_cf_html("");
        assert_eq!(parse_cf_html(&blob).unwrap(), "");
    }

    #[test]
    fn reject_empty() {
        assert!(parse_cf_html(&[]).is_err());
    }

    #[test]
    fn reject_no_header() {
        assert!(parse_cf_html(b"<html><body>hi</body></html>").is_err());
    }

    #[test]
    fn reject_garbage() {
        assert!(parse_cf_html(b"\x00\x01\x02not html at all\xff\xfe").is_err());
    }

    #[test]
    fn reject_bad_offset() {
        let blob = b"Version:0.9\r\nStartHTML:00000097\r\nEndHTML:zz\r\nStartFragment:1\r\nEndFragment:2\r\nxx";
        assert!(parse_cf_html(blob).is_err());
    }

    #[test]
    fn reject_inverted_range() {
        let blob = b"Version:0.9\r\nStartFragment:20\r\nEndFragment:10\r\n0123456789";
        assert!(parse_cf_html(blob).is_err());
    }

    #[test]
    fn reject_offsets_beyond_len() {
        let blob = b"Version:0.9\r\nStartFragment:0\r\nEndFragment:99999\r\nshort";
        assert!(parse_cf_html(blob).is_err());
    }

    #[test]
    fn reject_truncated_blob() {
        let blob = build_cf_html("<p>truncated</p>");
        let cut = &blob[..blob.len() / 2];
        assert!(parse_cf_html(cut).is_err());
    }

    #[test]
    fn tolerant_missing_optional_fields() {
        let frag = "<p>optional</p>";
        let start_fragment = "StartFragment:00000000\r\nEndFragment:00000000\r\n".len();
        let end_fragment = start_fragment + frag.len();
        let minimal =
            format!("StartFragment:{start_fragment:08}\r\nEndFragment:{end_fragment:08}\r\n{frag}");
        assert_eq!(parse_cf_html(minimal.as_bytes()).unwrap(), frag);
    }
}
