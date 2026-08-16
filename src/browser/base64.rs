//! Just enough base64 to read what the extension sends.
//!
//! Decode only, and hand-rolled rather than another dependency for 30 lines.

const INVALID: u8 = 0xFF;

fn sextet(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => INVALID,
    }
}

/// `None` on anything malformed. Whitespace is skipped, padding optional.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;

    for byte in text.bytes() {
        if byte.is_ascii_whitespace() || byte == b'=' {
            continue;
        }
        let value = sextet(byte);
        if value == INVALID {
            return None;
        }
        accumulator = (accumulator << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }

    // Leftover bits are padding and must be zero; anything else is a truncated
    // or corrupt string rather than a short one.
    if bits >= 6 || (accumulator & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_usual_cases() {
        assert_eq!(decode("").unwrap(), b"");
        assert_eq!(decode("Zg==").unwrap(), b"f");
        assert_eq!(decode("Zm8=").unwrap(), b"fo");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn padding_is_optional_and_whitespace_is_ignored() {
        assert_eq!(decode("Zm8").unwrap(), b"fo");
        assert_eq!(decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(decode(" Zm9vYmFy ").unwrap(), b"foobar");
    }

    #[test]
    fn round_trips_every_byte_value() {
        const ALPHABET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let raw: Vec<u8> = (0..=255u8).collect();
        let mut encoded = String::new();
        for chunk in raw.chunks(3) {
            let mut block = [0u8; 3];
            block[..chunk.len()].copy_from_slice(chunk);
            let n = u32::from_be_bytes([0, block[0], block[1], block[2]]);
            for i in 0..4 {
                if i <= chunk.len() {
                    encoded.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
                } else {
                    encoded.push('=');
                }
            }
        }
        assert_eq!(decode(&encoded).unwrap(), raw);
    }

    #[test]
    fn junk_is_rejected_rather_than_guessed_at() {
        assert!(decode("****").is_none());
        assert!(decode("Zm9v!").is_none());
        // One leftover sextet cannot be part of any encoding.
        assert!(decode("Z").is_none());
    }
}
