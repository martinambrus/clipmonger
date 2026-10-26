//! Canonical content hash (D-F4) and stable search text (seg-03 S8).

use sha2::{Digest, Sha256};

/// Hash payloads per D-F4: sorted canonical MIME keys, each followed by
/// `mime + 0x00 + u64LE len + normalized bytes`. Text is CRLF-normalized,
/// uri-lists are sorted, images hash as stored PNG bytes (Step 1).
/// `x-os/*` natives are excluded so equal content hashes equal cross-OS.
#[must_use]
pub fn content_hash(payloads: &[(String, Vec<u8>)]) -> String {
    let mut items: Vec<(&str, Vec<u8>)> = Vec::with_capacity(payloads.len());
    for (mime, bytes) in payloads {
        if is_native(mime) {
            continue;
        }
        items.push((mime.as_str(), normalize(mime, bytes)));
    }
    items.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = Sha256::new();
    for (mime, bytes) in &items {
        hasher.update(mime.as_bytes());
        hasher.update([0x00]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex_encode(&hasher.finalize())
}

/// Stable width-independent search text: full text + newline + URI
/// basenames + newline + `source_app` (omitted when `strict`).
#[must_use]
pub fn make_search_text(
    text: &str,
    uris: &[String],
    source_app: Option<&str>,
    strict: bool,
) -> String {
    let mut out = String::from(text);
    out.push('\n');
    let mut first = true;
    for uri in uris {
        if !first {
            out.push(' ');
        }
        first = false;
        out.push_str(basename(uri));
    }
    out.push('\n');
    if !strict {
        if let Some(app) = source_app {
            out.push_str(app);
        }
    }
    out
}

/// True for native passthrough formats excluded from the canonical hash.
fn is_native(mime: &str) -> bool {
    mime.starts_with("x-os/") || mime.starts_with("x-os:")
}

/// Normalize payload bytes for hashing.
fn normalize(mime: &str, bytes: &[u8]) -> Vec<u8> {
    if mime == "text/uri-list" {
        // Sorted URI list: normalize endings, drop blanks, sort, rejoin.
        let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
        let text = text.replace('\r', "\n");
        let mut lines: Vec<&str> = text.split('\n').filter(|l| !l.trim().is_empty()).collect();
        lines.sort_unstable();
        return lines.join("\n").into_bytes();
    }
    if mime.starts_with("text/") {
        // UTF-8 text with CRLF -> LF.
        let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
        return text.replace('\r', "\n").into_bytes();
    }
    bytes.to_vec()
}

/// Basename of a URI (after the last `/` or `\`).
fn basename(uri: &str) -> &str {
    let uri = uri.trim();
    let after_slash = uri.rsplit('/').next().unwrap_or(uri);
    after_slash.rsplit('\\').next().unwrap_or(after_slash)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
