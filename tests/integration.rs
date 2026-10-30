// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 端到端集成测试：真起一个服务端进程，两客户端完成阶段一配对并互发字节。
//!
//! 覆盖评审指出的「tests/e2e.py 不在 cargo test 里」的问题 ——
//! 这些场景现在随 `cargo test` 一起跑，不再靠手动调用。

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PARAM: &str = r#"{"aead":"AES-256-GCM","hash":"SHA3-256","key_len":256,"module_flags":{},"random_len":256,"random_rate":1024,"window":60}"#;

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().expect("local_addr").port()
}

fn phase1(src: u8, dst: u8) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(PARAM.len() as u32).to_be_bytes());
    b.extend_from_slice(PARAM.as_bytes());
    b.extend_from_slice(&64u32.to_be_bytes());
    b.extend_from_slice(&[src; 32]);
    b.extend_from_slice(&[dst; 32]);
    b.extend_from_slice(&32u32.to_be_bytes());
    b.extend_from_slice(&[0xCCu8; 32]);
    b
}

struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server(port: u16) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_isostress-server"))
        .args(["--listen", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let srv = Server { child };

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return srv;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("服务端 10 秒内未监听端口 {port}");
}

fn recvn(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).expect("read_exact");
    buf
}

#[test]
fn pairs_two_peers_and_relays_bytes() {
    let port = free_port();
    let _srv = start_server(port);

    let fa = phase1(0xAA, 0xBB);
    let fb = phase1(0xBB, 0xAA);

    let mut a = TcpStream::connect(("127.0.0.1", port)).expect("connect A");
    a.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    a.write_all(&fa).expect("A send phase1");
    a.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let mut b = TcpStream::connect(("127.0.0.1", port)).expect("connect B");
    b.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    b.write_all(&fb).expect("B send phase1");
    b.flush().unwrap();

    // 配对后两边各自收到对方的阶段一帧，字节必须完全一致
    assert_eq!(recvn(&mut b, fa.len()), fa, "B 应收到 A 的原始阶段一帧");
    assert_eq!(recvn(&mut a, fb.len()), fb, "A 应收到 B 的原始阶段一帧");

    // 双向透传
    a.write_all(b"hello-from-A").unwrap();
    a.flush().unwrap();
    assert_eq!(recvn(&mut b, 12), b"hello-from-A");

    b.write_all(b"hello-from-B").unwrap();
    b.flush().unwrap();
    assert_eq!(recvn(&mut a, 12), b"hello-from-B");

    // 大块数据不得被截断或改写
    let blob: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    a.write_all(&blob).unwrap();
    a.flush().unwrap();
    assert_eq!(
        recvn(&mut b, blob.len()),
        blob,
        "100KB 大块数据必须原样透传"
    );
}

#[test]
fn rejects_malformed_phase1_without_answering() {
    let port = free_port();
    let _srv = start_server(port);

    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    // param_len 远超 4096
    s.write_all(&999_999u32.to_be_bytes()).unwrap();
    s.flush().unwrap();

    let mut buf = [0u8; 64];
    match s.read(&mut buf) {
        Ok(0) => {} // 对端关闭，可接受
        Ok(n) => panic!("静默丢弃失败：服务器回了 {n} 字节"),
        Err(e)
            if e.kind() == ErrorKind::WouldBlock
                || e.kind() == ErrorKind::TimedOut
                || e.kind() == ErrorKind::ConnectionReset
                || e.kind() == ErrorKind::ConnectionAborted => {}
        Err(e) => panic!("意外错误: {e}"),
    }
}

#[test]
fn refuses_to_start_control_plane_without_token() {
    let port = free_port();
    let out = Command::new(env!("CARGO_BIN_EXE_isostress-server"))
        .args([
            "--listen",
            &format!("127.0.0.1:{port}"),
            "--control",
            "127.0.0.1:0",
        ])
        .output()
        .expect("run server");
    assert!(!out.status.success(), "启用控制面却不给口令时必须启动失败");
}

/// 空口令不是「口令太弱」，而是「没有口令」：`ct_eq` 对两个空串返回真，
/// 于是任何客户端都能用 `{"op":"auth","token":""}` 通过鉴权，控制面等于不设防。
/// 命令行与 `ISOSTRESS_CONTROL_TOKEN` 两条入口都必须拒绝。
#[test]
fn refuses_to_start_control_plane_with_an_empty_token() {
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");

    for source in ["flag", "env"] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_isostress-server"));
        cmd.args(["--listen", &listen, "--control", "127.0.0.1:0"]);
        match source {
            "flag" => {
                cmd.args(["--control-token", ""]);
            }
            _ => {
                cmd.env("ISOSTRESS_CONTROL_TOKEN", "");
            }
        }
        let out = cmd.output().expect("run server");
        assert!(
            !out.status.success(),
            "来自 {source} 的空口令必须让进程启动失败，实际却启动了"
        );
    }
}
