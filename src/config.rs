// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 配置 —— 对应规范附录 B.2 的服务器三档。
//!
//! 三档的差别只在「存多少、等多久、放多严」，协议本身不变。
//! 数据面永远是纯中继，服务器永远不解密。
//!
//! ## 隐私默认
//!
//! 规范第 979 行【强制】：服务器运营者**不得**记录阶段一的配对信息。
//! 身份目录本质上是「某个令牌哈希何时出现过」的记录，即配对信息本身。
//! 因此 `Directory.enabled` 在**所有档位**都默认关闭 —— 只有运营者显式
//! 在配置文件里打开，才构成一个已知偏离规范意愿的扩展。

use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 配置装载失败。
///
/// 以前「读不到」和「读到了但不合法」都被压成 io::ErrorKind::InvalidData，
/// 调用方无法区分这两种情况，原始错误也只能拼进字符串里、丢掉类型。
#[derive(Debug)]
pub enum ConfigError {
    /// 文件读不到（不存在、权限不足等）
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// 文件读到了，但内容不是合法配置
    Parse {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "读取配置文件 {} 失败: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "配置文件 {} 解析失败: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

/// 服务器档位（附录 B.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    Beginner,
    Advanced,
    HighAnonymity,
}

impl Profile {
    /// 给人看的档位名（附录 B.2 的 B2-a/B2-b/B2-c）。
    ///
    /// 序列化名只有 serde 的 kebab-case 一处真值，这里不再写第二份；
    /// 由 profile_serialized_names_are_pinned 把它钉住。
    pub fn display_name(&self) -> &'static str {
        match self {
            Profile::Beginner => "B2-a 入门公益",
            Profile::Advanced => "B2-b 高级生产",
            Profile::HighAnonymity => "B2-c 高匿名",
        }
    }
}

/// 离线消息队列（L4 扩展）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OfflineConfig {
    pub enabled: bool,
    pub max_payload: usize,
    pub max_per_recipient: usize,
    /// 全局收件人条目上限，防止无鉴权控制面把 HashMap 撑爆
    pub max_recipients: usize,
    pub ttl_secs: u64,
}

/// 群组扇出（L4 扩展）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupConfig {
    pub enabled: bool,
    pub max_members: usize,
    /// 全局群组数量上限
    pub max_groups: usize,
    pub max_payload: usize,
}

/// 身份目录（L4 扩展）。**默认关闭**，见模块文档。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryConfig {
    pub enabled: bool,
    /// 记录保留时长（秒）
    pub ttl_secs: u64,
}

/// 全局配置。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub listen: SocketAddr,
    /// 控制面监听地址；None = 关闭。默认关闭。
    pub control_listen: Option<SocketAddr>,
    pub profile: Profile,
    pub handshake_timeout_secs: u64,
    pub lobby_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub max_connections: usize,
    pub max_per_ip: usize,
    pub buffer_bytes: usize,
    pub offline: OfflineConfig,
    pub group: GroupConfig,
    pub directory: DirectoryConfig,
    /// 控制面单行读取超时。
    pub control_read_timeout_secs: u64,
    /// 控制面写响应超时。
    pub control_write_timeout_secs: u64,
    /// 中继单向写超时。
    pub relay_write_timeout_secs: u64,
    /// accept 出错后的退避，避免瞬时错误变成满速重试。
    pub accept_backoff_ms: u64,
    /// 等候池清扫周期。取比 lobby 超时更小的值，超时才能及时释放。
    pub sweep_lobby_secs: u64,
    /// 存储清扫周期：按各存储自己的 TTL 判过期，这里只决定多久看一次。
    pub sweep_storage_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self::for_profile(Profile::Beginner)
    }
}

impl Config {
    /// 按档位给出一套默认值。
    pub fn for_profile(p: Profile) -> Self {
        let (max_conn, per_ip, idle, offline, group) = match p {
            Profile::Beginner => (
                2_000,
                32,
                0,
                OfflineConfig {
                    enabled: true,
                    max_payload: 64 * 1024,
                    max_per_recipient: 20,
                    max_recipients: 10_000,
                    ttl_secs: 86_400,
                },
                GroupConfig {
                    enabled: true,
                    max_members: 32,
                    max_groups: 10_000,
                    max_payload: 64 * 1024,
                },
            ),
            Profile::Advanced => (
                20_000,
                128,
                0,
                OfflineConfig {
                    enabled: true,
                    max_payload: 256 * 1024,
                    max_per_recipient: 100,
                    max_recipients: 100_000,
                    ttl_secs: 604_800,
                },
                GroupConfig {
                    enabled: true,
                    max_members: 256,
                    max_groups: 50_000,
                    max_payload: 256 * 1024,
                },
            ),
            Profile::HighAnonymity => (
                500,
                4,
                300,
                OfflineConfig {
                    enabled: false,
                    max_payload: 16 * 1024,
                    max_per_recipient: 0,
                    max_recipients: 0,
                    ttl_secs: 0,
                },
                GroupConfig {
                    enabled: false,
                    max_members: 8,
                    max_groups: 0,
                    max_payload: 16 * 1024,
                },
            ),
        };

        Self {
            listen: "0.0.0.0:7443".parse().expect("默认监听地址合法"),
            // 控制面默认关闭：未显式 --control 不监听任何管理端口
            control_listen: None,
            profile: p,
            handshake_timeout_secs: 15,
            lobby_timeout_secs: 30,
            idle_timeout_secs: idle,
            max_connections: max_conn,
            max_per_ip: per_ip,
            buffer_bytes: 32 * 1024,
            offline,
            group,
            // 所有档位默认关闭：见模块文档
            control_read_timeout_secs: 30,
            control_write_timeout_secs: 30,
            relay_write_timeout_secs: 30,
            accept_backoff_ms: 50,
            sweep_lobby_secs: 5,
            sweep_storage_secs: 60,
            directory: DirectoryConfig {
                enabled: false,
                ttl_secs: 3_600,
            },
        }
    }

