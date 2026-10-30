// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 附录 F 逐字节帧规范。
//!
//! 服务器只解释**阶段一帧**：参数集与身份宣告。
//! 阶段二/三/四对服务器是不透明字节，原样转发，不解析、不解密。
//!
//! 字节序全为大端。变长字段统一 `[长度:4B][载荷]`。

use std::fmt;

use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::{timeout_at, Instant};

/// 阶段一参数 JSON 上限（附录 F.6）
pub const PARAM_JSON_MAX: u32 = 4096;
/// 身份宣告上限（附录 F.6）
pub const IDENTITY_MAX: u32 = 8192;
/// nonce 上限（附录 F.6）
pub const NONCE_MAX: u32 = 512;

/// 令牌哈希长度（恒 32B）
pub const TOKEN_HASH_LEN: usize = 32;
/// 身份宣告最小长度：src_hash ‖ dst_hash = 64B
pub const IDENTITY_MIN: u32 = 64;

/// 单个令牌哈希。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenHash(pub [u8; TOKEN_HASH_LEN]);

impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 只打印前 4 字节：完整哈希属于身份材料，不得进日志
        write!(
            f,
            "TokenHash({:02x}{:02x}{:02x}{:02x}…)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

/// 身份宣告解释出的源 / 目标令牌哈希。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub src: TokenHash,
    pub dst: TokenHash,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Identity{{src:{:?}, dst:{:?}}}", self.src, self.dst)
    }
}

/// 阶段一参数集（附录 F.1，默认 7 项）。
#[derive(Clone, Debug, Deserialize)]
pub struct ParamSet {
    pub aead: String,
    pub hash: String,
    pub key_len: u16,
    pub module_flags: serde_json::Map<String, serde_json::Value>,
    /// 接入随机数长度，**以比特计**
    pub random_len: u16,
    pub random_rate: u32,
    pub window: u32,
}

