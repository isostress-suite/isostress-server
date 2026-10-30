// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 严格静默丢弃（附录 F.2）—— 服务器侧的门禁行为。
//!
//! 规范要求五要素缺一不可：
//!
//! 1. 不发送任何字节
//! 2. 门禁窗口内不主动关闭连接 —— FIN / RST 本身就是「被拒」的信号，
//!    所以在窗口结束前既不写、也不关
//! 3. 以 8192B 为块持续读并丢弃，挂起到截止时间
//! 4. 诊断只进本地 stderr，绝不进网络侧
//! 5. 网络观感与「关闭的端口」不可区分
//!
//! 第 2 条与第 5 条容易读成矛盾：一条说不关，一条说要跟关掉的端口
//! 一样。它们的调和方式是**时间**：
//!
//! * 窗口**之内**，对端看到的是一个「还在、但不吭声」的连接。这跟一个
//!   关掉的端口不同（关掉的端口会立刻 RST），但对端无法区分「被拒」与
//!   「对端只是慢」，这正是要点。
//! * 窗口**结束**时，连接按普通连接收尾 —— 关闭，不带任何错误码、
//!   不带任何应用层响应。跟一个正常结束的连接相比，对端看不出差别。
//!
//! 所以「不可区分」说的是**没有可区分的拒绝痕迹**，不是「永远不关」。
//! 真的永远不关，连接就会一直挂着，那才是可区分的。
//!
//! 服务端的职责边界（总指导第 972 行）：服务器最坏只能拒转 / 延迟 / 丢包，
//! 这些都是可用性攻击，偷不到内容。所以门禁判定失败时，静默丢弃就是正确响应。

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::{timeout_at, Instant};

/// 丢弃时的读取块大小（附录 F.2 指定 8192）
pub const DRAIN_CHUNK: usize = 8192;

/// 静默丢弃：读走并扔掉对端的字节，直到截止时间或对端关闭。
///
/// 返回被丢弃的字节数（只用于本地诊断与限流，绝不回给对端）。
pub async fn drain_silently<R: AsyncRead + Unpin>(r: &mut R, duration: Duration) -> u64 {
    let deadline = Instant::now() + duration;
    let mut buf = [0u8; DRAIN_CHUNK];
    let mut drained: u64 = 0;

    loop {
        match timeout_at(deadline, r.read(&mut buf)).await {
            // 对端干净关闭
            Ok(Ok(0)) => break,
            // 继续吞
            Ok(Ok(n)) => drained = drained.saturating_add(n as u64),
            // 连接已坏（RST 等）
            Ok(Err(_)) => break,
            // 截止时间到
            Err(_) => break,
        }
    }

    drained
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn drains_without_writing() {
        let (mut a, mut b) = duplex(65536);

        // 对端猛灌 20000 字节
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let payload = vec![0x5Au8; 20_000];
            let _ = b.write_all(&payload).await;
            payload.len()
        });

        let drained = drain_silently(&mut a, Duration::from_millis(300)).await;
        let sent = writer.await.unwrap();
        assert!(drained > 0, "应该真的读走了字节");
        assert!(drained <= sent as u64 + DRAIN_CHUNK as u64);
    }

    #[tokio::test]
    async fn returns_promptly_on_peer_close() {
        let (mut a, b) = duplex(1024);
        drop(b);
        let t0 = std::time::Instant::now();
        let n = drain_silently(&mut a, Duration::from_secs(10)).await;
        assert_eq!(n, 0);
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "对端关闭应立即返回，不该干等 10 秒"
        );
    }
}
