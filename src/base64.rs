//! Minimal RFC 4648 standard base64 codec for measurement payloads.
//!
//! The format study needs real encoded byte counts for packed attribute
//! streams without adding a dependency; keep this tiny and total.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = u32::from(*chunk.get(1).unwrap_or(&0));
        let third = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (first << 16) | (second << 8) | third;
        output.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        output.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    output
}

fn decode_symbol(symbol: u8) -> Result<u32, String> {
    match symbol {
        b'A'..=b'Z' => Ok(u32::from(symbol - b'A')),
        b'a'..=b'z' => Ok(u32::from(symbol - b'a') + 26),
        b'0'..=b'9' => Ok(u32::from(symbol - b'0') + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(format!("invalid base64 symbol 0x{symbol:02x}")),
    }
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of four".to_owned());
    }
    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let padding = chunk.iter().filter(|symbol| **symbol == b'=').count();
        if padding > 0 && (!last || padding > 2) {
            return Err("base64 padding only allowed at the end".to_owned());
        }
        let mut triple = 0u32;
        for (position, symbol) in chunk.iter().enumerate() {
            if *symbol == b'=' {
                if position < 4 - padding {
                    return Err("base64 padding must be trailing".to_owned());
                }
                triple <<= 6;
            } else {
                triple = (triple << 6) | decode_symbol(*symbol)?;
            }
        }
        output.push((triple >> 16) as u8);
        if padding < 2 {
            output.push((triple >> 8) as u8);
        }
        if padding < 1 {
            output.push(triple as u8);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn round_trips_all_remainders() {
        for bytes in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0u8, 159, 146, 150, 255, 0, 1][..],
        ] {
            let encoded = encode(bytes);
            assert_eq!(encoded.len(), bytes.len().div_ceil(3) * 4);
            assert_eq!(decode(&encoded).as_deref(), Ok(bytes));
        }
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(decode("abc").is_err());
        assert!(decode("ab=d").is_err());
        assert!(decode("abcd=efg").is_err());
        assert!(decode("ab\u{0}d").is_err());
    }
}
