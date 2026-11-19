//! Standard base64 decoding, for icons carried in scriptlet headers (`# Icon: data:image/png;base64,...`).

/// Decode standard base64 (`+` `/`, optional `=` padding). Whitespace is skipped. Returns `None` on any
/// other character.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_padded_and_unpadded_text() {
        assert_eq!(decode("TWFu").unwrap(), b"Man");
        assert_eq!(decode("TWE=").unwrap(), b"Ma");
        assert_eq!(decode("TQ==").unwrap(), b"M");
        assert_eq!(decode("TQ").unwrap(), b"M");
        assert_eq!(decode("aGVs\nbG8=").unwrap(), b"hello");
    }

    #[test]
    fn rejects_other_characters() {
        assert!(decode("TW*u").is_none());
    }
}