    /// 启动前校验：把「运行时静默改写成合法值」换成「启动时拒绝」。
    pub fn validate(&self) -> Result<(), String> {
        // 对所有连接生效的项
        if self.handshake_timeout_secs == 0 {
            return Err("handshake_timeout_secs must be greater than zero".into());
        }
        if self.lobby_timeout_secs == 0 {
            return Err("lobby_timeout_secs must be greater than zero".into());
        }
        if self.max_connections == 0 {
            return Err("max_connections must be greater than zero".into());
        }
        if self.buffer_bytes == 0 {
            return Err("buffer_bytes must be greater than zero".into());
        }
        for (name, v) in [
            ("control_read_timeout_secs", self.control_read_timeout_secs),
            (
                "control_write_timeout_secs",
                self.control_write_timeout_secs,
            ),
            ("relay_write_timeout_secs", self.relay_write_timeout_secs),
            ("accept_backoff_ms", self.accept_backoff_ms),
            ("sweep_lobby_secs", self.sweep_lobby_secs),
            ("sweep_storage_secs", self.sweep_storage_secs),
        ] {
            if v == 0 {
                return Err(format!("{name} must be greater than zero"));
            }
        }

        // 条件生效的项：功能关着时，它自己的参数取 0 是正常的
        // （例如高匿名档下 offline 是关的，ttl_secs 本来就是 0）。
        if self.offline.enabled {
            if self.offline.ttl_secs == 0 {
                return Err(
                    "offline.ttl_secs must be greater than zero when offline is enabled".into(),
                );
            }
            if self.offline.max_per_recipient == 0 {
                return Err(
                    "offline.max_per_recipient must be greater than zero when offline is enabled"
                        .into(),
                );
            }
            if self.offline.max_recipients == 0 {
                return Err(
                    "offline.max_recipients must be greater than zero when offline is enabled"
                        .into(),
                );
            }
        }
        if self.group.enabled {
            for (name, v) in [
                ("group.max_groups", self.group.max_groups),
                ("group.max_members", self.group.max_members),
                ("group.max_payload", self.group.max_payload),
            ] {
                if v == 0 {
                    return Err(format!(
                        "{name} must be greater than zero when group is enabled"
                    ));
                }
            }
        }
        if self.directory.enabled && self.directory.ttl_secs == 0 {
            return Err(
                "directory.ttl_secs must be greater than zero when directory is enabled".into(),
            );
        }
        Ok(())
    }

    /// 从 JSON 文件读取。
    ///
    /// 文件不存在时**返回错误**，不静默降级 —— 否则 `--config` 路径写错会让
    /// 高匿名档静默变成 beginner 档（离线存储开、目录开）。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 零值必须让进程启动失败，而不是在运行期被静默改写成 1。
    #[test]
    fn validate_rejects_zero_durations() {
        let mut c = Config::for_profile(Profile::Beginner);
        c.handshake_timeout_secs = 0;
        assert!(c.validate().is_err(), "零超时必须被拒绝");
    }

    #[test]
    fn validate_rejects_zero_limits() {
        let mut c = Config::for_profile(Profile::Beginner);
        c.max_connections = 0;
        assert!(c.validate().is_err(), "零连接上限必须被拒绝");
    }

    /// 档位的序列化名只有 serde 一处真值，这个测试把它钉住。
    #[test]
    fn profile_serialized_names_are_pinned() {
        let cases = [
            (Profile::Beginner, "\"beginner\""),
            (Profile::Advanced, "\"advanced\""),
            (Profile::HighAnonymity, "\"high-anonymity\""),
        ];
        for (p, want) in cases {
            assert_eq!(serde_json::to_string(&p).unwrap(), want);
        }
    }

    #[test]
    fn validate_accepts_every_shipped_profile() {
        for p in [Profile::Beginner, Profile::Advanced, Profile::HighAnonymity] {
            Config::for_profile(p)
                .validate()
                .unwrap_or_else(|e| panic!("{p:?} 配置应可启动: {e}"));
        }
    }

    #[test]
    fn directory_is_off_in_every_profile() {
        for p in [Profile::Beginner, Profile::Advanced, Profile::HighAnonymity] {
            assert!(
                !Config::for_profile(p).directory.enabled,
                "身份目录必须在所有档位默认关闭（规范第 979 行）"
            );
        }
    }

    #[test]
    fn control_plane_is_off_by_default() {
        for p in [Profile::Beginner, Profile::Advanced, Profile::HighAnonymity] {
            assert!(
                Config::for_profile(p).control_listen.is_none(),
                "控制面必须默认关闭"
            );
        }
    }

    #[test]
    fn high_anonymity_disables_storage_features() {
        let c = Config::for_profile(Profile::HighAnonymity);
        assert!(!c.offline.enabled);
        assert!(!c.group.enabled);
        assert!(!c.directory.enabled);
        assert!(c.idle_timeout_secs > 0);
    }

    #[test]
    fn beginner_enables_relay_storage_but_not_directory() {
        let c = Config::for_profile(Profile::Beginner);
        assert!(c.offline.enabled && c.group.enabled);
        assert!(!c.directory.enabled);
    }

    #[test]
    fn missing_config_file_is_an_error_not_a_fallback() {
        let r = Config::load(Path::new("/nonexistent/zt.toml"));
        assert!(r.is_err(), "配置文件不存在必须报错退出，不能静默降级");
    }
}
