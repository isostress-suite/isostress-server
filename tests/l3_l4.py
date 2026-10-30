#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 isostress Suite maintainers
# 手动验收：静默丢弃 + L4 控制面（强制鉴权）。自动化回归见 tests/integration.rs。
import json, socket, struct, subprocess, time, sys

PORT, CPORT = 17453, 17454
TOKEN = 'test-token-please-rotate'

def start():
    p = subprocess.Popen(['./target/debug/isostress-server',
                          '--listen', f'127.0.0.1:{PORT}',
                          '--control', f'127.0.0.1:{CPORT}',
                          '--control-token', TOKEN,
                          '--profile', 'beginner'],
                         stderr=subprocess.PIPE)
    time.sleep(1.2)
    return p

def test_silent_drop():
    s = socket.create_connection(('127.0.0.1', PORT))
    s.sendall(struct.pack('>I', 99999))
    s.settimeout(2.5)
    try:
        data = s.recv(4096)
        assert data == b'', f'静默丢弃失败：服务器回了 {len(data)} 字节'
        print('  [1] 静默丢弃：未回任何字节 OK')
    except socket.timeout:
        print('  [1] 静默丢弃：连接挂起无响应 OK')
    finally:
        s.close()

def call(sock, obj):
    sock.sendall((json.dumps(obj) + '\n').encode())
    buf = b''
    while not buf.endswith(b'\n'):
        c = sock.recv(65536)
        if not c:
            raise RuntimeError('控制面提前关闭')
        buf += c
    return json.loads(buf.decode())

def test_control():
    c = socket.create_connection(('127.0.0.1', CPORT))
    c.settimeout(5)
    h = 'ab' * 32
    h2 = 'cd' * 32

    r = call(c, {'op': 'ping'})
    assert r['ok'] is False, '未鉴权竟然能 ping'
    print('  [2] 未鉴权请求被拒绝 OK')

    r = call(c, {'op': 'auth', 'token': 'wrong'})
    assert r['ok'] is False
    print('  [3] 错误口令被拒绝 OK')

    r = call(c, {'op': 'auth', 'token': TOKEN})
    assert r['ok'] and r['data']['authed']
    print('  [4] 正确口令通过 OK')

    r = call(c, {'op': 'ping'})
    assert r['ok'] and r['data']['pong']
    print('  [5] ping OK')

    r = call(c, {'op': 'offline_push', 'dst': h, 'payload': 'deadbeef'})
    assert r['ok'], r
    r = call(c, {'op': 'offline_pull', 'dst': h})
    assert r['ok'] and r['data']['items'] == ['deadbeef'], r
    print('  [6] 离线消息存取一致 OK')

    r = call(c, {'op': 'group_create', 'name': 'g1', 'owner': h})
    assert r['ok'], r
    r = call(c, {'op': 'group_create', 'name': 'g1', 'owner': h})
    assert r['ok'] is False, '重复创建必须报错'
    print('  [7] 群组创建 + 重复创建被拒 OK')

    r = call(c, {'op': 'group_join', 'name': 'g1', 'member': h2})
    assert r['ok'], r
    r = call(c, {'op': 'group_send', 'name': 'g1', 'sender': h, 'payload': 'cafe'})
    assert r['ok'], r
    r = call(c, {'op': 'group_pull', 'name': 'g1', 'member': h2})
    assert r['ok'] and r['data']['items'][0]['payload'] == 'cafe', r
    print('  [8] 群组广播 + 游标拉取 OK')

    r = call(c, {'op': 'dir_lookup', 'hash': h})
    assert r['ok'] is False and '关闭' in r['error'], r
    print('  [9] 身份目录默认关闭 OK')

    r = call(c, {'op': 'stats'})
    assert r['ok'] and 'active_conns' in r['data']
    print(f"  [10] 统计 OK: accepted={r['data']['accepted']}")
    c.close()

p = start()
try:
    test_silent_drop()
    test_control()
    print('\nL3+L4 PASS')
finally:
    p.terminate()
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()
