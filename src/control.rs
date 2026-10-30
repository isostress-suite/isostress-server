// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! L4 控制面：JSON Lines over TCP。
//!
//! 与数据面完全隔离，不碰 isostress 线上帧。**默认关闭**，且启用时强制要求口令。
//!
//! 访问控制只有口令校验，没有按令牌的身份绑定 —— 因此本控制面按设计仅供
//! 运营者自己使用（运维脚本、管理端的账号体系在外层）。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use crate::l4::{Directory, Groups, L4Error, OfflineStore};
use crate::metrics::Metrics;
use crate::util::{from_hex, parse_token_hash, to_hex};

/// 单行请求上限
pub const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
/// 控制面并发连接上限
pub const MAX_CONTROL_CONNS: usize = 16;
/// 读完一行请求的空闲超时
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 单连接允许的鉴权失败次数
pub const MAX_AUTH_FAILURES: u32 = 5;

/// 控制面共享状态。
pub struct ControlState {
    pub offline: Arc<OfflineStore>,
    pub groups: Arc<Groups>,
    pub directory: Arc<Directory>,
    pub metrics: Arc<Metrics>,
    /// 必填：没有口令就不启动控制面
    pub token: String,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
enum Req {
    Auth {
        token: String,
    },
    Ping,
    OfflinePush {
        dst: String,
        payload: String,
    },
    OfflinePull {
        dst: String,
        max: Option<usize>,
    },
    GroupCreate {
        name: String,
        owner: String,
    },
    GroupJoin {
        name: String,
        member: String,
    },
    GroupSend {
        name: String,
        sender: String,
        payload: String,
    },
    GroupPull {
        name: String,
        member: String,
    },
    DirLookup {
        hash: String,
    },
    Stats,
}

#[derive(Serialize)]
struct Resp {
    ok: bool,
    /// 稳定的机器可读码。机器客户端只认这个；error 字段是给人看的，可本地化。
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

impl Resp {
    fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            error_code: None,
            error: None,
            data: Some(data),
        }
    }
    /// 请求本身不合法（形状、字段类型），无领域错误码。
    fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error_code: Some("bad_request"),
            error: Some(e.to_string()),
            data: None,
        }
    }
}

fn e(err: L4Error) -> Resp {
    Resp {
        ok: false,
        error_code: Some(err.code()),
        error: Some(err.to_string()),
        data: None,
    }
}

/// 常数时间比较，避免按字节短路泄露口令前缀。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    // 长度不同也走完整轮比较：用耗时去区分长度差本身就是侧信道。
    let n = a.len().max(b.len());
    let mut acc: u8 = 0;
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        acc |= x ^ y;
    }
    acc |= (a.len() != b.len()) as u8;
    acc == 0
}

/// 读一行，超过上限即报错（不无限增长缓冲区）。
async fn read_line_limited<R: AsyncBufRead + Unpin>(
    r: &mut R,
    max: usize,
    out: &mut Vec<u8>,
) -> std::io::Result<Option<usize>> {
    out.clear();
    loop {
        // 只 fill_buf 一次：两次调用之间缓冲区可能被改写，chunk_len 就会失配。
        let (chunk_len, hit_newline) = {
            let available = r.fill_buf().await?;
            if available.is_empty() {
                if out.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(out.len()));
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(p) => (p + 1, true),
                None => (available.len(), false),
            }
        };

        if out.len() + chunk_len > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "control line too long",
            ));
        }
        {
            let available = r.fill_buf().await?;
            out.extend_from_slice(&available[..chunk_len]);
        }
        r.consume(chunk_len);
        if hit_newline {
            return Ok(Some(out.len()));
        }
    }
}

