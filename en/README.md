# isostress Server

> **Status: implementation landed.** The data plane — frame parsing, lobby pairing,
> silent drop, byte relay — is implemented in Rust against isostress 1.0.1 and verified.
> The L4 extensions (offline delivery, groups, identity directory) run on a **separate
> control plane** that does not touch the wire format.

| | |
|---|---|
| Specification | isostress Master Guide 1.0.1 |
| Language | Rust (edition 2021, MSRV 1.80) |
| Artifact | single static binary, `aarch64` / `x86_64` |
| Runtime deps | none (no database, no broker, no TLS terminator) |
| Server version | `ISOSTRESS_SV-1.0.0-20261030` |
| Licence | AGPL-3.0-or-later **or** commercial |

## What this is

A relay server for isostress peers. It adds exactly one role: the **front desk**.

```
Peer A ──┐                                  ┌── Peer B
         ├──── isostress server (this repo) ────┤
Peer C ──┘      pairing + byte relay          └── Peer D
               server never decrypts
```

The server registers, pairs, forwards. It never opens the envelope: phase-2 public keys,
phase-3 confirmation tags and phase-4 ciphertext are all opaque to it. The only data it
reads is the two token hashes in the phase-1 frame (for pairing) and the session
identifier in the phase-4 header (for room routing).

## What this is not

- **Not a CA.** Token issuance and revocation (TCA / CRL) is a separate, independent role.
- **Not a decrypting middlebox.** Terminating the handshake would create a plaintext vantage
  point; that is explicitly out of scope.
- **Not persistent.** Offline envelopes and group logs live in memory and vanish on restart.

## Conformance

| Area | Spec | Status |
|---|---|---|
| Phase-1 parsing, length limits | Appendix F.1 / F.6 | implemented |
| Lobby pairing, 30 s timeout | §4.2 / Appendix H.3 | implemented |
| Silent-drop contract (five elements) | Appendix F.2 | implemented |
| Byte relay, transcript untouched | §4.2 line 254 | implemented |
| Profile defaults | Appendix B.2 | implemented |
| Offline / groups / directory | **not specified** | extension |

Section-by-section mapping: [`../docs/PROTOCOL-CONFORMANCE.md`](../docs/PROTOCOL-CONFORMANCE.md).
Extension contract: [`../docs/CONTROL-API.md`](../docs/CONTROL-API.md).

## Why a server edition exists

The client edition is a direct-connection model: both peers must be independently
reachable. That is fine among a small circle of mutually trusting people, but it does not
solve:

- a peer behind NAT that cannot port-forward;
- asynchronous delivery while the recipient is offline;
- multi-party groups, an identity directory, or cross-organisation federation;
- connecting without exposing either peer's IP address.

This repository adds those capabilities **without weakening the end-to-end properties**,
and stays interoperable with the client protocol.

## Quick start

```bash
cargo build --release
./target/release/isostress-server --listen 0.0.0.0:7443
```

With the control plane (recommended in production — always set a token):

```bash
./target/release/isostress-server \
  --listen 0.0.0.0:7443 \
  --control 127.0.0.1:<port> \
  --control-token "$(head -c 32 /dev/urandom | base64)" \
  --profile advanced
```

`<port>` is chosen by the deployment or the implementation default; the example value is not a convention.

## Profiles (Appendix B.2)

| Profile | Concurrent | Per-IP | Offline | Groups | Directory | Idle timeout |
|---|---|---|---|---|---|---|
| `beginner` | 2000 | 32 | 1 day | 32 members | **off** | none |
| `advanced` | 20000 | 128 | 7 days | 256 members | **off** | none |
| `high-anonymity` | 500 | 4 | **off** | **off** | **off** | 300 s |

The identity directory is off in **every** profile: it records when a token hash
appeared, which is pairing metadata and therefore forbidden by specification line 979.
Enabling it is an explicit operator decision that deviates from that requirement; the
server logs a warning at startup when it is on. The high-anonymity tier additionally
disables offline storage and groups.

The L4 control plane is not listening by default; when enabled, a token is **mandatory**
and the process exits without one.

## Server processing flow (§4.2)

| Step | Spec | Implementation |
|---|---|---|
| 1. Parameter check | reject on shape mismatch | length/structure validation (`src/frame.rs`) |
| 2. Token verification | TCA signature + CRL | **not implemented** — TCA/CRL role |
| 3. Lobby matching | find a resident holding H(Token_A) | `src/lobby.rs` |
| 4. Lobby management | 30 s auto-clear, no broadcast | `lobby_timeout_secs`, `Lobby::withdraw`, `Lobby::sweep` |

Pairing predicate — **mutual match**:

```
newcomer.src == waiter.dst   AND   newcomer.dst == waiter.src
```

A one-sided name collision does not pair, so an attacker cannot insert themselves into
someone else's session by guessing a target hash.

## Security boundaries

| Property | Mechanism |
|---|---|
| Server cannot read content | It holds no session key; it only forwards ciphertext |
| Pairing metadata is never logged | Spec line 978 is mandatory; logs carry aggregate counters only |
| Impersonation fails | Mutual pairing predicate |
| Probes are indistinguishable | Every anomaly goes to silent drop: no bytes, no close, no error |
| Slowloris | The whole phase-1 frame shares one 15 s deadline |
| Memory exhaustion | Every length field is checked before allocation |
| Traffic analysis | Directory timestamps floored to minutes; arbitrary lookup off by default |

Full operator capability statement: [`../docs/THREAT-MODEL.md`](../docs/THREAT-MODEL.md).

## Layout

```
src/
                 main.rs       process wiring: accept loop, connection guards, admission
                 version.rs    version contract (single source of truth)
                 frame.rs      Appendix F parsing
                 lobby.rs      pairing
                 silent.rs     silent drop
                 relay.rs      byte relay
                 config.rs     Appendix B.2 tiers
                 l4.rs         offline messages, groups, identity directory
                 control.rs    JSON Lines control plane
                 metrics.rs    counters
                 util.rs       hex and constant-time helpers
tests/           integration.rs (runs under cargo test) plus two manual scripts
docs/            architecture, conformance, control API, threat model, deployment
```

## Documents

- Protocol mapping: `docs/PROTOCOL-CONFORMANCE.md`
- Build and run: `docs/DEPLOYMENT.md`
- Module map: `docs/ARCHITECTURE.md`
- Control plane: `docs/CONTROL-API.md`

## Version applicability (many-to-many)

Guide versions and implementation versions are **not** one-to-one:

| Relation | Rule |
|---|---|
| One guide → many implementations | Shared declaration while protocol behaviour is unchanged |
| Many guides → one implementation | List each in `APPLIES_TO_DOC_VERSIONS` |
| Same number, different implementation | Equal numbers do not imply equal behaviour or compatibility |
| Compatibility judgement | Always decided by the phase-3 confirmation tag check |

Handshake frames carry **no version string**: neither the phase-1 parameter JSON nor the
phase-4 AAD contains a version or protocol identifier field.

## Licence

Dual-licensed: **AGPL-3.0-or-later** or a paid **commercial** licence.
See [LICENSING.md](../LICENSING.md) and [LICENSE](../LICENSE).
