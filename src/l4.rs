// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! L4 扩展：离线消息 / 群组 / 身份目录。
//!
//! 这三项**不在 isostress 规范内**。它们走独立控制面（JSON Lines over TCP），
//! 因为附录 F 的线上帧没有帧类型字段，往数据面插帧会破坏客户端互操作。
//!
//! payload 一律是不透明字节：服务器不解密、不解析。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{DirectoryConfig, GroupConfig, OfflineConfig};
use crate::frame::TokenHash;

/// L4 操作错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4Error {
    Disabled,
    PayloadTooLarge { max: usize },
    NotFound,
    NotAMember,
    AlreadyExists,
    GroupFull { max: usize },
    GroupLimit { max: usize },
    RecipientLimit { max: usize },
}

impl L4Error {
    /// 稳定的机器可读码。文案可改、可本地化，**码不可改**：机器客户端只认它。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::PayloadTooLarge { .. } => "payload_too_large",
            Self::NotFound => "not_found",
            Self::NotAMember => "not_a_member",
            Self::AlreadyExists => "already_exists",
            Self::GroupFull { .. } => "group_full",
            Self::GroupLimit { .. } => "group_limit",
            Self::RecipientLimit { .. } => "recipient_limit",
        }
    }
}

impl std::error::Error for L4Error {}

impl std::fmt::Display for L4Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            L4Error::Disabled => write!(f, "该功能在当前档位被关闭"),
            L4Error::PayloadTooLarge { max } => write!(f, "载荷超过上限 {max} 字节"),
            L4Error::NotFound => write!(f, "未找到"),
            L4Error::NotAMember => write!(f, "不是该群成员"),
            L4Error::AlreadyExists => write!(f, "已存在"),
            L4Error::GroupFull { max } => write!(f, "群成员已达上限 {max}"),
            L4Error::GroupLimit { max } => write!(f, "群组数量已达上限 {max}"),
            L4Error::RecipientLimit { max } => write!(f, "离线收件人数量已达上限 {max}"),
        }
    }
}

// ───────────────────────── 离线消息 ─────────────────────────

struct Stored {
    payload: Vec<u8>,
    stored_at: Instant,
}

/// 离线消息仓库：按收件人令牌哈希排队，服务器看不见内容。
pub struct OfflineStore {
    cfg: OfflineConfig,
    inner: Mutex<HashMap<TokenHash, VecDeque<Stored>>>,
}

/// 载荷上限校验。离线信封与群组日志用同一把尺子。
fn check_payload_len(len: usize, max: usize) -> Result<(), L4Error> {
    if len > max {
        return Err(L4Error::PayloadTooLarge { max });
    }
    Ok(())
}

