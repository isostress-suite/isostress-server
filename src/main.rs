// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! isostress 服务端：登记 → 配对 → 中继，全程不解密。
//!
//! 每个协议面一个模块：附录 F 的帧解析、等候池配对、静默丢弃、字节中继、
//! 附录 B.2 三档配置、L4 存储、控制面各自分离；模块边界与可见性和合并前一致。

/// 版本契约（单一事实来源）。
///
/// 与 `isostress_py/version.py`、`isostress_rs/version.rs` 保持同一套声明形状，
/// 差别只在角色前缀：客户端是语言前缀（PY/RS），服务端是角色前缀（SV）。
///
/// 握手帧**不携带任何版本串**：阶段一参数 JSON 与阶段四 AAD 都不含版本或协议标识字段。
mod version;

/// 取互斥量守卫，中毒时取回内值。
///
/// 持锁区间内没有 unwrap、没有用户输入处理、没有分配失败传播，中毒只可能来自
/// 「持锁任务被取消」。这种情形下计数器与映射仍自洽，恢复优于让连接崩掉 ——
/// 静默丢弃契约本来就要求异常对端无感。
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

mod util;

mod metrics;

mod silent;

mod frame;

mod lobby;

mod relay;

mod config;

mod l4;

mod control;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches, Parser, ValueEnum};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Instant};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, Profile};
use crate::control::ControlState;
use crate::frame::{read_phase1, Phase1Frame};
use crate::l4::{Directory, Groups, OfflineStore};
use crate::lobby::{Handoff, Lobby, Outcome};
use crate::metrics::Metrics;
use crate::relay::{relay_pair, RelayOpts};

/// 同时挂起等待的「被拒连接」上限。名额耗尽后直接关闭连接，
/// 避免洪泛时后台任务数无界增长。
const MAX_PENDING_DRAINS: usize = 256;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ProfileArg {
    Beginner,
    Advanced,
    HighAnonymity,
}

impl From<ProfileArg> for Profile {
    fn from(p: ProfileArg) -> Self {
        match p {
            ProfileArg::Beginner => Profile::Beginner,
            ProfileArg::Advanced => Profile::Advanced,
            ProfileArg::HighAnonymity => Profile::HighAnonymity,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "isostress-server",
    version = version::IMPLEMENTATION_ID,
    about = "isostress relay server"
)]
struct Args {
    #[arg(long)]
    listen: Option<SocketAddr>,
    #[arg(long)]
    control: Option<SocketAddr>,
    #[arg(long, value_enum, default_value = "beginner")]
    profile: ProfileArg,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, env = "ISOSTRESS_CONTROL_TOKEN")]
    control_token: Option<String>,
    #[arg(long)]
    dump_config: bool,
}

struct Shared {
    cfg: Config,
    lobby: Lobby<TcpStream>,
    metrics: Arc<Metrics>,
    offline: Arc<OfflineStore>,
    directory: Arc<Directory>,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    /// 限制「拒绝后挂起」这一类后台任务的数量：没有它，洪泛时每个被拒连接
    /// 都会 spawn 一个 15 秒的 drain 任务，并发任务数不受 max_connections 约束。
    drain_limit: Arc<tokio::sync::Semaphore>,
}

struct ConnGuard(Arc<Metrics>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.active_conns.fetch_sub(1, Ordering::Relaxed);
    }
}

struct IpGuard<'a> {
    map: &'a Mutex<HashMap<IpAddr, usize>>,
    ip: IpAddr,
}

