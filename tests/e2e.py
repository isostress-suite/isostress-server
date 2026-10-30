#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 isostress Suite maintainers
# 端到端验证：真实 TCP 连服务端，模拟两个客户端完成阶段一配对并互发数据。
import socket, struct, subprocess, sys, time, os

PARAM = b'{"aead":"AES-256-GCM","hash":"SHA3-256","key_len":256,"module_flags":{},"random_len":256,"random_rate":1024,"window":60}'
assert len(PARAM) == 120, len(PARAM)

def phase1(src, dst):
    b = struct.pack('>I', len(PARAM)) + PARAM
    ident = bytes([src]*32) + bytes([dst]*32)
    b += struct.pack('>I', len(ident)) + ident
    nonce = bytes(range(32))
    b += struct.pack('>I', len(nonce)) + nonce
    return b

def recvn(s, n, timeout=5):
    s.settimeout(timeout)
    buf = b''
    while len(buf) < n:
        c = s.recv(n - len(buf))
        if not c:
            raise RuntimeError(f'对端提前关闭，只收到 {len(buf)}/{n}')
        buf += c
    return buf

PORT = 17443
proc = subprocess.Popen(['./target/debug/isostress-server', '--listen', f'127.0.0.1:{PORT}',
                         '--profile', 'beginner'], stderr=subprocess.PIPE)
time.sleep(1.2)
try:
    fa = phase1(0xAA, 0xBB)
    fb = phase1(0xBB, 0xAA)

    a = socket.create_connection(('127.0.0.1', PORT))
    a.sendall(fa)
    time.sleep(0.3)          # 让 A 先进等候池
    b = socket.create_connection(('127.0.0.1', PORT))
    b.sendall(fb)

    # 各自应收到「对方的阶段一帧」
    got_at_b = recvn(b, len(fa))
    got_at_a = recvn(a, len(fb))
    assert got_at_b == fa, 'B 收到的不是 A 的帧'
    assert got_at_a == fb, 'A 收到的不是 B 的帧'
    print('  [1/3] 阶段一配对与帧交换 OK')

    # 配对后应能双向透传任意字节（服务端不解密、不改写）
    a.sendall(b'hello-from-A')
    assert recvn(b, 12) == b'hello-from-A'
    b.sendall(b'hello-from-B')
    assert recvn(a, 12) == b'hello-from-B'
    print('  [2/3] 双向字节中继 OK')

    # 大块数据（验证不会被截断/改写）
    blob = bytes((i * 37) % 256 for i in range(100_000))
    a.sendall(blob)
    assert recvn(b, len(blob), timeout=15) == blob, '大块数据被破坏'
    print('  [3/3] 100KB 原样透传 OK')

    a.close(); b.close()
    print('\nE2E PASS')
finally:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
