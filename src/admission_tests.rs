// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers

use super::*;
use crate::metrics::Metrics;
use std::sync::atomic::{AtomicU64, Ordering};

fn fresh_metrics() -> Arc<Metrics> {
    Arc::new(Metrics {
        accepted: AtomicU64::new(0),
        rejected_silent: AtomicU64::new(0),
        paired: AtomicU64::new(0),
        lobby_timeout: AtomicU64::new(0),
        bytes_in: AtomicU64::new(0),
        bytes_out: AtomicU64::new(0),
        offline_stored: AtomicU64::new(0),
        offline_delivered: AtomicU64::new(0),
        group_broadcasts: AtomicU64::new(0),
        active_pairs: AtomicU64::new(0),
        active_conns: AtomicU64::new(0),
    })
}

#[test]
fn conn_guard_releases_the_slot_it_took() {
    let m = fresh_metrics();
    m.active_conns.fetch_add(1, Ordering::Relaxed);
    {
        let _g = ConnGuard(Arc::clone(&m));
    }
    assert_eq!(
        Metrics::get(&m.active_conns),
        0,
        "守卫析构必须把占用的连接名额还回去，否则上限会越用越紧"
    );
}

#[test]
fn connection_and_pair_counters_are_independent() {
    let m = fresh_metrics();
    m.active_conns.fetch_add(2, Ordering::Relaxed);
    m.active_pairs.fetch_add(1, Ordering::Relaxed);

    // 构造守卫本身不改计数，只有析构才归还名额
    assert_eq!(Metrics::get(&m.active_conns), 2);
    let g = ConnGuard(Arc::clone(&m));
    assert_eq!(Metrics::get(&m.active_conns), 2);
    assert_eq!(
        Metrics::get(&m.active_pairs),
        1,
        "释放一条连接不该动到配对计数：两者衡量的不是同一件事"
    );
    drop(g);
    assert_eq!(Metrics::get(&m.active_conns), 1);
    assert_eq!(Metrics::get(&m.active_pairs), 1, "配对计数仍然不该被动到");
}

#[test]
fn ip_guard_drops_the_entry_when_the_last_connection_goes() {
    let map: Mutex<HashMap<IpAddr, usize>> = Mutex::new(HashMap::new());
    let ip: IpAddr = "192.0.2.1".parse().expect("literal ip");
    *crate::lock(&map).entry(ip).or_insert(0) += 1;

    {
        let _g = IpGuard { map: &map, ip };
    }

    assert!(
        crate::lock(&map).is_empty(),
        "归零的 IP 条目必须移除，否则映射会随访问过的地址无限增长"
    );
}

#[test]
fn ip_guard_keeps_the_entry_while_another_connection_holds_it() {
    let map: Mutex<HashMap<IpAddr, usize>> = Mutex::new(HashMap::new());
    let ip: IpAddr = "192.0.2.2".parse().expect("literal ip");
    *crate::lock(&map).entry(ip).or_insert(0) += 2;

    {
        let _g = IpGuard { map: &map, ip };
    }

    assert_eq!(
        crate::lock(&map).get(&ip),
        Some(&1),
        "还有一条连接占着这个地址，条目不能被删"
    );
}

#[test]
fn ip_guard_on_an_absent_entry_does_not_panic() {
    // 析构走 saturating 减法：即便条目已经不在，也只是无事发生
    let map: Mutex<HashMap<IpAddr, usize>> = Mutex::new(HashMap::new());
    let ip: IpAddr = "192.0.2.3".parse().expect("literal ip");

    {
        let _g = IpGuard { map: &map, ip };
    }

    assert!(crate::lock(&map).is_empty());
}
