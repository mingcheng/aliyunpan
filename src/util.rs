use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn hex_with(bytes: &[u8], table: &[u8; 16]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(table[(b >> 4) as usize] as char);
        out.push(table[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    hex_with(bytes, HEX_LOWER)
}

pub(crate) fn hex_upper(bytes: &[u8]) -> String {
    hex_with(bytes, HEX_UPPER)
}

/// Standard base64 with padding (RFC 4648 §4).
pub(crate) fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(chunk[0]) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(BASE64[(n >> 18) as usize & 63] as char);
        out.push(BASE64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| Error::Crypto(format!("system rng: {e}")))?;
    Ok(buf)
}

/// Generate a 32-character lowercase hexadecimal device ID.
pub(crate) fn random_device_id() -> Result<String> {
    Ok(hex_lower(&random_bytes::<16>()?))
}

/// Validate a file name against Aliyun Drive's length and character restrictions.
pub fn validate_file_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidInput("file name is empty".into()));
    }
    if name.len() > 1024 {
        return Err(Error::InvalidInput("file name exceeds 1024 bytes".into()));
    }
    if let Some(c) = name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c)) {
        return Err(Error::InvalidInput(format!(
            "file name contains forbidden character {c:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encoding() {
        assert_eq!(hex_lower(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(hex_upper(&[0x00, 0xab, 0xff]), "00ABFF");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn base64_rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(base64_encode(input.as_bytes()), expected, "input {input:?}");
        }
        assert_eq!(base64_encode(&[0xfb, 0xff, 0xfe]), "+//+");
    }

    #[test]
    fn device_id_format() {
        let id = random_device_id().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(id, random_device_id().unwrap());
    }

    #[test]
    fn file_name_rules() {
        assert!(validate_file_name("报告 2026.pdf").is_ok());
        assert!(validate_file_name("").is_err());
        assert!(validate_file_name("a/b").is_err());
        assert!(validate_file_name("a|b").is_err());
        assert!(validate_file_name(&"a".repeat(1025)).is_err());
        assert!(validate_file_name(&"a".repeat(1024)).is_ok());
    }
}