impl Drop for IpGuard<'_> {
    fn drop(&mut self) {
        let mut g = crate::lock(self.map);
        if let Some(n) = g.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.remove(&self.ip);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // clap default --version prints only the implementation id, while the
    // startup banner prints the composed form, which also carries the
    // document revision. Hand clap the same composed string so the two
    // cannot disagree, and keep format_version() the single source of it.
    // The clap "string" feature lets Command own the value, so no leaked
    // allocation is needed to fake a 'static lifetime.
    let cmd = Args::command().version(version::format_version());
    let args = Args::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());

    let mut cfg = match &args.config {
        Some(p) => Config::load(p)?,
        None => Config::for_profile(args.profile.into()),
    };
    if let Some(l) = args.listen {
        cfg.listen = l;
    }
    if let Some(c) = args.control {
        cfg.control_listen = Some(c);
    }

    if args.dump_config {
        println!("{}", serde_json::to_string_pretty(&cfg)?);
        return Ok(());
    }

    // 空口令等同于没给。ct_eq 对两个空串返回真，所以空的共享密钥会让任何
    // 客户端用 {"op":"auth","token":""} 通过鉴权 —— 控制面等于完全不设防。
    // 「没给」与「给了个空的」在这里一并拒绝。
    if cfg.control_listen.is_some() {
        match args.control_token.as_deref() {
            None => return Err("control plane requires --control-token".into()),
            Some(t) if t.trim().is_empty() => {
                return Err("control plane token must not be empty".into())
            }
            Some(_) => {}
        }
    }

    eprintln!("isostress · {}", version::format_version());
    debug!(
        role = version::ROLE,
        prefix = version::ROLE_PREFIX,
        code = version::CODE_VERSION,
        release = version::IMPLEMENTATION_ID,
        build = version::BUILD_ID,
        doc = version::FOLLOWS_DOC,
        doc_path = version::FOLLOWS_DOC_PATH,
        applies_to = ?version::APPLIES_TO_DOC_VERSIONS,
        clients = ?version::CLIENT_VERSIONS,
        servers = ?version::SERVER_VERSIONS,
        "version contract"
    );
    info!(
        profile = cfg.profile.display_name(),
        listen = %cfg.listen,
        control = ?cfg.control_listen,
        directory = cfg.directory.enabled,
        "starting isostress relay (no decryption)"
    );
    if cfg.directory.enabled {
        warn!(
            "identity directory enabled: this is pairing metadata, which the specification forbids"
        );
    }

    let metrics = Arc::new(Metrics::default());
    let offline = Arc::new(OfflineStore::new(cfg.offline.clone()));
    let groups = Arc::new(Groups::new(cfg.group.clone()));
    let directory = Arc::new(Directory::new(cfg.directory.clone()));

    let shared = Arc::new(Shared {
        cfg: cfg.clone(),
        lobby: Lobby::new(),
        metrics: Arc::clone(&metrics),
        offline: Arc::clone(&offline),
        directory: Arc::clone(&directory),
        per_ip: Mutex::new(HashMap::new()),
        drain_limit: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_DRAINS)),
    });

    if let Some(caddr) = cfg.control_listen {
        let token = args.control_token.clone().expect("checked above");
        let state = Arc::new(ControlState {
            offline: Arc::clone(&offline),
            groups: Arc::clone(&groups),
            directory: Arc::clone(&directory),
            metrics: Arc::clone(&metrics),
            token,
        });
        tokio::spawn(async move {
            if let Err(err) = control::serve(caddr, state).await {
                warn!(error = %err, "control plane failed");
            }
        });
    }

    {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut lobby_tick =
                tokio::time::interval(Duration::from_secs(shared.cfg.sweep_lobby_secs));
            let mut offline_tick =
                tokio::time::interval(Duration::from_secs(shared.cfg.sweep_storage_secs));
            let mut dir_tick =
                tokio::time::interval(Duration::from_secs(shared.cfg.sweep_storage_secs));
            loop {
                tokio::select! {
                    _ = lobby_tick.tick() => {
                        let n = shared.lobby.sweep();
                        if n > 0 { debug!(n, "lobby sweep"); }
                    }
                    _ = offline_tick.tick() => {
                        let n = shared.offline.sweep();
                        if n > 0 { debug!(n, "offline sweep"); }
                    }
                    _ = dir_tick.tick() => {
                        let n = shared.directory.sweep();
                        if n > 0 { debug!(n, "directory sweep"); }
                    }
                }
            }
        });
    }

    // 配置不合法就不进入监听：宁可启动失败，也不要在运行期悄悄改写调用方给的值
    if let Err(reason) = cfg.validate() {
        return Err(format!("配置校验失败: {reason}").into());
    }

    let listener = TcpListener::bind(cfg.listen).await?;
    info!(addr = %cfg.listen, "data plane listening");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(err) => {
                // 不退避的话，EMFILE 之类的瞬时错误会变成满速重试
                warn!(error = %err, "accept failed, backing off");
                tokio::time::sleep(Duration::from_millis(shared.cfg.accept_backoff_ms)).await;
                continue;
            }
        };

        if shared.metrics.active_conns.load(Ordering::Relaxed) as usize
            >= shared.cfg.max_connections
        {
            Metrics::inc(&shared.metrics.rejected_silent);
            // 过载也不得直接 FIN：与其它拒绝路径保持同样的静默观感与时长。
            // 但挂起任务数必须有上限，否则洪泛时任务数会失控。
            let d = Duration::from_secs(shared.cfg.handshake_timeout_secs);
            if let Ok(permit) = Arc::clone(&shared.drain_limit).try_acquire_owned() {
                let mut s = stream;
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = silent::drain_silently(&mut s, d).await;
                });
            } else {
                // 挂起名额耗尽：宁可提前关闭，也不让任务数无界增长
                drop(stream);
            }
            continue;
        }
        shared.metrics.active_conns.fetch_add(1, Ordering::Relaxed);
        Metrics::inc(&shared.metrics.accepted);

        let shared_state = Arc::clone(&shared);
        tokio::spawn(async move {
            let _g = ConnGuard(Arc::clone(&shared_state.metrics));
            handle_conn(stream, peer, shared_state).await;
        });
    }
}

