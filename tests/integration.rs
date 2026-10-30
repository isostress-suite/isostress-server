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

/// 连上某个端口，等它真的开始监听。
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

/// 读一行控制面响应。
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

/// 控制面鉴权，返回是否通过。
fn auth(s: &mut TcpStream, token: &str) -> bool {
    let line = format!("{{\"op\":\"auth\",\"token\":\"{token}\"}}\n");
    s.write_all(line.as_bytes()).expect("写鉴权请求");
    s.flush().expect("flush");
    read_line(s).contains("\"authed\":true")
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

/// SIGTERM 必须让进程停止接受连接并正常退出，而不是像 SIGKILL 那样被直接打死。
#[cfg(unix)]
#[test]
fn exits_with_a_zero_status_on_sigterm() {
    let port = free_port();
    let mut srv = start_server(port);

    let pid = srv.child.id().to_string();
    let sent = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("发送 SIGTERM");
    assert!(sent.success(), "kill -TERM 应成功");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = srv.child.try_wait().expect("try_wait") {
            assert!(status.success(), "SIGTERM 后应正常退出，实际 {status:?}");
            return;
        }
        assert!(Instant::now() < deadline, "SIGTERM 后 10 秒内未退出");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 停机日志必须把控制面连接也算进去。
///
/// 宽限只等数据面连接：控制面连接是长连，空闲时要等读超时才关，在宽限窗口内
/// 不会自己结束，等它没有意义。但日志若只报数据面连接数，就会漏掉仍然开着的
/// 控制面连接 —— 停机时那一条信息是唯一的现场记录。
#[cfg(unix)]
#[test]
fn the_shutdown_log_counts_control_plane_connections() {
    const TOKEN: &str = "integration-test-token";
    let port = free_port();
    let cport = free_port();

    // stderr 落盘：停机那几行日志是这条测试要断言的东西。
    let err_path = std::env::temp_dir().join(format!("zt-dsp-shutdown-{cport}.log"));
    let err_file = std::fs::File::create(&err_path).expect("建 stderr 文件");
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
        .stderr(Stdio::from(err_file))
        .spawn()
        .expect("spawn server");
    let mut srv = Server { child };

    // 留一条活的控制面连接：鉴权成功后连接保持打开。
    let mut c = connect_with_retry(cport);
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert!(auth(&mut c, TOKEN), "控制面鉴权应通过");

    let pid = srv.child.id().to_string();
    Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("发送 SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if srv.child.try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "SIGTERM 后 10 秒内未退出");
        std::thread::sleep(Duration::from_millis(50));
    }

    let log = std::fs::read_to_string(&err_path).expect("读 stderr");
    let _ = std::fs::remove_file(&err_path);
    // tracing 的输出带 ANSI 转义码时，字段名与取值之间会插进转义序列
    // （`control_plane\x1b[0m\x1b[2m=\x1b[0m1`），直接按字面量匹配会落空。
    // 是否带转义码取决于运行环境有没有终端，所以先把转义序列去掉再断言。
    let plain = strip_ansi(&log);

    assert!(
        plain.contains("control_plane=1"),
        "停机日志必须报出仍开着的控制面连接数，实际日志：\n{plain}"
    );
}

/// 去掉 ANSI 转义序列。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI 序列以 '[' 开头，以一个字节参数结束（0x40–0x7E）。
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
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