/// 帧解析错误。服务器侧一律走「静默丢弃」，对外不产生任何可观测差异。
#[derive(Debug)]
pub enum FrameError {
    /// 对端干净关闭
    Eof,
    /// 共享截止时间耗尽（Slowloris 防护）
    Timeout,
    /// 长度字段超限
    LengthExceeded(&'static str),
    /// 身份宣告太短，无法解释出 src/dst
    IdentityTooShort,
    /// 参数集非法：JSON 无效、缺项、取值越界
    InvalidParam(&'static str),
    /// nonce 实际长度与声明的 random_len 不一致
    NonceLengthMismatch {
        declared_bits: u16,
        actual_bits: u32,
    },
    /// 源与目标是同一个令牌哈希
    SelfAddressed,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Eof => write!(f, "eof"),
            FrameError::Timeout => write!(f, "timeout"),
            FrameError::LengthExceeded(w) => write!(f, "length exceeded: {w}"),
            FrameError::IdentityTooShort => write!(f, "identity too short"),
            FrameError::InvalidParam(w) => write!(f, "invalid param: {w}"),
            FrameError::NonceLengthMismatch {
                declared_bits,
                actual_bits,
            } => {
                write!(f, "nonce length mismatch: declared {declared_bits} bits, actual {actual_bits} bits")
            }
            FrameError::SelfAddressed => write!(f, "source and destination are identical"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 从流里读满 `buf`，全部字节共享同一个截止时间。
async fn read_exact_deadline<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), FrameError> {
    match timeout_at(deadline, r.read_exact(buf)).await {
        Err(_) => Err(FrameError::Timeout),
        // 守卫臂与兜底臂结果相同（都是 Eof），留着只会让人以为两者有区别
        Ok(Err(_)) => Err(FrameError::Eof),
        Ok(Ok(_)) => Ok(()),
    }
}

/// 读一个 [len:4B][payload] 段。
///
/// 参数 JSON、身份宣告、接入随机数用的是同一种布局，收成一处，
/// 免得三份写法各自漂移。返回长度字段与载荷，调用方把长度字段
/// 原样拼回 raw —— 阶段一帧必须逐字节复现，不能重新编码。
async fn read_len_prefixed<R: AsyncRead + Unpin>(
    r: &mut R,
    deadline: Instant,
    max: u32,
    what: &'static str,
) -> Result<(u32, Vec<u8>), FrameError> {
    let len = read_u32_be(r, deadline).await?;
    if len > max {
        return Err(FrameError::LengthExceeded(what));
    }
    let mut buf = vec![0u8; len as usize];
    if len > 0 {
        read_exact_deadline(r, &mut buf, deadline).await?;
    }
    Ok((len, buf))
}

async fn read_u32_be<R: AsyncRead + Unpin>(
    r: &mut R,
    deadline: Instant,
) -> Result<u32, FrameError> {
    let mut b = [0u8; 4];
    read_exact_deadline(r, &mut b, deadline).await?;
    Ok(u32::from_be_bytes(b))
}

/// 附录 F.6：module_flags 深度上限与节点数上限。
fn check_flags_depth(
    v: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), FrameError> {
    if depth > 32 {
        return Err(FrameError::InvalidParam("module_flags deeper than 32"));
    }
    *nodes += 1;
    if *nodes > 1024 {
        return Err(FrameError::InvalidParam(
            "module_flags has more than 1024 nodes",
        ));
    }
    match v {
        serde_json::Value::Object(m) => {
            for (_, x) in m {
                check_flags_depth(x, depth + 1, nodes)?;
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                check_flags_depth(x, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 校验参数集取值。
fn validate_params(p: &ParamSet, nonce_len: u32) -> Result<(), FrameError> {
    let mut nodes = 0usize;
    for (_, v) in &p.module_flags {
        check_flags_depth(v, 1, &mut nodes)?;
    }
    if p.aead.is_empty() {
        return Err(FrameError::InvalidParam("aead empty"));
    }
    if p.hash.is_empty() {
        return Err(FrameError::InvalidParam("hash empty"));
    }
    match p.key_len {
        128 | 192 | 256 => {}
        _ => return Err(FrameError::InvalidParam("key_len not 128/192/256")),
    }
    if p.random_len == 0 || p.random_len % 8 != 0 {
        return Err(FrameError::InvalidParam(
            "random_len not a positive multiple of 8",
        ));
    }
    if p.random_len as u32 > NONCE_MAX * 8 {
        return Err(FrameError::InvalidParam("random_len exceeds nonce cap"));
    }
    if p.random_rate == 0 {
        return Err(FrameError::InvalidParam("random_rate zero"));
    }
    if p.window == 0 {
        return Err(FrameError::InvalidParam("window zero"));
    }
    // 附录 F.1：nonce 长度 = random_len / 8
    let expected = p.random_len as u32 / 8;
    if nonce_len != expected {
        return Err(FrameError::NonceLengthMismatch {
            declared_bits: p.random_len,
            actual_bits: nonce_len.saturating_mul(8),
        });
    }
    Ok(())
}

/// 阶段一帧（完整原始字节 + 解释结果）。
#[derive(Clone)]
pub struct Phase1Frame {
    /// 原始帧字节，含全部长度前缀；配对后原样转给对端
    pub raw: Vec<u8>,
    /// 已通过校验的参数集
    pub param: ParamSet,
    /// 解释出的身份
    pub identity: Identity,
}

impl Phase1Frame {
    /// 原始帧长度（诊断用）
    pub fn raw_len(&self) -> usize {
        self.raw.len()
    }
}

/// 读一整帧阶段一，并执行附录 F.2 要求的参数核对。
pub async fn read_phase1<R: AsyncRead + Unpin>(
    r: &mut R,
    deadline: Instant,
) -> Result<Phase1Frame, FrameError> {
    let mut raw: Vec<u8> = Vec::with_capacity(256);

    let (param_len, param_json) =
        read_len_prefixed(r, deadline, PARAM_JSON_MAX, "param_json").await?;
    raw.extend_from_slice(&param_len.to_be_bytes());
    raw.extend_from_slice(&param_json);

    let (identity_len, identity_blob) =
        read_len_prefixed(r, deadline, IDENTITY_MAX, "identity_blob").await?;
    if identity_len < IDENTITY_MIN {
        return Err(FrameError::IdentityTooShort);
    }
    raw.extend_from_slice(&identity_len.to_be_bytes());
    raw.extend_from_slice(&identity_blob);

    let (nonce_len, nonce) = read_len_prefixed(r, deadline, NONCE_MAX, "nonce").await?;
    raw.extend_from_slice(&nonce_len.to_be_bytes());
    raw.extend_from_slice(&nonce);

    // 参数集必须是合法 UTF-8 JSON
    let json = std::str::from_utf8(&param_json)
        .map_err(|_| FrameError::InvalidParam("param_json is not utf-8"))?;
    let param: ParamSet = serde_json::from_str(json)
        .map_err(|_| FrameError::InvalidParam("param_json is not a valid parameter set"))?;
    validate_params(&param, nonce_len)?;

    let mut src = [0u8; TOKEN_HASH_LEN];
    let mut dst = [0u8; TOKEN_HASH_LEN];
    src.copy_from_slice(&identity_blob[..TOKEN_HASH_LEN]);
    dst.copy_from_slice(&identity_blob[TOKEN_HASH_LEN..2 * TOKEN_HASH_LEN]);
    if src == dst {
        return Err(FrameError::SelfAddressed);
    }

    let _ = nonce; // 长度已校验；nonce 内容对服务器不透明，不保留
    Ok(Phase1Frame {
        raw,
        param,
        identity: Identity {
            src: TokenHash(src),
            dst: TokenHash(dst),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    fn dl() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    const DEFAULT_PARAM: &str = r#"{"aead":"AES-256-GCM","hash":"SHA3-256","key_len":256,"module_flags":{},"random_len":256,"random_rate":1024,"window":60}"#;

    fn build(param: &str, src: u8, dst: u8, nonce_len: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(param.len() as u32).to_be_bytes());
        b.extend_from_slice(param.as_bytes());
        b.extend_from_slice(&64u32.to_be_bytes());
        b.extend_from_slice(&[src; 32]);
        b.extend_from_slice(&[dst; 32]);
        b.extend_from_slice(&nonce_len.to_be_bytes());
        b.extend_from_slice(&vec![0xCCu8; nonce_len as usize]);
        b
    }

    #[tokio::test]
    async fn parses_default_phase1() {
        assert_eq!(DEFAULT_PARAM.len(), 120);
        let raw = build(DEFAULT_PARAM, 0xAA, 0xBB, 32);
        assert_eq!(raw.len(), 228);
        let mut c = Cursor::new(raw);
        let f = read_phase1(&mut c, dl()).await.unwrap();
        assert_eq!(f.raw_len(), 228);
        assert_eq!(f.identity.src.0, [0xAA; 32]);
        assert_eq!(f.identity.dst.0, [0xBB; 32]);
        assert_eq!(f.param.key_len, 256);
        assert_eq!(f.param.random_len, 256);
    }

    #[tokio::test]
    async fn rejects_non_json_param() {
        let mut c = Cursor::new(build("not json", 0xAA, 0xBB, 0));
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::InvalidParam(_))
        ));
    }

    #[tokio::test]
    async fn rejects_missing_required_key() {
        let p = r#"{"aead":"AES-256-GCM","hash":"SHA3-256","key_len":256,"module_flags":{},"random_len":256,"random_rate":1024}"#;
        let mut c = Cursor::new(build(p, 0xAA, 0xBB, 32));
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::InvalidParam(_))
        ));
    }

    #[tokio::test]
    async fn rejects_nonce_not_matching_random_len() {
        // 声明 random_len=256（即 32B nonce），实际只给 16B
        let mut c = Cursor::new(build(DEFAULT_PARAM, 0xAA, 0xBB, 16));
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::NonceLengthMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn rejects_bad_key_len() {
        let p = r#"{"aead":"AES-256-GCM","hash":"SHA3-256","key_len":999,"module_flags":{},"random_len":256,"random_rate":1024,"window":60}"#;
        let mut c = Cursor::new(build(p, 0xAA, 0xBB, 32));
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::InvalidParam(_))
        ));
    }

    #[tokio::test]
    async fn rejects_self_addressed() {
        let mut c = Cursor::new(build(DEFAULT_PARAM, 0xAA, 0xAA, 32));
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::SelfAddressed)
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_param_length() {
        let mut raw = (PARAM_JSON_MAX + 1).to_be_bytes().to_vec();
        raw.extend_from_slice(&[0u8; 8]);
        let mut c = Cursor::new(raw);
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::LengthExceeded("param_json"))
        ));
    }

    #[tokio::test]
    async fn rejects_short_identity() {
        let mut raw = 2u32.to_be_bytes().to_vec();
        raw.extend_from_slice(b"{}");
        raw.extend_from_slice(&32u32.to_be_bytes());
        raw.extend_from_slice(&[0u8; 32]);
        let mut c = Cursor::new(raw);
        assert!(matches!(
            read_phase1(&mut c, dl()).await,
            Err(FrameError::IdentityTooShort)
        ));
    }

    #[test]
    fn debug_never_prints_full_hash() {
        let t = TokenHash([0xAB; 32]);
        let s = format!("{t:?}");
        assert!(s.len() < 40);
        assert!(!s.contains("abababababababab"));
    }
}
