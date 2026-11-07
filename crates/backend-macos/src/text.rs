//! Text encodings for the pasteboard (pure, T-803).
//!
//! UTF-16 goes out little-endian WITHOUT a BOM (`CopyQ` write quirk: some
//! apps paste the BOM as a character); a BOM on read is stripped.

/// UTF-16LE bytes of `text`, no BOM.
#[must_use]
pub fn utf16_no_bom(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .filter(|unit| *unit != 0xFEFF)
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Decode UTF-16 bytes (LE unless a BOM says otherwise), lossy.
#[must_use]
pub fn utf16_to_utf8(bytes: &[u8]) -> String {
    let (big, body) = match bytes {
        [0xFE, 0xFF, rest @ ..] => (true, rest),
        [0xFF, 0xFE, rest @ ..] => (false, rest),
        _ => (false, bytes),
    };
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if big {
                u16::from_be_bytes(*pair)
            } else {
                u16::from_le_bytes(*pair)
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Split `text` into UTF-16 chunks of at most `max` units without cutting
/// a surrogate pair (`CGEventKeyboardSetUnicodeString` takes ~20 per event).
#[must_use]
pub fn utf16_chunks(text: &str, max: usize) -> Vec<Vec<u16>> {
    let max = max.max(2);
    let mut out = Vec::new();
    let mut current: Vec<u16> = Vec::with_capacity(max);
    let mut buf = [0u16; 2];
    for c in text.chars() {
        let units = c.encode_utf16(&mut buf);
        if current.len() + units.len() > max {
            out.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(units);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t803_no_bom_vectors() {
        assert_eq!(utf16_no_bom(""), Vec::<u8>::new());
        assert_eq!(utf16_no_bom("A"), [0x41, 0x00]);
        assert_eq!(utf16_no_bom("é€"), [0xE9, 0x00, 0xAC, 0x20]);
        assert_eq!(utf16_no_bom("😀"), [0x3D, 0xD8, 0x00, 0xDE]);
        assert_eq!(
            utf16_no_bom("\u{FEFF}x"),
            [0x78, 0x00],
            "a leading BOM in the text is not written"
        );
        assert_ne!(utf16_no_bom("x").get(..2), Some(&[0xFF, 0xFE][..]));
    }

    #[test]
    fn utf16_decode_handles_boms_and_odd_bytes() {
        assert_eq!(utf16_to_utf8(&utf16_no_bom("héllo 😀")), "héllo 😀");
        assert_eq!(utf16_to_utf8(&[0xFF, 0xFE, 0x41, 0x00]), "A");
        assert_eq!(utf16_to_utf8(&[0xFE, 0xFF, 0x00, 0x41]), "A");
        assert_eq!(utf16_to_utf8(&[0x41, 0x00, 0x42]), "A");
    }

    #[test]
    fn chunks_keep_surrogate_pairs_whole() {
        let text = format!("{}😀b", "a".repeat(19));
        let chunks = utf16_chunks(&text, 20);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 19);
        assert_eq!(chunks[1], "😀b".encode_utf16().collect::<Vec<_>>());
        assert_eq!(utf16_chunks("", 20), Vec::<Vec<u16>>::new());
        let long = "x".repeat(45);
        let sizes: Vec<usize> = utf16_chunks(&long, 20).iter().map(Vec::len).collect();
        assert_eq!(sizes, [20, 20, 5]);
    }
}
