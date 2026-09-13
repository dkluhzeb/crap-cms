//! Lowercase-hex rendering of a byte slice.
//!
//! One implementation for every digest, signature, and checksum the codebase
//! renders — four hand-rolled copies had drifted apart in capacity hints and
//! error handling before this.

use std::fmt::Write as _;

/// Render bytes as lowercase hex, two zero-padded characters per byte.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            // Writing to a String is infallible.
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::hex_encode;

    #[test]
    fn renders_two_padded_lowercase_chars_per_byte() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0x0a]), "0a");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }
}