/// 启动控制面。
pub async fn serve(addr: SocketAddr, state: Arc<ControlState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "控制面已监听（需要口令）");
    let live = Arc::new(AtomicUsize::new(0));

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(err) => {
                warn!(error = %err, "控制面 accept 失败");
                continue;
            }
        };

        if live.load(Ordering::Relaxed) >= MAX_CONTROL_CONNS {
            debug!(%peer, "控制面连接数已满，拒绝");
            drop(stream);
            continue;
        }
        live.fetch_add(1, Ordering::Relaxed);

        let child_state = Arc::clone(&state);
        let live_count = Arc::clone(&live);
        tokio::spawn(async move {
            if let Err(err) = handle(stream, child_state).await {
                debug!(%peer, error = %err, "控制面连接结束");
            }
            live_count.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

async fn handle(stream: TcpStream, state: Arc<ControlState>) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = Vec::new();
    let mut authed = false;
    let mut failures: u32 = 0;

    loop {
        let read = tokio::time::timeout(
            READ_TIMEOUT,
            read_line_limited(&mut reader, MAX_LINE_BYTES, &mut line),
        )
        .await;

        let n = match read {
            Err(_) => return Ok(()),
            Ok(Err(err)) => {
                let r = Resp::err(format!("请求行非法: {err}"));
                write_resp(&mut write_half, &r).await?;
                return Ok(());
            }
            Ok(Ok(None)) => return Ok(()),
            Ok(Ok(Some(n))) => n,
        };
        if n == 0 {
            continue;
        }

        let text = match std::str::from_utf8(&line) {
            Ok(t) => t.trim(),
            Err(_) => {
                write_resp(&mut write_half, &Resp::err("请求不是合法 UTF-8")).await?;
                continue;
            }
        };
        if text.is_empty() {
            continue;
        }

        let resp = match serde_json::from_str::<Req>(text) {
            Err(err) => Resp::err(format!("请求解析失败: {err}")),
            Ok(Req::Auth { token }) => {
                if ct_eq(token.as_bytes(), state.token.as_bytes()) {
                    authed = true;
                    Resp::ok(serde_json::json!({"authed": true}))
                } else {
                    failures += 1;
                    if failures >= MAX_AUTH_FAILURES {
                        write_resp(&mut write_half, &Resp::err("鉴权失败次数过多")).await?;
                        return Ok(());
                    }
                    Resp::err("鉴权失败")
                }
            }
            Ok(_) if !authed => Resp::err("未鉴权：请先发送 auth 请求"),
            Ok(req) => dispatch(req, &state),
        };

        write_resp(&mut write_half, &resp).await?;
    }
}

async fn write_resp<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, r: &Resp) -> std::io::Result<()> {
    // 序列化失败不能静默替换成假响应：那会把内部错误伪装成服务端拒绝。
    let mut out = match serde_json::to_vec(r) {
        Ok(v) => v,
        Err(err) => {
            warn!(error = %err, "控制面响应序列化失败");
            br#"{"ok":false,"error_code":"internal"}"#.to_vec()
        }
    };
    out.push(b'\n');
    w.write_all(&out).await?;
    w.flush().await
}

