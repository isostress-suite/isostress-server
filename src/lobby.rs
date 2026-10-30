// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

//! 等候池（规范里叫「大厅」/「匹配池」）—— 酒店的等人逻辑。
//!
//! 依据总指导 4.2 服务器处理流程与第 613 行：
//! > 查找等候池中是否有持有 H(Token_A) 的住客（即 B 已经先到，在等 A）。
//! > 如果有 → 将 A 和 B 配对，送入同一房间；如果没有 → 将 A 放入等候池，等待 B 到来。
//!
//! 配对判据是**双向匹配**，杜绝「名字撞上就接通」的身份冒用：
//!
//!    新来者.src == 已在等者.dst  且  新来者.dst == 已在等者.src
//!
//! 时限：30 秒自动清除（附录 H.3），不广播、不通知、不留痕。
//!
//! ## 连接交接
//!
//! 后到的一方（B）把「自己的阶段一帧 + 自己的连接」通过通道交给先到的一方（A），
//! 然后 B 的任务直接收工。A 醒来后同时握着两条连接，由它完成双向中继。
//! 好处：池子里从不存放 socket，也不会出现两个任务共同操作同一条连接。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::frame::{Identity, Phase1Frame, TokenHash};

/// 后到一方交给先到一方的包裹。
pub struct Handoff<T> {
    /// 后到方的阶段一帧（由接管方写入先到方的连接）
    pub frame: Phase1Frame,
    /// 后到方的连接
    pub payload: T,
}

struct Entry<T> {
    id: u64,
    identity: Identity,
    deliver: oneshot::Sender<Handoff<T>>,
}

/// 等候池。
pub struct Lobby<T> {
    /// key = 在等的人要找的目标令牌哈希（即 entry.identity.dst）
    waiters: Mutex<HashMap<TokenHash, Vec<Entry<T>>>>,
    next_id: AtomicU64,
}

/// 登记结果。
pub enum Outcome<T> {
    /// 配上了。你是后到的一方：把 `Handoff{ frame: 你的帧, payload: 你的连接 }`
    /// 发进 `deliver_to_peer`，你的任务即可收工。
    Matched {
        /// 接管方把这条交接投递出去后，连接就归它所有。
        deliver_to_peer: oneshot::Sender<Handoff<T>>,
    },
    /// 没人等，进池子等对端
    Waiting {
        rx: oneshot::Receiver<Handoff<T>>,
        ticket: Ticket,
    },
}

/// 退出池子的凭据。超时或断开时用来清理自己的登记。
#[derive(Clone, Copy)]
pub struct Ticket {
    id: u64,
    key: TokenHash,
}

impl<T> Lobby<T> {
    pub fn new() -> Self {
        Self {
            waiters: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// 登记自己；能配上就直接配对。
    pub fn register(&self, frame: Phase1Frame) -> Outcome<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let me = frame.identity;
        let (tx, rx) = oneshot::channel();

        let mut guard = crate::lock(&self.waiters);

        // 我要找的人，正是「回头会来找我」的那个 key
        let key = me.src;
        if let Some(entries) = guard.get_mut(&key) {
            if let Some(pos) = entries
                .iter()
                // 双向匹配，并额外排除自配对（纵深防御：frame 层已拒 src == dst）
                .position(|e| {
                    me.src != me.dst && e.identity.src == me.dst && e.identity.dst == me.src
                })
            {
                let peer = entries.remove(pos);
                if entries.is_empty() {
                    guard.remove(&key);
                }
                drop(guard);
                return Outcome::Matched {
                    deliver_to_peer: peer.deliver,
                };
            }
        }

        guard.entry(me.dst).or_default().push(Entry {
            id,
            identity: me,
            deliver: tx,
        });

        Outcome::Waiting {
            rx,
            ticket: Ticket { id, key: me.dst },
        }
    }

    /// 退出池子。超时 / 断开时调用。
    pub fn withdraw(&self, ticket: Ticket) {
        let mut guard = crate::lock(&self.waiters);
        if let Some(entries) = guard.get_mut(&ticket.key) {
            entries.retain(|e| e.id != ticket.id);
            if entries.is_empty() {
                guard.remove(&ticket.key);
            }
        }
    }

    /// 清理「没人接听」的僵尸登记。返回清理条数。
    pub fn sweep(&self) -> usize {
        let mut guard = crate::lock(&self.waiters);
        let mut removed = 0usize;
        guard.retain(|_, entries| {
            let before = entries.len();
            entries.retain(|e| !e.deliver.is_closed());
            removed += before - entries.len();
            !entries.is_empty()
        });
        removed
    }

    /// 当前等候人数（诊断用，不含身份信息）。
    #[allow(dead_code)]
    pub fn waiting_count(&self) -> usize {
        crate::lock(&self.waiters).values().map(|v| v.len()).sum()
    }
}

impl<T> Default for Lobby<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::TokenHash;

