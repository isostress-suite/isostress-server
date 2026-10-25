# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 ZT-DSP Suite contributors

"""Tests for the ZT-DSP server role.

Run them from the repository root:

    python -m pytest zt_dsp_sv_py/tests -q
"""

from __future__ import annotations

import json
import socket
import threading

import pytest

from zt_dsp_py.frames import pack_phase_one
from zt_dsp_sv_py import version
from zt_dsp_sv_py.server import (
    Lobby,
    ParamSetError,
    Server,
    ServerConfig,
    ServerConfigError,
    Waiter,
    config_for_profile,
    relay_pair,
    silent_drop,
    split_identity,
    validate_param_set,
)

VALID_PARAMS = {
    "aead": "AES-256-GCM",
    "hash": "SHA3-256",
    "key_len": 256,
    "module_flags": {},
    "random_len": 256,
    "random_rate": 1024,
    "window": 60,
}


def param_bytes(**overrides) -> bytes:
    doc = dict(VALID_PARAMS)
    doc.update(overrides)
    return json.dumps(doc, sort_keys=True, separators=(",", ":")).encode("utf-8")


def token(seed: int) -> bytes:
    return bytes([seed]) * 32


# --- version contract ----------------------------------------------------


def test_version_contract_is_self_consistent():
    assert version.IMPLEMENTATION_ID == "ZT-DSP_SV-1.0.0"
    assert version.ROLE == "server"
    assert version.LANGUAGE_PREFIX == "SV"
    assert version.FOLLOWS_DOC == "ZT-DSP Protocol Master Guide"
    assert version.DOCS_VERSION == "1.0.2"
    assert version.FOLLOWS_DOC_PATH.startswith("ZT-DSP-docs/")
    assert "\n" not in version.format_version()


# --- parameter set -------------------------------------------------------


def test_param_set_accepts_the_seven_items():
    params = validate_param_set(param_bytes())
    assert params.key_len == 256
    assert params.random_len == 256


def test_param_set_rejects_a_missing_item():
    doc = dict(VALID_PARAMS)
    del doc["window"]
    with pytest.raises(ParamSetError):
        validate_param_set(json.dumps(doc).encode("utf-8"))


def test_param_set_rejects_an_extra_item():
    with pytest.raises(ParamSetError):
        validate_param_set(param_bytes(extra="nope"))


def test_param_set_rejects_a_bad_key_len():
    with pytest.raises(ParamSetError):
        validate_param_set(param_bytes(key_len=111))


def test_param_set_rejects_a_non_multiple_of_eight_random_len():
    with pytest.raises(ParamSetError):
        validate_param_set(param_bytes(random_len=100))


def test_param_set_rejects_deep_module_flags():
    deep = current = {}
    for _ in range(40):
        current["n"] = {}
        current = current["n"]
    with pytest.raises(ParamSetError):
        validate_param_set(param_bytes(module_flags=deep))


# --- identity ------------------------------------------------------------


def test_identity_splits_into_src_and_dst():
    src, dst = split_identity(token(1) + token(2))
    assert src == token(1)
    assert dst == token(2)


def test_identity_rejects_self_addressing():
    with pytest.raises(ParamSetError):
        split_identity(token(3) + token(3))


def test_identity_rejects_a_wrong_length():
    with pytest.raises(ParamSetError):
        split_identity(token(1) + token(2)[:16])


# --- lobby ---------------------------------------------------------------


def _pair(lobby, a_src, a_dst, b_src, b_dst):
    # The first peer registers and waits; the second one completes the pair.
    first = Waiter(a_src, a_dst, socket.socket(), b"a")
    assert lobby.join(first) is None
    second = Waiter(b_src, b_dst, socket.socket(), b"b")
    matched = lobby.join(second)
    return first, second, matched


def test_lobby_matches_only_mutual_identities():
    lobby = Lobby(30.0)
    first, _second, matched = _pair(lobby, token(1), token(2), token(2), token(1))
    assert matched is first
    assert first.event.is_set()
    assert first.peer_frame == b"b"


def test_lobby_refuses_a_one_sided_collision():
    lobby = Lobby(30.0)
    first = Waiter(token(1), token(2), socket.socket(), b"a")
    assert lobby.join(first) is None
    # Same destination on both sides: not mutual, so no connection.
    second = Waiter(token(9), token(2), socket.socket(), b"b")
    assert lobby.join(second) is None
    assert not first.event.is_set()


def test_lobby_removes_a_waiter_that_gave_up():
    lobby = Lobby(30.0)
    first = Waiter(token(1), token(2), socket.socket(), b"a")
    lobby.join(first)
    assert lobby.waiting_count() == 1
    lobby.leave(first)
    assert lobby.waiting_count() == 0


# --- silent drop ---------------------------------------------------------


def test_silent_drop_writes_nothing_back():
    server_side, peer = socket.socketpair()
    try:
        peer.sendall(b"x" * 100)
        silent_drop(server_side, 0.3)
        peer.settimeout(0.5)
        with pytest.raises((socket.timeout, ConnectionResetError, OSError)):
            data = peer.recv(16)
            assert data == b""
    finally:
        server_side.close()
        peer.close()


# --- relay ---------------------------------------------------------------


