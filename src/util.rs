// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 小工具：十六进制编解码。不引入额外依赖，保持二进制干净可移植。

/// 字节 → 小写十六进制
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// 十六进制 → 字节。非法字符或奇数长度返回 None。
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(out)
}

/// 解析 32 字节令牌哈希（64 个十六进制字符）。
pub fn parse_token_hash(s: &str) -> Option<crate::frame::TokenHash> {
    let bytes = from_hex(s)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(crate::frame::TokenHash(arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let data = [0x00u8, 0x0f, 0xa5, 0xff, 0x10];
        let h = to_hex(&data);
        assert_eq!(h, "000fa5ff10");
        assert_eq!(from_hex(&h).unwrap(), data);
    }

    #[test]
    fn hex_rejects_bad_input() {
        assert!(from_hex("abc").is_none(), "奇数长度应拒绝");
        assert!(from_hex("zz").is_none(), "非法字符应拒绝");
    }

    #[test]
    fn token_hash_requires_32_bytes() {
        assert!(parse_token_hash(&"ab".repeat(32)).is_some());
        assert!(parse_token_hash(&"ab".repeat(16)).is_none());
    }
}