    fn param() -> crate::frame::ParamSet {
        crate::frame::ParamSet {
            aead: "AES-256-GCM".into(),
            hash: "SHA3-256".into(),
            key_len: 256,
            module_flags: serde_json::Map::new(),
            random_len: 256,
            random_rate: 1024,
            window: 60,
        }
    }

    fn frame(src: u8, dst: u8) -> Phase1Frame {
        Phase1Frame {
            raw: vec![0u8; 228],
            param: param(),
            identity: Identity {
                src: TokenHash([src; 32]),
                dst: TokenHash([dst; 32]),
            },
        }
    }

    #[test]
    fn mutual_match_pairs_both() {
        let lobby: Lobby<u32> = Lobby::new();
        let first = lobby.register(frame(0xAA, 0xBB));
        assert_eq!(lobby.waiting_count(), 1);

        match lobby.register(frame(0xBB, 0xAA)) {
            Outcome::Matched { deliver_to_peer } => {
                // 模拟后到方把自己交过去
                assert!(
                    deliver_to_peer
                        .send(Handoff {
                            frame: frame(0xBB, 0xAA),
                            payload: 42
                        })
                        .is_ok(),
                    "先到方应该还在等"
                );
            }
            _ => panic!("B 应该配上 A"),
        }
        assert_eq!(lobby.waiting_count(), 0);

        // 先到方收到的必须正是后到方那一帧，而不是自己的或空的
        match first {
            Outcome::Waiting { rx, .. } => {
                let handoff = rx.blocking_recv().expect("先到方应当收到交接");
                assert_eq!(handoff.frame.identity.src.0, [0xBB; 32]);
                assert_eq!(handoff.frame.identity.dst.0, [0xAA; 32]);
                assert_eq!(handoff.payload, 42);
            }
            _ => panic!("A 应该在等候"),
        }
    }

    #[test]
    fn same_target_without_reverse_match_does_not_pair() {
        let lobby: Lobby<u32> = Lobby::new();
        let _a = lobby.register(frame(0xAA, 0xBB));
        assert!(matches!(
            lobby.register(frame(0xCC, 0xBB)),
            Outcome::Waiting { .. }
        ));
        assert_eq!(lobby.waiting_count(), 2);
    }

    #[test]
    fn self_pairing_is_refused() {
        let lobby: Lobby<u32> = Lobby::new();
        // src == dst 的两次登记不得互相配上
        let _a = lobby.register(frame(0xAA, 0xAA));
        let b = lobby.register(frame(0xAA, 0xAA));
        assert!(
            matches!(b, Outcome::Waiting { .. }),
            "自地址登记不得与自身配对"
        );
    }

    #[test]
    fn impersonation_rejected() {
        let lobby: Lobby<u32> = Lobby::new();
        let _a = lobby.register(frame(0xAA, 0xBB));
        // X 冒充：src=X, dst=A，A 要找的是 B，双向不成立
        assert!(matches!(
            lobby.register(frame(0xCC, 0xAA)),
            Outcome::Waiting { .. }
        ));
    }

    #[test]
    fn withdraw_cleans_up() {
        let lobby: Lobby<u32> = Lobby::new();
        let ticket = match lobby.register(frame(0xAA, 0xBB)) {
            Outcome::Waiting { ticket, .. } => ticket,
            _ => unreachable!(),
        };
        assert_eq!(lobby.waiting_count(), 1);
        lobby.withdraw(ticket);
        assert_eq!(lobby.waiting_count(), 0);
    }

    #[test]
    fn sweep_removes_dropped_receivers() {
        let lobby: Lobby<u32> = Lobby::new();
        match lobby.register(frame(0xAA, 0xBB)) {
            Outcome::Waiting { rx, .. } => drop(rx),
            _ => unreachable!(),
        }
        assert_eq!(lobby.sweep(), 1);
        assert_eq!(lobby.waiting_count(), 0);
    }
}
