# Architecture

This document explains how the server is built and why. For the wire format itself,
see the upstream specification; this repository implements it.

## 1. Role model

isostress has three roles. This repository implements exactly one of them.

| Role | Responsibility | In this repo |
|---|---|---|
| **TCA** | Issues and revokes identity tokens; publishes a CRL | No — separate authority |
| **Server** | Pairs peers, relays opaque bytes | **Yes** |
| **Client** | Runs the handshake, encrypts, encrypts again | No — see `isostress-client` |

The server is deliberately *not* a participant in the key exchange. It forwards phase-1
frames after reading the identity declaration, and from then on moves phases 2, 3 and 4
as opaque bytes. It never derives a session key.

```
        phase 1 (identity)      phase 2 (pubkey)      phase 3 (tag)
Peer A ─────────────────────────┐            ┌─────────────────────────  Peer B
                                │            │
                        ┌───────▼────────────▼───────┐
                        │  isostress server             │
                        │  reads: token hashes       │
                        │  reads: session id         │
                        │  never: pubkeys, tags,     │
                        │         ciphertext         │
                        └────────────────────────────┘
```

## 2. Connection lifecycle

Each accepted TCP connection moves through a small state machine.

```
        ┌──────────┐
        │ ACCEPTED │  per-IP limit, connection cap
        └────┬─────┘
             │ read phase-1 frame (15 s shared deadline)
             ▼
   ┌──────────────────┐   parse/limit failure
   │ PHASE1_PARSED    │ ──────────────────────► SILENT_DROP
   └────────┬─────────┘                          (15 s drain)
            │ lobby.register
            ▼
   ┌──────────────────┐   peer present      ┌────────────┐
   │ IN_LOBBY         │ ──────────────────► │  PAIRED    │
   └────────┬─────────┘                     └─────┬──────┘
            │ 30 s, nobody came                   │ relay until EOF
            ▼                                     ▼
        SILENT_DROP                             CLOSED
```

Note that a rejected connection is **not** closed immediately. Every rejection path —
parse failure, bad parameter set, and lobby timeout — drains for the *same* window,
`handshake_timeout_secs`, so an observer cannot distinguish one rejection cause from another
by timing. This is the silent-drop contract; see `docs/PROTOCOL-CONFORMANCE.md` §F.2.

## 3. Pairing

The waiting pool is keyed by *the hash the newcomer is looking for*. A newcomer
`(src, dst)` searches the pool for a waiter `e` satisfying:

```
e.src == newcomer.dst   AND   e.dst == newcomer.src
```

Both directions must line up. A one-sided name collision (two peers both looking for
the same third party) does **not** produce a match, which prevents an attacker from
inserting themselves into someone else's session by guessing a target hash.

Entries expire after 30 seconds (specification §4.2 / Appendix H.3). Expiry is silent:
the entry is removed, the socket is drained, nothing is broadcast.

## 4. Connection handoff

Rust's ownership model makes it awkward for two tasks to share two sockets. The server
solves this by having the **second** peer hand its socket to the **first** peer's task:

1. Peer A arrives first, parses its phase-1 frame, and waits on a channel.
2. Peer B arrives, matches A, and sends `Handoff { frame: B_phase1, payload: B_socket }`
   into A's channel, then its own task ends.
3. A wakes up holding both sockets, writes each peer's phase-1 frame to the other's
   socket, and runs the relay.

Consequences:

- The waiting pool never stores sockets, only frames and channel senders.
- No socket is ever operated by two tasks concurrently.
- If A times out while B is pairing, B's send fails and B closes; the race window is
  a fraction of a millisecond and both peers would have failed anyway.

## 5. Why L4 is a separate control plane

The `Appendix F` wire format has **no frame-type field**. A frame's meaning is derived
from its position in the handshake. Adding a control frame would therefore:

1. break interoperability with existing clients, which would silently drop an unknown
   frame (the contract requires them to);
2. weaken the silent-drop guarantee, because a server-issued frame is something a
   probe could trigger;
3. couple the extension to the protocol version.

So the L4 features — offline envelopes, group log, identity directory — live on a
**separate TCP port** speaking JSON Lines. The data plane stays byte-for-byte
specification-conformant, and is deployed with the control plane disabled by default.

The consequence is that the existing clients cannot use L4: they only speak the wire
format. Those features are for operators and for clients built against this extension.
The identity directory is additionally off in every profile, because it records when a
token hash appeared, which the specification forbids (line 979).

```
data plane   :7443   isostress wire format      (spec-conformant)
control plane:7444   JSON Lines              (extension, loopback by default)
```

## 6. Concurrency

One Tokio task per connection, plus one housekeeping task.

| Task | Purpose |
|---|---|
| accept loop | one per listener |
| connection task | parser → lobby → relay (or silence) |
| reaper | every 5 s: sweep stale lobby entries, expired envelopes, old directory rows |

State is shared through `Arc<Shared>` with a `Mutex` only around the per-IP counter,
the waiting pool, and the L4 stores. The relay path takes no locks at all.

## 7. Limits

| Limit | Value | Source |
|---|---|---|
| Phase-1 parameter JSON | 4096 B | Appendix F.6 |
| Identity blob | 8192 B | Appendix F.6 |
| Nonce | 512 B | Appendix F.6 |
| Ciphertext | 1 MiB | Appendix F.6 |
| Handshake deadline | 15 s, shared by the whole frame | Appendix H.3 |
| Lobby timeout | 30 s | Appendix H.3 |
| Drain chunk | 8192 B | Appendix F.2 |
| Relay write timeout | 30 s | local hardening: a peer that connects but never reads must not stall the task |

All length checks happen **before** the payload is allocated or read. A declared
length above the cap terminates parsing immediately.