impl OfflineStore {
    pub fn new(cfg: OfflineConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 投递一条离线消息，返回该收件人当前积压条数。
    pub fn push(&self, dst: TokenHash, payload: Vec<u8>) -> Result<usize, L4Error> {
        if !self.cfg.enabled || self.cfg.max_per_recipient == 0 {
            return Err(L4Error::Disabled);
        }
        check_payload_len(payload.len(), self.cfg.max_payload)?;
        let mut guard = crate::lock(&self.inner);

        if !guard.contains_key(&dst) && guard.len() >= self.cfg.max_recipients {
            return Err(L4Error::RecipientLimit {
                max: self.cfg.max_recipients,
            });
        }

        let q = guard.entry(dst).or_default();
        if q.len() >= self.cfg.max_per_recipient {
            q.pop_front();
        }
        q.push_back(Stored {
            payload,
            stored_at: Instant::now(),
        });
        Ok(q.len())
    }

    /// 取走某收件人的至多 max 条消息。
    pub fn pull(&self, dst: TokenHash, max: usize) -> Result<Vec<Vec<u8>>, L4Error> {
        if !self.cfg.enabled {
            return Err(L4Error::Disabled);
        }
        let mut guard = crate::lock(&self.inner);
        let Some(q) = guard.get_mut(&dst) else {
            return Ok(Vec::new());
        };
        let take = q.len().min(max);
        let mut out = Vec::with_capacity(take);
        for _ in 0..take {
            if let Some(item) = q.pop_front() {
                out.push(item.payload);
            }
        }
        if q.is_empty() {
            guard.remove(&dst);
        }
        Ok(out)
    }

    /// 清理过期消息，返回清理条数。
    pub fn sweep(&self) -> usize {
        if !self.cfg.enabled {
            return 0;
        }
        let ttl = Duration::from_secs(self.cfg.ttl_secs);
        let mut guard = crate::lock(&self.inner);
        let mut removed = 0usize;
        guard.retain(|_, q| {
            let before = q.len();
            q.retain(|s| s.stored_at.elapsed() < ttl);
            removed += before - q.len();
            !q.is_empty()
        });
        removed
    }

    pub fn depth(&self) -> usize {
        crate::lock(&self.inner).values().map(|q| q.len()).sum()
    }
}

// ───────────────────────── 群组 ─────────────────────────

struct Broadcast {
    payload: Vec<u8>,
    cursor: u64,
}

struct GroupState {
    members: HashSet<TokenHash>,
    log: VecDeque<Broadcast>,
    next_cursor: u64,
    cursors: HashMap<TokenHash, u64>,
}

impl GroupState {
    fn new() -> Self {
        Self {
            members: HashSet::new(),
            log: VecDeque::new(),
            next_cursor: 1,
            cursors: HashMap::new(),
        }
    }
}

/// 群组：成员名单 + 追加日志，成员按游标拉取。
pub struct Groups {
    cfg: GroupConfig,
    inner: Mutex<HashMap<String, GroupState>>,
}

impl Groups {
    pub fn new(cfg: GroupConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 创建群组。同名群组已存在时返回 `AlreadyExists`，不静默复用。
    pub fn create(&self, name: &str, owner: TokenHash) -> Result<(), L4Error> {
        if !self.cfg.enabled {
            return Err(L4Error::Disabled);
        }
        if name.is_empty() {
            return Err(L4Error::NotFound);
        }
        let mut guard = crate::lock(&self.inner);
        if guard.contains_key(name) {
            return Err(L4Error::AlreadyExists);
        }
        if guard.len() >= self.cfg.max_groups {
            return Err(L4Error::GroupLimit {
                max: self.cfg.max_groups,
            });
        }
        let mut g = GroupState::new();
        g.members.insert(owner);
        g.cursors.insert(owner, 0);
        guard.insert(name.to_string(), g);
        Ok(())
    }

    pub fn join(&self, name: &str, member: TokenHash) -> Result<usize, L4Error> {
        if !self.cfg.enabled {
            return Err(L4Error::Disabled);
        }
        let mut guard = crate::lock(&self.inner);
        let Some(g) = guard.get_mut(name) else {
            return Err(L4Error::NotFound);
        };
        if !g.members.contains(&member) && g.members.len() >= self.cfg.max_members {
            return Err(L4Error::GroupFull {
                max: self.cfg.max_members,
            });
        }
        g.members.insert(member);
        g.cursors.entry(member).or_insert(0);
        Ok(g.members.len())
    }

    pub fn send(&self, name: &str, sender: TokenHash, payload: Vec<u8>) -> Result<u64, L4Error> {
        if !self.cfg.enabled {
            return Err(L4Error::Disabled);
        }
        check_payload_len(payload.len(), self.cfg.max_payload)?;
        let mut guard = crate::lock(&self.inner);
        let Some(g) = guard.get_mut(name) else {
            return Err(L4Error::NotFound);
        };
        if !g.members.contains(&sender) {
            // 专用变体而不是 NotFound：报错不回显群组是否存在，
            // 免得成为探测群组存在性的旁路。
            return Err(L4Error::NotAMember);
        }
        let cursor = g.next_cursor;
        g.next_cursor += 1;
        g.log.push_back(Broadcast { payload, cursor });
        // 每名成员最多保留 64 条群日志。
        // 系数写在这里而不是散在别处：64 = 每成员保留的日志条数上限，
        // 成员数上限乘以它就是群日志的总条数上限。
        // max_members 在配置校验里必须大于 0，所以这里不再兜底。
        let cap = self.cfg.max_members.saturating_mul(64);
        while g.log.len() > cap {
            g.log.pop_front();
        }
        Ok(cursor)
    }

    pub fn pull(&self, name: &str, member: TokenHash) -> Result<Vec<(u64, Vec<u8>)>, L4Error> {
        if !self.cfg.enabled {
            return Err(L4Error::Disabled);
        }
        let mut guard = crate::lock(&self.inner);
        let Some(g) = guard.get_mut(name) else {
            return Err(L4Error::NotFound);
        };
        if !g.members.contains(&member) {
            // 同上：不回显群组是否存在。
            return Err(L4Error::NotAMember);
        }
        let since = *g.cursors.get(&member).unwrap_or(&0);
        let mut out = Vec::new();
        for b in g.log.iter() {
            if b.cursor > since {
                out.push((b.cursor, b.payload.clone()));
            }
        }
        if let Some(last) = out.last() {
            g.cursors.insert(member, last.0);
        }
        Ok(out)
    }
}

// ───────────────────────── 身份目录 ─────────────────────────

/// 身份目录：只回答「这个令牌哈希最近活跃过吗」。
///
/// 规范第 979 行禁止服务器记录配对信息，而本结构正是这样一份记录。
/// 因此它在所有档位默认关闭；只有运营者显式开启才存在。
/// 访问控制由控制面的身份校验负责，本类型不再接受调用方自行声明的开关。
pub struct Directory {
    cfg: DirectoryConfig,
    seen: Mutex<HashMap<TokenHash, Instant>>,
}

impl Directory {
    pub fn new(cfg: DirectoryConfig) -> Self {
        Self {
            cfg,
            seen: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// 记录一次活跃。
    pub fn touch(&self, hash: TokenHash) {
        if !self.cfg.enabled {
            return;
        }
        let mut guard = crate::lock(&self.seen);
        guard.insert(hash, Instant::now());
    }

    /// 查询。返回 (是否活跃, 最近活跃距今的**粗化**秒数)。
    pub fn lookup(&self, hash: TokenHash) -> Result<(bool, Option<u64>), L4Error> {
        if !self.cfg.enabled {
            return Err(L4Error::Disabled);
        }
        let guard = crate::lock(&self.seen);
        match guard.get(&hash) {
            None => Ok((false, None)),
            Some(t) => Ok((true, Some(t.elapsed().as_secs() / 60 * 60))),
        }
    }

    /// 按配置的 TTL 清理。
    pub fn sweep(&self) -> usize {
        if !self.cfg.enabled {
            return 0;
        }
        let ttl = Duration::from_secs(self.cfg.ttl_secs);
        let mut guard = crate::lock(&self.seen);
        let before = guard.len();
        guard.retain(|_, t| t.elapsed() < ttl);
        before - guard.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> TokenHash {
        TokenHash([n; 32])
    }

    fn offline_cfg() -> OfflineConfig {
        OfflineConfig {
            enabled: true,
            max_payload: 1024,
            max_per_recipient: 3,
            max_recipients: 4,
            ttl_secs: 3600,
        }
    }

    fn group_cfg() -> GroupConfig {
        GroupConfig {
            enabled: true,
            max_members: 8,
            max_groups: 4,
            max_payload: 1024,
        }
    }

    #[test]
    fn offline_fifo_eviction() {
        let s = OfflineStore::new(offline_cfg());
        for i in 0..3u8 {
            s.push(h(1), vec![i]).unwrap();
        }
        assert_eq!(s.push(h(1), vec![9]).unwrap(), 3);
        let items = s.pull(h(1), 10).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], vec![1]);
        assert_eq!(items[2], vec![9]);
    }

    #[test]
    fn offline_rejects_oversize() {
        let s = OfflineStore::new(offline_cfg());
        assert!(matches!(
            s.push(h(1), vec![0u8; 2048]),
            Err(L4Error::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn offline_recipient_cap_is_enforced() {
        let s = OfflineStore::new(offline_cfg());
        for i in 0..4u8 {
            s.push(h(i), vec![i]).unwrap();
        }
        assert!(matches!(
            s.push(h(9), vec![9]),
            Err(L4Error::RecipientLimit { max: 4 })
        ));
    }

    #[test]
    fn offline_zero_quota_means_disabled() {
        let mut c = offline_cfg();
        c.max_per_recipient = 0;
        let s = OfflineStore::new(c);
        assert!(matches!(s.push(h(1), vec![1]), Err(L4Error::Disabled)));
    }

    #[test]
    fn group_create_is_not_idempotent() {
        let g = Groups::new(group_cfg());
        g.create("room", h(1)).unwrap();
        assert!(matches!(
            g.create("room", h(1)),
            Err(L4Error::AlreadyExists)
        ));
    }

    #[test]
    fn group_cursor_advances() {
        let g = Groups::new(group_cfg());
        g.create("room", h(1)).unwrap();
        g.join("room", h(2)).unwrap();
        g.send("room", h(1), b"hello".to_vec()).unwrap();
        let got = g.pull("room", h(2)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, b"hello");
        assert!(g.pull("room", h(2)).unwrap().is_empty());
    }

    #[test]
    fn group_rejects_non_member() {
        let g = Groups::new(group_cfg());
        g.create("room", h(1)).unwrap();
        // 非成员拿到的是专用变体，不回显群组是否存在
        assert!(matches!(
            g.send("room", h(3), b"x".to_vec()),
            Err(L4Error::NotAMember)
        ));
    }

    #[test]
    fn group_count_cap_is_enforced() {
        let g = Groups::new(group_cfg());
        for i in 0..4u8 {
            g.create(&format!("g{i}"), h(1)).unwrap();
        }
        assert!(matches!(
            g.create("g9", h(1)),
            Err(L4Error::GroupLimit { max: 4 })
        ));
    }

    fn dir_cfg(enabled: bool) -> DirectoryConfig {
        DirectoryConfig {
            enabled,
            ttl_secs: 3600,
        }
    }

    #[test]
    fn directory_disabled_by_default_refuses() {
        let d = Directory::new(dir_cfg(false));
        assert!(matches!(d.lookup(h(7)), Err(L4Error::Disabled)));
    }

    #[test]
    fn directory_coarsens_time() {
        let d = Directory::new(dir_cfg(true));
        d.touch(h(7));
        let (alive, ago) = d.lookup(h(7)).unwrap();
        assert!(alive);
        // 刚写入的记录必须落在第一个整分钟桶里；这同时排除「没做粗化」的实现
        let ago = ago.expect("刚写入的记录应有时间戳");
        assert!(ago < 60, "刚写入的记录粗化后应小于 60 秒，实际 {ago}");
        assert_eq!(ago % 60, 0, "粗化结果必须是整分钟");
    }

    #[test]
    fn disabled_features_refuse() {
        let s = OfflineStore::new(OfflineConfig {
            enabled: false,
            max_payload: 1,
            max_per_recipient: 1,
            max_recipients: 1,
            ttl_secs: 1,
        });
        assert!(matches!(s.push(h(1), vec![1]), Err(L4Error::Disabled)));
    }
}