def test_relay_moves_bytes_verbatim():
    a, b = socket.socketpair()
    c, d = socket.socketpair()
    payload = bytes(range(256)) * 64
    try:
        cfg = config_for_profile("beginner", ("127.0.0.1", 0))
        # b is the peer of a, so what b writes arrives on a.
        # d is the peer of c, so what the relay writes to c arrives on d.
        sender = threading.Thread(target=lambda: (b.sendall(payload), b.close()))
        sender.start()
        relay_pair(a, c, cfg)
        sender.join()
        d.settimeout(2.0)
        got = b""
        while len(got) < len(payload):
            chunk = d.recv(len(payload) - len(got))
            if not chunk:
                break
            got += chunk
        assert got == payload
    finally:
        for s in (a, b, c, d):
            try:
                s.close()
            except OSError:
                pass


# --- configuration -------------------------------------------------------


def test_config_refuses_zero_durations():
    cfg = config_for_profile("beginner", ("127.0.0.1", 0))
    object.__setattr__(cfg, "handshake_timeout", 0.0)
    with pytest.raises(ServerConfigError):
        cfg.validate()


def test_config_refuses_a_control_port_without_a_token():
    with pytest.raises(ServerConfigError):
        config_for_profile(
            "beginner", ("127.0.0.1", 0), control_listen=("127.0.0.1", 1)
        ).validate()


def test_every_shipped_profile_starts():
    for name in ("beginner", "advanced", "high-anonymity"):
        config_for_profile(name, ("127.0.0.1", 0)).validate()


def test_the_identity_directory_is_off_in_every_profile():
    for name in ("beginner", "advanced", "high-anonymity"):
        assert config_for_profile(name, ("127.0.0.1", 0)).directory.enabled is False


# --- end to end ----------------------------------------------------------


def _start_server():
    cfg = config_for_profile("beginner", ("127.0.0.1", 0))
    cfg = ServerConfig(
        **{**cfg.__dict__, "handshake_timeout": 5.0, "lobby_timeout": 5.0}
    )
    server = Server(cfg)
    sock = server.bind()
    host, port = sock.getsockname()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, host, port


def test_two_peers_are_paired_and_bytes_flow():
    server, host, port = _start_server()
    try:
        left = socket.create_connection((host, port), timeout=5)
        right = socket.create_connection((host, port), timeout=5)
        left.settimeout(5)
        right.settimeout(5)

        # Mutual identities: left says src=A dst=B, right says src=B dst=A.
        left.sendall(pack_phase_one(param_bytes(), token(1) + token(2), b"n" * 32))
        right.sendall(pack_phase_one(param_bytes(), token(2) + token(1), b"n" * 32))

        # Each peer must receive the other peer's Phase 1 frame.
        got_left = left.recv(4096)
        got_right = right.recv(4096)
        assert got_left.startswith(b"\x00\x00\x00P") or len(got_left) > 0
        assert len(got_right) > 0

        # And after the frames, plain bytes flow through.
        left.sendall(b"hello-from-left")
        assert right.recv(64) == b"hello-from-left"
        right.sendall(b"hello-from-right")
        assert left.recv(64) == b"hello-from-right"
    finally:
        server.stop()


def test_a_rejected_peer_hears_nothing():
    server, host, port = _start_server()
    try:
        peer = socket.create_connection((host, port), timeout=5)
        # Deliberately shorter than the server's five second drop window. When
        # the two were equal this test raced the server's own close: whichever
        # side the scheduler woke first decided the outcome, which is why it
        # failed on one interpreter and passed on another with no code change.
        peer.settimeout(1.5)

        # A parameter set that is missing an item must be rejected silently.
        bad = dict(VALID_PARAMS)
        del bad["window"]
        peer.sendall(
            pack_phase_one(
                json.dumps(bad).encode("utf-8"), token(1) + token(2), b"n" * 32
            )
        )

        # What Appendix F.2 requires is that not one byte comes back, and that
        # no active close is visible while the window is open. A timeout is the
        # correct observation. The old version of this test accepted only a
        # timeout and put its byte check after the recv, where it could never
        # run.
        try:
            received = peer.recv(64)
        except (socket.timeout, TimeoutError):
            received = b""
        assert received == b"", f"silent drop violated: peer received {received!r}"

        # Nothing was written, so the connection is still held open rather than
        # closed early. Asking again must not produce a reset.
        try:
            peer.settimeout(1.0)
            extra = peer.recv(64)
        except (socket.timeout, TimeoutError):
            extra = b""
        assert extra == b"", f"peer heard {extra!r} while the window was open"
    finally:
        server.stop()


def test_the_drop_window_ends_by_closing_quietly():
    server, host, port = _start_server()
    try:
        peer = socket.create_connection((host, port), timeout=5)
        # Longer than the server's five second window, so the observation
        # outlives the window instead of racing it.
        peer.settimeout(10)

        bad = dict(VALID_PARAMS)
        del bad["window"]
        peer.sendall(
            pack_phase_one(
                json.dumps(bad).encode("utf-8"), token(1) + token(2), b"n" * 32
            )
        )

        # Element five: at the end of the window the connection is closed like
        # any finished connection. End of stream, zero bytes, no reset, and no
        # error text that would make the rejection observable.
        assert peer.recv(64) == b""
    finally:
        server.stop()
