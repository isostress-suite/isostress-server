// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 ZT-DSP Suite contributors

//! 端到端集成测试：真起一个服务端进程，两客户端完成阶段一配对并互发字节。
//!
//! 覆盖评审指出的「tests/e2e.py 不在 cargo test 里」的问题 ——
//! 这些场景现在随 `cargo test` 一起跑，不再靠手动调用。

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PARAM: &str = r#"{"aead":"AES-256-GCM","hash":"SHA3-256","key_len":256,"module_flags":{},"random_len":256,"random_rate":1024,"window":60}"#;

/// 本进程已经分配出去的端口。
///
/// `free_port()` 绑定 0 号端口、记下端口号、再把监听器丢掉——端口随即回到系统
/// 手里，同一进程里另一个测试可能拿到同一个端口。于是两个服务端抢同一个地址：
/// 后绑的那个以 EADDRINUSE 退出，先绑的那个测试却连到了别人的服务端，
/// 或者干脆连不上。
///
/// 这就是这个仓库 CI 偶发变红的来源：两个测试同时申请端口，拿到同一个，
/// 一个测试的连接被拒，另一个测试的断言落在别人的服务端上。
static ALLOCATED: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

fn free_port() -> u16 {
    loop {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let p = l.local_addr().expect("local_addr").port();
        drop(l);
        let mut used = ALLOCATED.lock().unwrap_or_else(|e| e.into_inner());
        if used.contains(&p) {
            // 已被本进程的另一个测试拿去，换一个。
            continue;
        }
        used.push(p);
        return p;
    }
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
    let child = Command::new(env!("CARGO_BIN_EXE_zt-dsp-server"))
        .args(["--listen", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut srv = Server { child };

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if connect_retry_interrupted(port).is_ok() {
            return srv;
        }
        // 服务端若已退出（例如端口被占导致绑定失败），错误信息里要看得出来，
        // 而不是让人对着「10 秒内未监听」猜。
        if let Some(status) = srv.child.try_wait().expect("try_wait") {
            panic!("服务端在监听 {port} 之前就退出了：{status:?}（端口可能已被占用）");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("服务端 10 秒内未监听端口 {port}");
}

/// 连一次，EINTR 就重试。
///
/// 子进程退出时内核会给父进程发 SIGCHLD。那个信号会打断阻塞中的套接字调用，
/// 而 Rust 的标准库不替我们重试，错误原样返回 EINTR。测试里几个用例并发地起
/// 服务、杀服务，于是这种打断时有时无地出现：一次 `connect` 报 EINTR，或一次
/// `read` 报 EINTR，测试就红了，与被测的服务端毫无关系。
fn connect_retry_interrupted(port: u16) -> std::io::Result<TcpStream> {
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// 读一次，EINTR 就重试。见 [`connect_retry_interrupted`]。
fn read_retry_interrupted(s: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match s.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
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

    let mut a = connect_retry_interrupted(port).expect("connect A");
    a.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    a.write_all(&fa).expect("A send phase1");
    a.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let mut b = connect_retry_interrupted(port).expect("connect B");
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

    let mut s = connect_retry_interrupted(port).expect("connect");
    s.set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    // param_len 远超 4096
    s.write_all(&999_999u32.to_be_bytes()).unwrap();
    s.flush().unwrap();

    let mut buf = [0u8; 64];
    match read_retry_interrupted(&mut s, &mut buf) {
        Ok(0) => {} // 对端关闭，可接受
        Ok(n) => panic!("静默丢弃失败：服务器回了 {n} 字节"),
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
        Err(e) => panic!("意外错误: {e}"),
    }
}

#[test]
fn refuses_to_start_control_plane_without_token() {
    let port = free_port();
    let out = Command::new(env!("CARGO_BIN_EXE_zt-dsp-server"))
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

fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = connect_retry_interrupted(port) {
            return s;
        }
        assert!(Instant::now() < deadline, "端口 {port} 在 10 秒内未就绪");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_line(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match read_retry_interrupted(s, &mut b) {
            Ok(0) => panic!("读行途中连接被关闭"),
            Ok(_) => {
                if b[0] == b'\n' {
                    return String::from_utf8(buf).expect("响应必须是 UTF-8");
                }
                buf.push(b[0]);
            }
            Err(err) => panic!("读行失败: {err}"),
        }
    }
}

fn auth(s: &mut TcpStream, token: &str) -> bool {
    let line = format!("{{\"op\":\"auth\",\"token\":\"{token}\"}}\n");
    s.write_all(line.as_bytes()).expect("写鉴权请求");
    s.flush().expect("flush");
    read_line(s).contains("\"authed\":true")
}

/// 失败预算按「连续失败」计，而不是按连接生命周期累计。
///
/// 此前成功鉴权不清零：先试错四次、再成功登录的脚本，只要再手误一次就累计到
/// 5 次并被断开。这里走 4 次错误 → 1 次成功 → 1 次错误 → 1 次成功，
/// 第二次成功必须仍然可达。
#[test]
fn a_successful_auth_resets_the_failure_budget() {
    const TOKEN: &str = "integration-test-token";
    let port = free_port();
    let cport = free_port();

    let child = Command::new(env!("CARGO_BIN_EXE_zt-dsp-server"))
        .args([
            "--listen",
            &format!("127.0.0.1:{port}"),
            "--control",
            &format!("127.0.0.1:{cport}"),
            "--control-token",
            TOKEN,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _srv = Server { child };

    let mut c = connect_with_retry(cport);
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    for attempt in 1..=4 {
        assert!(!auth(&mut c, "wrong"), "第 {attempt} 次错误口令应被拒");
    }
    assert!(auth(&mut c, TOKEN), "正确口令应通过");
    assert!(!auth(&mut c, "wrong"), "成功后再错一次应被拒");
    assert!(
        auth(&mut c, TOKEN),
        "成功鉴权必须把失败预算清零；否则这里已累计 5 次失败并被断开"
    );
}

/// 控制面的读超时必须来自配置，而不是写死的常量。
///
/// `Config` 里的 `control_read_timeout_secs` 此前没有任何代码读取，控制面用的是
/// 源码里的 30 秒常量；`control_write_timeout_secs` 与 `accept_backoff_ms` 同样是死配置。
/// 这里用一份把读超时设为 1 秒的配置起服务端，连上后什么都不发：若超时仍写死 30 秒，
/// 连接不会在 5 秒内被关闭，测试就会失败。
#[test]
fn control_plane_honours_the_configured_read_timeout() {
    let port = free_port();
    let cport = free_port();

    let cfg = format!(
        r#"{{
  "listen": "127.0.0.1:{port}",
  "control_listen": "127.0.0.1:{cport}",
  "profile": "beginner",
  "handshake_timeout_secs": 15,
  "lobby_timeout_secs": 30,
  "idle_timeout_secs": 0,
  "max_connections": 2000,
  "max_per_ip": 32,
  "buffer_bytes": 32768,
  "offline": {{ "enabled": true, "max_payload": 65536, "max_per_recipient": 20, "max_recipients": 10000, "ttl_secs": 86400 }},
  "group": {{ "enabled": true, "max_members": 32, "max_groups": 10000, "max_payload": 65536 }},
  "directory": {{ "enabled": false, "ttl_secs": 3600 }},
  "control_read_timeout_secs": 1,
  "control_write_timeout_secs": 30,
  "relay_write_timeout_secs": 30,
  "accept_backoff_ms": 50,
  "sweep_lobby_secs": 5,
  "sweep_storage_secs": 60
}}"#
    );

    let cfg_path = std::env::temp_dir().join(format!("zt-dsp-cfg-{cport}.json"));
    std::fs::write(&cfg_path, cfg).expect("写配置文件");

    let child = Command::new(env!("CARGO_BIN_EXE_zt-dsp-server"))
        .args([
            "--config",
            cfg_path.to_str().expect("路径是 UTF-8"),
            "--control-token",
            "integration-test-token",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _srv = Server { child };

    let mut c = connect_with_retry(cport);
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // 什么都不发。服务端应在配置的 1 秒读超时后关闭这条连接。
    let start = Instant::now();
    let mut buf = [0u8; 1];
    let closed = matches!(c.read(&mut buf), Ok(0));
    let elapsed = start.elapsed();

    let _ = std::fs::remove_file(&cfg_path);

    assert!(
        closed,
        "控制面应在配置的读超时后关闭空闲连接，而不是一直挂着"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "读超时必须来自配置（1 秒），实际等待了 {elapsed:?}"
    );
}

/// 空口令不是「口令太弱」，而是「没有口令」：`ct_eq` 对两个空串返回真，
/// 于是任何客户端都能用 `{"op":"auth","token":""}` 通过鉴权，控制面等于不设防。
/// 命令行与 `ZT_DSP_CONTROL_TOKEN` 两条入口都必须拒绝。
#[test]
fn refuses_to_start_control_plane_with_an_empty_token() {
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");

    for source in ["flag", "env"] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zt-dsp-server"));
        cmd.args(["--listen", &listen, "--control", "127.0.0.1:0"]);
        match source {
            "flag" => {
                cmd.args(["--control-token", ""]);
            }
            _ => {
                cmd.env("ZT_DSP_CONTROL_TOKEN", "");
            }
        }
        let out = cmd.output().expect("run server");
        assert!(
            !out.status.success(),
            "来自 {source} 的空口令必须让进程启动失败，实际却启动了"
        );
    }
}
