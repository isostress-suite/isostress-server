// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 配对成功后的双向字节中继。
//!
//! 服务器只做三件事：把 A 的字节倒给 B、把 B 的字节倒给 A、计数。
//! 不解析、不改写、不解密。

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::metrics::Metrics;

/// 中继参数。
pub struct RelayOpts {
    pub buffer_bytes: usize,
    /// 单向读空闲超时；None = 不设
    pub idle_timeout: Option<Duration>,
    /// 单次写超时；防止对端只连不读把任务永久挂住。None = 不设。
    pub write_timeout: Option<Duration>,
}

/// 中继结束原因。空闲超时与正常 EOF 必须可区分，否则日志里分不清对端走了还是对端还在但一直不说话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// 对端正常关闭
    Eof,
    /// 读空闲超时
    Idle,
    /// 写超时（对端只连不读）
    WriteStalled,
    /// 其他 IO 错误
    Error,
}

/// 单向搬运。`counter` 是跨 future 共享的累计字节数。
async fn pipe<R, W>(
    r: &mut R,
    w: &mut W,
    opts: &RelayOpts,
    counter: &AtomicU64,
) -> io::Result<StopCause>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; opts.buffer_bytes];

    loop {
        let n = match opts.idle_timeout {
            None => r.read(&mut buf).await?,
            Some(d) => match tokio::time::timeout(d, r.read(&mut buf)).await {
                Err(_) => return Ok(StopCause::Idle),
                Ok(res) => res?,
            },
        };
        if n == 0 {
            return Ok(StopCause::Eof);
        }

        match opts.write_timeout {
            None => {
                w.write_all(&buf[..n]).await?;
                w.flush().await?;
            }
            Some(d) => {
                let written = tokio::time::timeout(d, async {
                    w.write_all(&buf[..n]).await?;
                    w.flush().await
                })
                .await;
                match written {
                    Err(_) => return Ok(StopCause::WriteStalled),
                    Ok(Err(e)) => return Err(e),
                    Ok(Ok(())) => {}
                }
            }
        }

        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// 双向中继一对已配对的连接。
///
/// 任一侧结束即**取消**另一侧（`select!` 丢弃未完成的 future），
/// 不等待半开连接自然超时。
pub async fn relay_pair<A, B>(
    a: A,
    b: B,
    opts: RelayOpts,
    metrics: &Metrics,
) -> (u64, u64, StopCause)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut ar, mut aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);

    let ab_bytes = AtomicU64::new(0);
    let ba_bytes = AtomicU64::new(0);

    let mut ab = Box::pin(pipe(&mut ar, &mut bw, &opts, &ab_bytes));
    let mut ba = Box::pin(pipe(&mut br, &mut aw, &opts, &ba_bytes));

    let cause = tokio::select! {
        r = &mut ab => r.unwrap_or(StopCause::Error),
        r = &mut ba => r.unwrap_or(StopCause::Error),
    };

    // 显式丢弃未完成的一侧：select! 只停止轮询，future 仍被 pin 住
    drop(ab);
    drop(ba);

    let ab_n = ab_bytes.load(Ordering::Relaxed);
    let ba_n = ba_bytes.load(Ordering::Relaxed);
    Metrics::add(&metrics.bytes_in, ab_n);
    Metrics::add(&metrics.bytes_out, ba_n);
    (ab_n, ba_n, cause)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn opts() -> RelayOpts {
        RelayOpts {
            buffer_bytes: 8192,
            idle_timeout: None,
            write_timeout: Some(Duration::from_secs(5)),
        }
    }

    async fn write_and_read(
        mut from: DuplexStream,
        mut to: DuplexStream,
        payload: &'static [u8],
    ) -> Vec<u8> {
        let w = tokio::spawn(async move {
            from.write_all(payload).await.unwrap();
            from.flush().await.unwrap();
        });
        let mut got = vec![0u8; payload.len()];
        to.read_exact(&mut got).await.unwrap();
        w.await.unwrap();
        got
    }

    #[tokio::test]
    async fn relays_both_directions() {
        let m = Metrics::default();
        let (c1, s1) = tokio::io::duplex(65536);
        let (c2, s2) = tokio::io::duplex(65536);

        let h = tokio::spawn(async move { relay_pair(s1, s2, opts(), &m).await });

        let mut c1 = c1;
        let mut c2 = c2;
        c1.write_all(b"ping").await.unwrap();
        c1.flush().await.unwrap();
        let mut got = [0u8; 4];
        c2.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");

        c2.write_all(b"pong").await.unwrap();
        c2.flush().await.unwrap();
        let mut got2 = [0u8; 4];
        c1.read_exact(&mut got2).await.unwrap();
        assert_eq!(&got2, b"pong");

        let _ = write_and_read(c1, c2, b"").await;
        let (ab, ba, _cause) = h.await.unwrap();
        assert!(ab >= 4, "A→B 至少 4 字节，实际 {ab}");
        assert!(ba >= 4, "B→A 至少 4 字节，实际 {ba}");
    }

    #[tokio::test]
    async fn one_side_closing_cancels_the_other() {
        let m = Metrics::default();
        let (c1, s1) = tokio::io::duplex(1024);
        let (c2, s2) = tokio::io::duplex(1024);
        let o = RelayOpts {
            buffer_bytes: 1024,
            idle_timeout: None,
            write_timeout: None,
        };
        let h = tokio::spawn(async move { relay_pair(s1, s2, o, &m).await });
        drop(c1);
        let t0 = std::time::Instant::now();
        let _ = h.await.unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "一侧关闭应立刻取消另一侧"
        );
        drop(c2);
    }

    #[tokio::test]
    async fn idle_timeout_closes_quiet_pair() {
        let m = Metrics::default();
        let (c1, s1) = tokio::io::duplex(1024);
        let (c2, s2) = tokio::io::duplex(1024);
        let o = RelayOpts {
            buffer_bytes: 1024,
            idle_timeout: Some(Duration::from_millis(120)),
            write_timeout: None,
        };
        let h = tokio::spawn(async move { relay_pair(s1, s2, o, &m).await });
        let t0 = std::time::Instant::now();
        let _ = h.await.unwrap();
        assert!(t0.elapsed() < Duration::from_secs(2));
        drop(c1);
        drop(c2);
    }
}
