# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (c) 2026 ZT-DSP Suite contributors

"""The ZT-DSP server role: pair peers, then relay opaque bytes.

The server never decrypts anything.  It reads one Phase 1 frame (Appendix F),
takes the two token hashes that make up the pairing identity out of the
identity announcement, matches peers whose identities are mutual inverses,
puts each peer's Phase 1 frame where the other peer expects it, and from then
on moves bytes between the two sockets without interpreting them.  Phases 2-4
happen entirely between the peers.

Three properties are load-bearing and must not be relaxed:

1. The relay is byte-exact.  Phase 1 frames are replayed verbatim - the raw
   bytes enter the Phase 3 transcript, and a canonicalised re-encoding would
   make the confirmation tag mismatch (Appendix F.0, Convention 6).
2. Rejection is silent.  Every rejection path drains the connection for the
   same window, sends nothing, and never writes an error back (Appendix F.2).
3. Pairing is mutual.  A peer is only connected to the peer whose identity
   is the exact inverse of its own; a one-sided collision never connects.

The L4 features (offline envelopes, group log, identity directory) live on a
separate control plane, never in the wire format: Appendix F frames have no
frame type field, and their meaning is decided by handshake position, so
inserting new frames would break interoperability with existing clients.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import logging
import os
import select
import signal
import socket
import sys
import threading
import time
from dataclasses import dataclass, field
from typing import Any

from zt_dsp_py.errors import (
    ZTDSPParamError,
    ZTDSPProtocolError,
    ZTDSPTimeoutError,
)
from zt_dsp_py.frames import unpack_phase_one
from zt_dsp_py.params import ZTDSPParams, validate_module_flags

from . import version

log = logging.getLogger("zt_dsp_sv")

TOKEN_HASH_LEN = 32
IDENTITY_LEN = TOKEN_HASH_LEN * 2

#: The seven parameters the Phase 1 parameter set carries (Appendix F.1).
REQUIRED_PARAMS = frozenset(
    ("aead", "hash", "key_len", "module_flags", "random_len", "random_rate", "window")
)

KEY_LENS = (128, 192, 256)

#: Hard cap on the number of module_flags nodes (Appendix F.6).
MAX_FLAG_NODES = 1024

#: Hard cap on the module_flags nesting depth (Appendix F.6).
MAX_FLAG_DEPTH = 32

DRAIN_CHUNK = 8192


class ServerConfigError(ValueError):
    """Configuration that would make the server misbehave at runtime."""


@dataclass(frozen=True)
class OfflineConfig:
    enabled: bool = False
    max_payload: int = 65536
    max_recipients: int = 64
    max_per_recipient: int = 32
    ttl_secs: int = 3600


@dataclass(frozen=True)
class GroupConfig:
    enabled: bool = False
    max_payload: int = 65536
    max_groups: int = 32
    ttl_secs: int = 3600


@dataclass(frozen=True)
class DirectoryConfig:
    # Off in every profile.  The identity directory records when a token hash
    # appeared, which is pairing metadata, and the specification forbids
    # recording pairing metadata.  Enabling it is an explicit, loud decision.
    enabled: bool = False
    ttl_secs: int = 3600


@dataclass(frozen=True)
class ServerConfig:
    """Everything the server needs, resolved once at startup."""

    profile: str = "beginner"
    listen: tuple[str, int] = ("127.0.0.1", 9931)
    max_connections: int = 512
    per_ip_limit: int = 8
    handshake_timeout: float = 15.0
    lobby_timeout: float = 30.0
    idle_timeout: float = 300.0
    buffer_bytes: int = 65536
    offline: OfflineConfig = field(default_factory=OfflineConfig)
    group: GroupConfig = field(default_factory=GroupConfig)
    directory: DirectoryConfig = field(default_factory=DirectoryConfig)
    control_listen: tuple[str, int] | None = None
    control_token: bytes | None = None

    def validate(self) -> None:
        """Reject a configuration that would misbehave, instead of clamping it.

        Zero values are refused outright.  A value belonging to a disabled
        feature is not checked: in the high-anonymity profile the offline
        store is off and its limits legitimately stay at zero.
        """
        if self.handshake_timeout <= 0:
            raise ServerConfigError("handshake_timeout must be greater than zero")
        if self.lobby_timeout <= 0:
            raise ServerConfigError("lobby_timeout must be greater than zero")
        if self.idle_timeout <= 0:
            raise ServerConfigError("idle_timeout must be greater than zero")
        if self.max_connections <= 0:
            raise ServerConfigError("max_connections must be greater than zero")
        if self.per_ip_limit <= 0:
            raise ServerConfigError("per_ip_limit must be greater than zero")
        if self.buffer_bytes <= 0:
            raise ServerConfigError("buffer_bytes must be greater than zero")
        if self.offline.enabled:
            for name in (
                "max_payload",
                "max_recipients",
                "max_per_recipient",
                "ttl_secs",
            ):
                if getattr(self.offline, name) <= 0:
                    raise ServerConfigError(
                        f"offline.{name} must be greater than zero when offline is enabled"
                    )
        if self.group.enabled:
            for name in ("max_payload", "max_groups", "ttl_secs"):
                if getattr(self.group, name) <= 0:
                    raise ServerConfigError(
                        f"group.{name} must be greater than zero when group is enabled"
                    )
        if self.directory.enabled and self.directory.ttl_secs <= 0:
            raise ServerConfigError(
                "directory.ttl_secs must be greater than zero when directory is enabled"
            )
        if self.control_listen is not None and not self.control_token:
            raise ServerConfigError(
                "a control port without a token is refused: start with --control-token"
            )


PROFILES: dict[str, dict[str, Any]] = {
    "beginner": {
        "label": "B2-a beginner",
        "max_connections": 256,
        "per_ip_limit": 4,
        "offline": OfflineConfig(enabled=False),
        "group": GroupConfig(enabled=False),
    },
    "advanced": {
        "label": "B2-b advanced",
        "max_connections": 1024,
        "per_ip_limit": 16,
        "offline": OfflineConfig(enabled=True),
        "group": GroupConfig(enabled=True),
    },
    "high-anonymity": {
        "label": "B2-c high anonymity",
        "max_connections": 128,
        "per_ip_limit": 2,
        "offline": OfflineConfig(enabled=False),
        "group": GroupConfig(enabled=False),
    },
}


def config_for_profile(
    name: str,
    listen: tuple[str, int],
    control_listen: tuple[str, int] | None = None,
    control_token: bytes | None = None,
) -> ServerConfig:
    """Build the configuration of one Appendix B.2 profile."""
    if name not in PROFILES:
        raise ServerConfigError(
            "unknown profile {!r}; expected one of {}".format(
                name, ", ".join(sorted(PROFILES))
            )
        )
    p = PROFILES[name]
    return ServerConfig(
        profile=name,
        listen=listen,
        max_connections=p["max_connections"],
        per_ip_limit=p["per_ip_limit"],
        offline=p["offline"],
        group=p["group"],
        control_listen=control_listen,
        control_token=control_token,
    )


class ParamSetError(Exception):
    """The Phase 1 parameter set is not acceptable."""


def validate_param_set(param_raw: bytes) -> ZTDSPParams:
    """Validate the Phase 1 parameter set against Appendix F.1 and F.6.

    zt_dsp_py.params.ZTDSPParams.from_wire checks the shape and the types.
    The four rules below are the server's own duty, because the client is the
    party that produces these values:

    * the set carries exactly the seven required parameters - no more, no less;
    * key_len is one of 128 / 192 / 256;
    * random_len is a positive multiple of 8, and nonce_len * 8 == random_len;
    * module_flags nests at most 32 deep and holds at most 1024 nodes.

    Returns the parsed parameter set.  Raises ParamSetError otherwise.
    """
    try:
        doc = json.loads(param_raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ParamSetError("parameter payload is not valid UTF-8 JSON") from exc

    if not isinstance(doc, dict):
        raise ParamSetError("parameter payload must be a JSON object")

    keys = set(doc)
    if keys != REQUIRED_PARAMS:
        missing = sorted(REQUIRED_PARAMS - keys)
        extra = sorted(keys - REQUIRED_PARAMS)
        raise ParamSetError(
            f"parameter set mismatch (missing={missing}, unexpected={extra})"
        )

    # from_wire already enforces the type rules, that random_len is a positive
    # multiple of 8, that key_len is one of 128/192/256, and that
    # random_len/8 stays within the connection-random cap.
    try:
        params = ZTDSPParams.from_wire(doc)
    except ZTDSPParamError as exc:
        raise ParamSetError(str(exc)) from exc

    try:
        validate_module_flags(params.module_flags)
    except ZTDSPParamError as exc:
        raise ParamSetError(str(exc)) from exc

    return params


def split_identity(identity: bytes) -> tuple[bytes, bytes]:
    """Split the identity announcement into (src token hash, dst token hash).

    The announcement is 64 bytes: the source token hash followed by the
    destination token hash, each 32 bytes.
    """
    if len(identity) != IDENTITY_LEN:
        raise ParamSetError(
            f"identity announcement must be {IDENTITY_LEN} bytes, got {len(identity)}"
        )
    src = identity[:TOKEN_HASH_LEN]
    dst = identity[TOKEN_HASH_LEN:]
    if src == dst:
        raise ParamSetError("self-addressed identity announcement")
    return src, dst


def _token_label(token: bytes) -> str:
    """A short, non-reversible label for a token hash.

    The full hash is identity material and must never reach a log.  Four bytes
    of it are enough to tell two entries apart in a diagnostic.
    """
    return token[:4].hex()


class Waiter:
    """One peer waiting in the lobby for its counterpart."""

    __slots__ = (
        "done",
        "dst",
        "event",
        "frame",
        "outcome",
        "peer_frame",
        "sock",
        "src",
    )

    def __init__(
        self, src: bytes, dst: bytes, sock: socket.socket, frame: bytes
    ) -> None:
        self.src = src
        self.dst = dst
        self.sock = sock
        self.frame = frame
        self.event = threading.Event()
        self.done = threading.Event()
        self.peer_frame: bytes | None = None
        self.outcome: str | None = None


class Lobby:
    """The pairing pool.

    A peer is only ever connected to the peer whose identity is the exact
    inverse of its own: newcomer.src == waiter.dst and newcomer.dst ==
    waiter.src.  A one-sided collision - two peers that happen to pick the
    same destination - never connects.
    """

    def __init__(self, timeout: float) -> None:
        self._timeout = timeout
        self._lock = threading.Lock()
        self._waiters: dict[tuple[bytes, bytes], list[Waiter]] = {}

    def join(self, waiter: Waiter) -> Waiter | None:
        """Register a peer. Returns the counterpart if this completes a pair."""
        with self._lock:
            bucket = self._waiters.get((waiter.dst, waiter.src))
            if bucket:
                peer = bucket.pop(0)
                if not bucket:
                    del self._waiters[(waiter.dst, waiter.src)]
                peer.peer_frame = waiter.frame
                peer.outcome = "matched"
                peer.event.set()
                return peer
            self._waiters.setdefault((waiter.src, waiter.dst), []).append(waiter)
            return None

    def leave(self, waiter: Waiter) -> None:
        """Remove a waiter that gave up."""
        with self._lock:
            bucket = self._waiters.get((waiter.src, waiter.dst))
            if not bucket:
                return
            try:
                bucket.remove(waiter)
            except ValueError:
                return
            if not bucket:
                del self._waiters[(waiter.src, waiter.dst)]

    def waiting_count(self) -> int:
        with self._lock:
            return sum(len(b) for b in self._waiters.values())


def silent_drop(sock: socket.socket, seconds: float) -> None:
    """Appendix F.2 silent drop.

    Five elements, all of them together:

    1. not a single byte is written back to the peer;
    2. the connection is not closed while the window is open - the peer sees
       neither a FIN nor a RST and cannot tell this apart from a peer that is
       merely slow;
    3. the incoming side is drained in 8192 byte chunks, so a peer that keeps
       writing cannot turn a rejection into a memory problem;
    4. diagnostics go to the local log only, never to the peer;
    5. at the end of the window the connection is closed like any other
       finished connection - no error is reported, because a report is the
       one thing that would make the rejection observable.

    The same window is used by every rejection path, so a rejected peer
    cannot tell one reason from another by timing.
    """
    deadline = time.monotonic() + seconds
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            sock.settimeout(remaining)
            data = sock.recv(DRAIN_CHUNK)
            if not data:
                break
    except (TimeoutError, OSError):
        pass


def relay_pair(sock_a: socket.socket, sock_b: socket.socket, cfg: ServerConfig) -> str:
    """Move bytes between two peers until one of them stops talking.

    Nothing in here interprets the payload.  The server does not know what
    phase the peers are in and does not need to: after the Phase 1 frames
    have been swapped, the two sockets are byte streams that belong to the
    peers alone.
    """
    deadline = time.monotonic() + cfg.idle_timeout
    while True:
        timeout = deadline - time.monotonic()
        if timeout <= 0:
            return "idle"
        try:
            readable, _, _ = select.select([sock_a, sock_b], [], [], timeout)
        except (OSError, ValueError):
            return "error"
        if not readable:
            return "idle"
        for ready in readable:
            try:
                data = ready.recv(cfg.buffer_bytes)
            except OSError:
                return "error"
            if not data:
                return "eof"
            other = sock_b if ready is sock_a else sock_a
            try:
                other.sendall(data)
            except OSError:
                return "error"


class ServerState:
    """Counters and the per-IP admission limit."""

    def __init__(self, cfg: ServerConfig) -> None:
        self.cfg = cfg
        self._lock = threading.Lock()
        self._per_ip: dict[str, int] = {}
        self.active_connections = 0
        self.active_pairs = 0
        self.rejected = 0

    def admit(self, peer_ip: str) -> bool:
        """Take a connection slot, honouring the per-IP limit."""
        with self._lock:
            if self.active_connections >= self.cfg.max_connections:
                return False
            current = self._per_ip.get(peer_ip, 0)
            if current >= self.cfg.per_ip_limit:
                return False
            self._per_ip[peer_ip] = current + 1
            self.active_connections += 1
            return True

    def release(self, peer_ip: str) -> None:
        with self._lock:
            self.active_connections -= 1
            remaining = self._per_ip.get(peer_ip, 1) - 1
            if remaining <= 0:
                self._per_ip.pop(peer_ip, None)
            else:
                self._per_ip[peer_ip] = remaining

    def pair_opened(self) -> None:
        with self._lock:
            self.active_pairs += 1

    def pair_closed(self) -> None:
        with self._lock:
            self.active_pairs -= 1

    def note_rejection(self) -> None:
        with self._lock:
            self.rejected += 1

    def snapshot(self) -> dict[str, int]:
        with self._lock:
            return {
                "active_connections": self.active_connections,
                "active_pairs": self.active_pairs,
                "rejected": self.rejected,
            }


def handle_connection(
    sock: socket.socket,
    peer: Any,
    cfg: ServerConfig,
    lobby: Lobby,
    state: ServerState,
) -> None:
    """Serve one connection, from Phase 1 to the end of the relay.

    Every rejection path ends in the same silent drop with the same window,
    so a rejected peer cannot tell one reason from another by timing, and
    cannot tell a rejection from a closed port.
    """
    peer_ip = peer[0] if peer else "?"
    if not state.admit(peer_ip):
        state.note_rejection()
        log.debug("peer %s rejected: admission limit", peer_ip)
        silent_drop(sock, cfg.handshake_timeout)
        return

    try:
        deadline = time.monotonic() + cfg.handshake_timeout
        try:
            raw, param_raw, identity, nonce = unpack_phase_one(sock, deadline)
        except (ZTDSPProtocolError, ZTDSPTimeoutError, OSError) as exc:
            state.note_rejection()
            log.debug("peer %s phase 1 rejected: %s", peer_ip, exc)
            silent_drop(sock, cfg.handshake_timeout)
            return

        try:
            params = validate_param_set(param_raw)
            src, dst = split_identity(identity)
        except ParamSetError as exc:
            state.note_rejection()
            log.debug("peer %s phase 1 invalid: %s", peer_ip, exc)
            silent_drop(sock, cfg.handshake_timeout)
            return

        # The one peer-side check the specification gives for the access nonce:
        # the peer cannot recompute it, so all that can be checked is its length.
        if len(nonce) * 8 != params.random_len:
            state.note_rejection()
            log.debug(
                "peer %s phase 1 invalid: nonce length %d does not match random_len %d",
                peer_ip,
                len(nonce),
                params.random_len,
            )
            silent_drop(sock, cfg.handshake_timeout)
            return

        log.debug(
            "peer %s phase 1 accepted (src=%s dst=%s)",
            peer_ip,
            _token_label(src),
            _token_label(dst),
        )

        waiter = Waiter(src, dst, sock, raw)
        counterpart = lobby.join(waiter)

        if counterpart is None:
            # We arrived first.  Wait for the peer whose identity is ours.
            if not waiter.event.wait(cfg.lobby_timeout):
                lobby.leave(waiter)
                state.note_rejection()
                log.debug("peer %s lobby timeout", peer_ip)
                silent_drop(sock, cfg.handshake_timeout)
                return
            # The matcher owns the relay now; this thread only waits for it.
            waiter.done.wait()
            return

        # We are the second one.  Swap the Phase 1 frames, then relay.
        #
        # The frames go to the *other* socket: the peer that sent first waits
        # for the other peer's frame, so the frame read from one socket has to
        # be delivered to the other one.
        try:
            sock.sendall(counterpart.frame)
            counterpart.sock.sendall(raw)
        except OSError as exc:
            log.debug("peer %s handoff failed: %s", peer_ip, exc)
            counterpart.done.set()
            state.note_rejection()
            silent_drop(sock, cfg.handshake_timeout)
            return

        state.pair_opened()
        try:
            cause = relay_pair(sock, counterpart.sock, cfg)
            log.debug("peer %s relay ended: %s", peer_ip, cause)
        finally:
            state.pair_closed()
            counterpart.done.set()
    finally:
        state.release(peer_ip)
        with contextlib.suppress(OSError):
            sock.close()


class Server:
    """The listening server."""

    def __init__(self, cfg: ServerConfig) -> None:
        self.cfg = cfg
        self.lobby = Lobby(cfg.lobby_timeout)
        self.state = ServerState(cfg)
        self._sock: socket.socket | None = None
        self._stop = threading.Event()

    def bind(self) -> socket.socket:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(self.cfg.listen)
        sock.listen(128)
        self._sock = sock
        return sock

    def serve_forever(self) -> None:
        assert self._sock is not None
        self._sock.settimeout(0.5)
        while not self._stop.is_set():
            try:
                conn, peer = self._sock.accept()
            except TimeoutError:
                continue
            except OSError:
                if self._stop.is_set():
                    break
                # Do not spin at full speed on a transient accept error.
                time.sleep(0.05)
                continue
            conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            thread = threading.Thread(
                target=self._serve_one,
                args=(conn, peer),
                daemon=True,
            )
            thread.start()

    def _serve_one(self, conn: socket.socket, peer: Any) -> None:
        try:
            handle_connection(conn, peer, self.cfg, self.lobby, self.state)
        except Exception:
            log.exception("unhandled error while serving %s", peer)
            with contextlib.suppress(OSError):
                conn.close()

    def stop(self) -> None:
        self._stop.set()
        if self._sock is not None:
            with contextlib.suppress(OSError):
                self._sock.close()


# --------------------------------------------------------------------------
# L4 control plane
#
# Offline envelopes, the group log and the identity directory live here, on a
# separate port speaking JSON Lines.  They are deliberately NOT in the wire
# format: an Appendix F frame has no frame type field and its meaning is
# decided by handshake position, so putting these on the data plane would
# break interoperability with every existing client.
# --------------------------------------------------------------------------


class ControlError(Exception):
    """An L4 operation failed.

    Carries a stable machine-readable code next to the human-readable text.
    The code is what a machine client matches on; the text may be localised
    or reworded at any time.
    """

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


class OfflineStore:
    """Per-recipient envelopes with FIFO eviction and a TTL."""

    def __init__(self, cfg: OfflineConfig) -> None:
        self.cfg = cfg
        self._lock = threading.Lock()
        self._boxes: dict[str, list[tuple[float, bytes]]] = {}

    def push(self, recipient: str, payload: bytes) -> int:
        if not self.cfg.enabled:
            raise ControlError("disabled", "the offline store is not enabled")
        if len(payload) > self.cfg.max_payload:
            raise ControlError(
                "payload_too_large",
                f"payload exceeds the {self.cfg.max_payload} byte limit",
            )
        now = time.time()
        with self._lock:
            box = self._boxes.setdefault(recipient, [])
            box.append((now, payload))
            while len(box) > self.cfg.max_per_recipient:
                box.pop(0)
            self._evict_locked(now)
            return len(box)

    def pull(self, recipient: str) -> list[bytes]:
        if not self.cfg.enabled:
            raise ControlError("disabled", "the offline store is not enabled")
        now = time.time()
        with self._lock:
            self._evict_locked(now)
            box = self._boxes.get(recipient, [])
            out = [payload for _, payload in box]
            del box[:]
            return out

    def _evict_locked(self, now: float) -> None:
        deadline = now - self.cfg.ttl_secs
        for recipient in list(self._boxes):
            box = self._boxes[recipient]
            kept = [item for item in box if item[0] >= deadline]
            if kept:
                self._boxes[recipient] = kept
            else:
                del self._boxes[recipient]


class Group:
    """One group: a bounded member set and a log with per-member cursors."""

    def __init__(self, name: str, members: list[str], ttl_secs: int) -> None:
        self.name = name
        self.members = set(members)
        self.log: list[tuple[float, str, bytes]] = []
        self.cursors: dict[str, int] = {m: 0 for m in members}
        self.created = time.time()
        self.ttl_secs = ttl_secs


class Groups:
    """A bounded set of groups."""

    def __init__(self, cfg: GroupConfig) -> None:
        self.cfg = cfg
        self._lock = threading.Lock()
        self._groups: dict[str, Group] = {}

    def create(self, name: str, members: list[str]) -> None:
        if not self.cfg.enabled:
            raise ControlError("disabled", "groups are not enabled")
        with self._lock:
            if name in self._groups:
                raise ControlError("already_exists", f"group {name!r} already exists")
            if len(self._groups) >= self.cfg.max_groups:
                raise ControlError(
                    "group_limit",
                    f"the server already holds {self.cfg.max_groups} groups",
                )
            self._groups[name] = Group(name, members, self.cfg.ttl_secs)

    def send(self, name: str, sender: str, payload: bytes) -> int:
        if not self.cfg.enabled:
            raise ControlError("disabled", "groups are not enabled")
        if len(payload) > self.cfg.max_payload:
            raise ControlError(
                "payload_too_large",
                f"payload exceeds the {self.cfg.max_payload} byte limit",
            )
        with self._lock:
            group = self._groups.get(name)
            if group is None:
                raise ControlError("not_found", "no such group")
            if sender not in group.members:
                # A dedicated code, so that the answer does not reveal whether
                # the group exists to somebody who is not a member.
                raise ControlError(
                    "not_a_member", "sender is not a member of the group"
                )
            group.log.append((time.time(), sender, payload))
            return len(group.log)

    def pull(self, name: str, member: str) -> list[bytes]:
        if not self.cfg.enabled:
            raise ControlError("disabled", "groups are not enabled")
        with self._lock:
            group = self._groups.get(name)
            if group is None:
                raise ControlError("not_found", "no such group")
            if member not in group.members:
                raise ControlError(
                    "not_a_member", "member is not a member of the group"
                )
            start = group.cursors.get(member, 0)
            out = [payload for _, _, payload in group.log[start:]]
            group.cursors[member] = len(group.log)
            return out


class ControlState:
    """The L4 state behind the control plane."""

    def __init__(self, cfg: ServerConfig) -> None:
        self.offline = OfflineStore(cfg.offline)
        self.groups = Groups(cfg.group)
        self.directory_enabled = cfg.directory.enabled

    def dispatch(self, request: dict[str, Any]) -> dict[str, Any]:
        """Run one request and return the response body."""
        op = request.get("op")
        if not isinstance(op, str):
            raise ControlError("bad_request", "op is missing or not a string")
        handler = _CONTROL_OPS.get(op)
        if handler is None:
            raise ControlError("unknown_op", f"unknown op {op!r}")
        return handler(self, request)

    # -- individual operations -------------------------------------------

    def _op_offline_push(self, req: dict[str, Any]) -> dict[str, Any]:
        recipient = _need_str(req, "recipient")
        payload = _need_b64(req, "payload")
        return {"queued": self.offline.push(recipient, payload)}

    def _op_offline_pull(self, req: dict[str, Any]) -> dict[str, Any]:
        recipient = _need_str(req, "recipient")
        import base64

        return {
            "messages": [
                base64.b64encode(p).decode("ascii")
                for p in self.offline.pull(recipient)
            ]
        }

    def _op_group_create(self, req: dict[str, Any]) -> dict[str, Any]:
        name = _need_str(req, "name")
        members = req.get("members")
        if not isinstance(members, list) or not all(
            isinstance(m, str) for m in members
        ):
            raise ControlError("bad_request", "members must be a list of strings")
        self.groups.create(name, members)
        return {"name": name, "members": len(members)}

    def _op_group_send(self, req: dict[str, Any]) -> dict[str, Any]:
        name = _need_str(req, "name")
        sender = _need_str(req, "sender")
        payload = _need_b64(req, "payload")
        return {"entries": self.groups.send(name, sender, payload)}

    def _op_group_pull(self, req: dict[str, Any]) -> dict[str, Any]:
        name = _need_str(req, "name")
        member = _need_str(req, "member")
        import base64

        return {
            "messages": [
                base64.b64encode(p).decode("ascii")
                for p in self.groups.pull(name, member)
            ]
        }

    def _op_directory_lookup(self, req: dict[str, Any]) -> dict[str, Any]:
        # The identity directory records when a token hash appeared.  That is
        # pairing metadata, and the specification forbids recording pairing
        # metadata, so this operation is refused unless the operator has
        # explicitly turned the directory on.
        if not self.directory_enabled:
            raise ControlError(
                "disabled",
                "the identity directory is off: it would record pairing metadata",
            )
        raise ControlError("not_implemented", "directory lookup is not available")


_CONTROL_OPS = {
    "offline.push": ControlState._op_offline_push,
    "offline.pull": ControlState._op_offline_pull,
    "group.create": ControlState._op_group_create,
    "group.send": ControlState._op_group_send,
    "group.pull": ControlState._op_group_pull,
    "directory.lookup": ControlState._op_directory_lookup,
}


def _need_str(req: dict[str, Any], key: str) -> str:
    value = req.get(key)
    if not isinstance(value, str) or not value:
        raise ControlError("bad_request", f"{key} must be a non-empty string")
    return value


def _need_b64(req: dict[str, Any], key: str) -> bytes:
    import base64

    value = req.get(key)
    if not isinstance(value, str):
        raise ControlError("bad_request", f"{key} must be a base64 string")
    try:
        return base64.b64decode(value, validate=True)
    except Exception as exc:
        raise ControlError("bad_request", f"{key} is not valid base64") from exc


CONTROL_MAX_LINE = 2 * 1024 * 1024
CONTROL_MAX_CONNECTIONS = 16
CONTROL_READ_TIMEOUT = 30.0
CONTROL_MAX_AUTH_FAILURES = 5


def _constant_time_eq(a: bytes, b: bytes) -> bool:
    """Compare two secrets without leaking their length through timing.

    The length difference is folded into the accumulator instead of
    short-circuiting the loop: a comparison that stops early tells the
    attacker where the first difference is.
    """
    n = max(len(a), len(b))
    acc = 0
    for i in range(n):
        x = a[i] if i < len(a) else 0
        y = b[i] if i < len(b) else 0
        acc |= x ^ y
    acc |= 1 if len(a) != len(b) else 0
    return acc == 0


class ControlServer:
    """The JSON Lines control plane.

    It listens on its own port, it does not listen at all unless a token was
    given, and it refuses to start without one.  A control port that is open
    and unauthenticated would let anybody read the offline store.
    """

    def __init__(self, cfg: ServerConfig) -> None:
        self.cfg = cfg
        self.state = ControlState(cfg)
        self._sock: socket.socket | None = None
        self._stop = threading.Event()
        self._conns = 0
        self._conns_lock = threading.Lock()

    def bind(self) -> socket.socket:
        assert self.cfg.control_listen is not None
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(self.cfg.control_listen)
        sock.listen(16)
        self._sock = sock
        return sock

    def serve_forever(self) -> None:
        assert self._sock is not None
        self._sock.settimeout(0.5)
        while not self._stop.is_set():
            try:
                conn, peer = self._sock.accept()
            except TimeoutError:
                continue
            except OSError:
                if self._stop.is_set():
                    break
                time.sleep(0.05)
                continue
            with self._conns_lock:
                if self._conns >= CONTROL_MAX_CONNECTIONS:
                    with contextlib.suppress(OSError):
                        conn.close()
                    continue
                self._conns += 1
            threading.Thread(
                target=self._serve_one, args=(conn, peer), daemon=True
            ).start()

    def _serve_one(self, conn: socket.socket, peer: Any) -> None:
        try:
            self._handle(conn)
        except Exception:
            log.exception("control plane error while serving %s", peer)
        finally:
            with self._conns_lock:
                self._conns -= 1
            with contextlib.suppress(OSError):
                conn.close()

    def _handle(self, conn: socket.socket) -> None:
        conn.settimeout(CONTROL_READ_TIMEOUT)
        buffer = b""
        authenticated = self.cfg.control_token is None
        failures = 0

        while True:
            try:
                chunk = conn.recv(4096)
            except TimeoutError:
                return
            except OSError:
                return
            if not chunk:
                return
            buffer += chunk
            if len(buffer) > CONTROL_MAX_LINE:
                self._send(conn, {"ok": False, "error_code": "line_too_long"})
                return

            while b"\n" in buffer:
                line, buffer = buffer.split(b"\n", 1)
                if not line.strip():
                    continue
                try:
                    request = json.loads(line.decode("utf-8"))
                except (UnicodeDecodeError, ValueError):
                    self._send(conn, {"ok": False, "error_code": "bad_json"})
                    continue
                if not isinstance(request, dict):
                    self._send(conn, {"ok": False, "error_code": "bad_request"})
                    continue

                if not authenticated:
                    token = request.get("token")
                    candidate = token.encode("utf-8") if isinstance(token, str) else b""
                    if not _constant_time_eq(candidate, self.cfg.control_token or b""):
                        failures += 1
                        self._send(
                            conn,
                            {"ok": False, "error_code": "unauthorized"},
                        )
                        if failures >= CONTROL_MAX_AUTH_FAILURES:
                            return
                        continue
                    authenticated = True
                    self._send(conn, {"ok": True, "data": {"authenticated": True}})
                    continue

                try:
                    data = self.state.dispatch(request)
                except ControlError as exc:
                    self._send(
                        conn,
                        {"ok": False, "error_code": exc.code, "error": exc.message},
                    )
                    continue
                self._send(conn, {"ok": True, "data": data})

    @staticmethod
    def _send(conn: socket.socket, body: dict[str, Any]) -> None:
        line = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8"
        )
        try:
            conn.sendall(line + b"\n")
        except OSError:
            pass

    def stop(self) -> None:
        self._stop.set()
        if self._sock is not None:
            with contextlib.suppress(OSError):
                self._sock.close()


def _parse_addr(text: str, what: str) -> tuple[str, int]:
    host, _, port = text.rpartition(":")
    if not host or not port:
        raise ServerConfigError(f"{what} must be given as host:port")
    try:
        port_number = int(port)
    except ValueError:
        raise ServerConfigError(f"{what} port must be a number") from None
    if not (0 < port_number < 65536):
        raise ServerConfigError(f"{what} port must be in 1..65535")
    return (host, port_number)


def _read_token(value: str) -> bytes:
    """Read the control-plane token.

    A value that names an existing file is read from that file; anything else
    is taken literally.  The token never comes from the environment or from
    the command line in a way that would land in the process list when a file
    is available.
    """
    if os.path.isfile(value):
        with open(value, "rb") as handle:
            return handle.read().strip()
    return value.encode("utf-8")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=version.PROGRAM_NAME,
        description=f"ZT-DSP {version.DOCS_VERSION} server: pair peers, then relay bytes.",
    )
    parser.add_argument(
        "--version",
        action="version",
        version=version.format_version(),
    )
    parser.add_argument(
        "--profile",
        choices=sorted(PROFILES),
        default="beginner",
        help="Appendix B.2 profile (default: beginner)",
    )
    parser.add_argument(
        "--listen",
        default="127.0.0.1:9931",
        help="address of the peer-facing port, host:port (default: 127.0.0.1:9931)",
    )
    parser.add_argument(
        "--control",
        default=None,
        help="address of the L4 control plane, host:port; off unless given",
    )
    parser.add_argument(
        "--control-token",
        default=None,
        help="control-plane token, or the path of a file holding it",
    )
    parser.add_argument(
        "--log-level",
        default="warning",
        choices=("debug", "info", "warning", "error"),
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logging.basicConfig(
        level=getattr(logging, args.log_level.upper()),
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )

    listen = _parse_addr(args.listen, "--listen")
    control_listen = _parse_addr(args.control, "--control") if args.control else None
    control_token = _read_token(args.control_token) if args.control_token else None

    cfg = config_for_profile(
        args.profile,
        listen,
        control_listen=control_listen,
        control_token=control_token,
    )
    try:
        cfg.validate()
    except ServerConfigError as exc:
        print(f"configuration error: {exc}", file=sys.stderr)
        return 2

    print(f"ZT-DSP {version.format_version()}", file=sys.stderr)
    print(
        "profile {} ({})".format(cfg.profile, PROFILES[cfg.profile]["label"]),
        file=sys.stderr,
    )
    if cfg.directory.enabled:
        print(
            "WARNING: the identity directory is on. It records when a token hash "
            "appeared, which is pairing metadata the specification forbids.",
            file=sys.stderr,
        )

    server = Server(cfg)
    try:
        server.bind()
    except OSError as exc:
        print(f"cannot listen on {listen[0]}:{listen[1]}: {exc}", file=sys.stderr)
        return 2

    control: ControlServer | None = None
    if control_listen is not None:
        control = ControlServer(cfg)
        try:
            control.bind()
        except OSError as exc:
            print(
                f"cannot listen on {control_listen[0]}:{control_listen[1]}: {exc}",
                file=sys.stderr,
            )
            return 2
        threading.Thread(target=control.serve_forever, daemon=True).start()
        print(
            f"control plane on {control_listen[0]}:{control_listen[1]}",
            file=sys.stderr,
        )

    print(f"listening on {listen[0]}:{listen[1]}", file=sys.stderr)

    def _shutdown(_signum: int, _frame: Any) -> None:
        server.stop()
        if control is not None:
            control.stop()

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, _shutdown)
        except (ValueError, OSError):
            pass

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.stop()
        if control is not None:
            control.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