async fn handle_conn(mut stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    stream.set_nodelay(true).ok();

    let ip = peer.ip();
    let denied = {
        let limit = shared.cfg.max_per_ip;
        let mut map = crate::lock(&shared.per_ip);
        let entry = map.entry(ip).or_insert(0);
        if limit > 0 && *entry >= limit {
            true
        } else {
            *entry += 1;
            false
        }
    };
    if denied {
        Metrics::inc(&shared.metrics.rejected_silent);
        drain_like_handshake(&mut stream, &shared).await;
        return;
    }
    let ip_guard = IpGuard {
        map: &shared.per_ip,
        ip,
    };

    let deadline = Instant::now() + Duration::from_secs(shared.cfg.handshake_timeout_secs);

    let my_frame = match read_phase1(&mut stream, deadline).await {
        Ok(f) => f,
        Err(err) => {
            Metrics::inc(&shared.metrics.rejected_silent);
            debug!(peer = %peer, error = %err, "phase1 rejected");
            drain_like_handshake(&mut stream, &shared).await;
            drop(ip_guard);
            return;
        }
    };
    debug!(
        peer = %peer,
        frame_len = my_frame.raw_len(),
        aead = %my_frame.param.aead,
        "phase1 accepted"
    );
    let identity = my_frame.identity;

    match shared.lobby.register(my_frame.clone()) {
        Outcome::Matched { deliver_to_peer } => {
            let handed = deliver_to_peer.send(Handoff {
                frame: my_frame,
                payload: stream,
            });
            if handed.is_err() {
                debug!(peer = %peer, "initiator already gone");
            }
            drop(ip_guard);
        }
        Outcome::Waiting { rx, ticket } => {
            let lobby_timeout = Duration::from_secs(shared.cfg.lobby_timeout_secs);
            match timeout(lobby_timeout, rx).await {
                Ok(Ok(handoff)) => {
                    Metrics::inc(&shared.metrics.paired);
                    shared.metrics.active_pairs.fetch_add(1, Ordering::Relaxed);
                    if shared.directory.enabled() {
                        shared.directory.touch(identity.src);
                        shared.directory.touch(identity.dst);
                    }

                    let mut peer_stream = handoff.payload;
                    let phase1_replay = Duration::from_secs(shared.cfg.handshake_timeout_secs);
                    let written = async {
                        write_frame(&mut peer_stream, &my_frame, phase1_replay).await?;
                        write_frame(&mut stream, &handoff.frame, phase1_replay).await?;
                        Ok::<(), std::io::Error>(())
                    }
                    .await;

                    if written.is_err() {
                        shared.metrics.active_pairs.fetch_sub(1, Ordering::Relaxed);
                        Metrics::inc(&shared.metrics.rejected_silent);
                        drop(ip_guard);
                        return;
                    }

                    let opts = RelayOpts {
                        buffer_bytes: shared.cfg.buffer_bytes,
                        idle_timeout: if shared.cfg.idle_timeout_secs > 0 {
                            Some(Duration::from_secs(shared.cfg.idle_timeout_secs))
                        } else {
                            None
                        },
                        write_timeout: Some(Duration::from_secs(
                            shared.cfg.relay_write_timeout_secs,
                        )),
                    };
                    let (ab, ba, cause) =
                        relay_pair(stream, peer_stream, opts, &shared.metrics).await;
                    debug!(peer = %peer, ab, ba, ?cause, "relay finished");
                    shared.metrics.active_pairs.fetch_sub(1, Ordering::Relaxed);
                    drop(ip_guard);
                }
                _ => {
                    shared.lobby.withdraw(ticket);
                    Metrics::inc(&shared.metrics.lobby_timeout);
                    debug!(peer = %peer, "lobby timeout");
                    drain_like_handshake(&mut stream, &shared).await;
                    drop(ip_guard);
                }
            }
        }
    }
}

async fn drain_like_handshake(stream: &mut TcpStream, shared: &Shared) {
    let _ = silent::drain_silently(
        stream,
        Duration::from_secs(shared.cfg.handshake_timeout_secs),
    )
    .await;
}

async fn write_frame(s: &mut TcpStream, f: &Phase1Frame, timeout: Duration) -> std::io::Result<()> {
    tokio::time::timeout(timeout, async {
        s.write_all(&f.raw).await?;
        s.flush().await
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "phase1 replay timed out"))?
}

#[cfg(test)]
mod admission_tests;