fn dispatch(req: Req, state: &ControlState) -> Resp {
    match req {
        Req::Auth { .. } => Resp::ok(serde_json::json!({"authed": true})),
        Req::Ping => Resp::ok(serde_json::json!({"pong": true})),

        Req::OfflinePush { dst, payload } => {
            let Some(d) = parse_token_hash(&dst) else {
                return Resp::err("dst 必须是 64 个十六进制字符");
            };
            let Some(bytes) = from_hex(&payload) else {
                return Resp::err("payload 必须是十六进制");
            };
            match state.offline.push(d, bytes) {
                Ok(depth) => {
                    Metrics::inc(&state.metrics.offline_stored);
                    Resp::ok(serde_json::json!({"depth": depth}))
                }
                Err(err) => e(err),
            }
        }

        Req::OfflinePull { dst, max } => {
            let Some(d) = parse_token_hash(&dst) else {
                return Resp::err("dst 必须是 64 个十六进制字符");
            };
            match state.offline.pull(d, max.unwrap_or(100)) {
                Ok(items) => {
                    Metrics::add(&state.metrics.offline_delivered, items.len() as u64);
                    let hexed: Vec<String> = items.iter().map(|v| to_hex(v)).collect();
                    Resp::ok(serde_json::json!({"items": hexed}))
                }
                Err(err) => e(err),
            }
        }

        Req::GroupCreate { name, owner } => {
            let Some(o) = parse_token_hash(&owner) else {
                return Resp::err("owner 必须是 64 个十六进制字符");
            };
            match state.groups.create(&name, o) {
                Ok(()) => Resp::ok(serde_json::json!({"created": name})),
                Err(err) => e(err),
            }
        }

        Req::GroupJoin { name, member } => {
            let Some(m) = parse_token_hash(&member) else {
                return Resp::err("member 必须是 64 个十六进制字符");
            };
            match state.groups.join(&name, m) {
                Ok(n) => Resp::ok(serde_json::json!({"members": n})),
                Err(err) => e(err),
            }
        }

        Req::GroupSend {
            name,
            sender,
            payload,
        } => {
            let Some(s) = parse_token_hash(&sender) else {
                return Resp::err("sender 必须是 64 个十六进制字符");
            };
            let Some(bytes) = from_hex(&payload) else {
                return Resp::err("payload 必须是十六进制");
            };
            match state.groups.send(&name, s, bytes) {
                Ok(cursor) => {
                    Metrics::inc(&state.metrics.group_broadcasts);
                    Resp::ok(serde_json::json!({"cursor": cursor}))
                }
                Err(err) => e(err),
            }
        }

        Req::GroupPull { name, member } => {
            let Some(m) = parse_token_hash(&member) else {
                return Resp::err("member 必须是 64 个十六进制字符");
            };
            match state.groups.pull(&name, m) {
                Ok(items) => {
                    let out: Vec<serde_json::Value> = items
                        .into_iter()
                        .map(|(c, p)| serde_json::json!({"cursor": c, "payload": to_hex(&p)}))
                        .collect();
                    Resp::ok(serde_json::json!({"items": out}))
                }
                Err(err) => e(err),
            }
        }

        Req::DirLookup { hash } => {
            let Some(h) = parse_token_hash(&hash) else {
                return Resp::err("hash 必须是 64 个十六进制字符");
            };
            match state.directory.lookup(h) {
                Ok((alive, ago)) => Resp::ok(serde_json::json!({"alive": alive, "ago_secs": ago})),
                Err(err) => e(err),
            }
        }

        Req::Stats => Resp::ok(serde_json::json!({
            "accepted": Metrics::get(&state.metrics.accepted),
            "paired": Metrics::get(&state.metrics.paired),
            "active_pairs": Metrics::get(&state.metrics.active_pairs),
            "active_conns": Metrics::get(&state.metrics.active_conns),
            "rejected_silent": Metrics::get(&state.metrics.rejected_silent),
            "lobby_timeout": Metrics::get(&state.metrics.lobby_timeout),
            "bytes_in": Metrics::get(&state.metrics.bytes_in),
            "bytes_out": Metrics::get(&state.metrics.bytes_out),
            "offline_depth": state.offline.depth(),
            "offline_stored": Metrics::get(&state.metrics.offline_stored),
            "group_broadcasts": Metrics::get(&state.metrics.group_broadcasts),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_responses_carry_a_machine_code() {
        let v = serde_json::to_value(e(L4Error::NotFound)).expect("serialisable");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error_code"], "not_found");
        assert!(
            v.get("error").is_some(),
            "机器码之外还要保留人读文案，二者不能互相取代"
        );
    }

    #[test]
    fn not_a_member_has_its_own_code() {
        let v = serde_json::to_value(e(L4Error::NotAMember)).expect("serialisable");
        assert_eq!(v["error_code"], "not_a_member");
        assert_ne!(
            v["error_code"], "not_found",
            "非成员不能和「群不存在」用同一个码，否则成了探测群存在性的旁路"
        );
    }

    #[test]
    fn successful_responses_omit_the_error_fields() {
        let v = serde_json::to_value(Resp::ok(serde_json::json!({"x": 1}))).expect("ok");
        assert_eq!(v["ok"], true);
        assert!(v.get("error_code").is_none());
        assert!(v.get("error").is_none());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let good = r#"{"op":"auth","token":"x"}"#;
        let bad = r#"{"op":"auth","token":"x","extra":1}"#;
        assert!(serde_json::from_str::<Req>(good).is_ok());
        assert!(
            serde_json::from_str::<Req>(bad).is_err(),
            "deny_unknown_fields 必须真的生效：多一个字段就整条拒绝"
        );
    }

    #[test]
    fn ct_eq_matches_normal_equality() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
    }

    #[tokio::test]
    async fn read_line_limited_rejects_overlong_line() {
        let data = vec![b'a'; 64];
        let mut r = BufReader::new(std::io::Cursor::new(data));
        let mut out = Vec::new();
        let e = read_line_limited(&mut r, 16, &mut out).await;
        assert!(e.is_err(), "超长行必须报错而不是无限缓冲");
    }

    #[tokio::test]
    async fn read_line_limited_reads_line() {
        let data = b"hello\nworld\n".to_vec();
        let mut r = BufReader::new(std::io::Cursor::new(data));
        let mut out = Vec::new();
        let n = read_line_limited(&mut r, 64, &mut out)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 6);
        assert_eq!(out, b"hello\n");
    }
}
