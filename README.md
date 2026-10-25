# ZT-DSP Server

> **Status: implementation landed.** The data plane (frame relay, lobby pairing,
> silent drop) is implemented in Rust and verified against the ZT-DSP 1.0.1 wire
> specification. The L4 extensions (offline delivery, groups, identity directory)
> are implemented on a **separate control plane** that does not touch the wire format.
> The same server role also ships as a Python implementation in
> [`zt_dsp_sv_py/`](zt_dsp_sv_py/README.md).

| | |
|---|---|
| **Protocol** | [ZT-DSP 1.0.1](https://github.com/ZT-DSP-Suite/ZT-DSP-docs) |
| **Language** | Rust (edition 2021, MSRV 1.80); the server role also ships in Python (>= 3.10) under `zt_dsp_sv_py/` |
| **Binaries** | single-file, static, `aarch64` + `x86_64` |
| **Runtime deps** | none (no database, no message broker, no TLS terminator) |
| **License** | AGPL-3.0-or-later **or** commercial — see below |

## What this is

A relay server for ZT-DSP peers. It introduces one role: **front desk**.

```
Peer A ──┐                                  ┌── Peer B
         ├──── ZT-DSP server (this repo) ────┤
Peer C ──┘      pairing + byte relay          └── Peer D
               server never decrypts
```

The server registers, pairs, and forwards. It never opens the envelope:
phase-2 public keys, phase-3 confirmation tags, and phase-4 ciphertext are all
opaque to it. The only things it reads are the two token hashes in the phase-1
frame (routing) and the session identifier in the phase-4 header (room lookup).

## What this is not

- **Not a CA.** Token issuance and revocation (TCA / CRL) are a separate, independent
  role. This repository does not issue or revoke identities.
- **Not a decrypting middlebox.** Any design that terminates the handshake would turn
  the server into a plaintext vantage point; that is explicitly out of scope.
- **Not persistent.** Offline envelopes and group logs live in memory and are lost on
  restart. Durable storage is a deployment decision, not a protocol one.

## Conformance

| Area | Spec | Status |
|---|---|---|
| Phase-1 frame parsing, length limits | Appendix F.1 / F.6 | implemented |
| Lobby pairing and timeout | §4.2, 30 s | implemented |
| Silent-drop contract (5 elements) | Appendix F.2 | implemented |
| Byte relay, no transcript touching | §4.2, line 254 | implemented |
| Profile defaults (three tiers) | Appendix B.2 | implemented |
| Offline / groups / directory | **not specified** | implemented as extension |

See [`docs/PROTOCOL-CONFORMANCE.md`](docs/PROTOCOL-CONFORMANCE.md) for the
section-by-section mapping, and [`docs/CONTROL-API.md`](docs/CONTROL-API.md) for the
extension.

## Quick start

```bash
cargo build --release
./target/release/zt-dsp-server --listen 0.0.0.0:7443
```

With the control plane enabled (the token is mandatory — the process refuses to start
without it):

```bash
./target/release/zt-dsp-server \
  --listen 0.0.0.0:7443 \
  --control 127.0.0.1:7444 \
  --control-token "$(head -c 32 /dev/urandom | base64)" \
  --profile advanced
```

Cross-compiling from aarch64 to x86_64:

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

See [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) for systemd units, profiles and
operational guidance.

## Documents

| Document | Contents |
|---|---|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Module map, pairing model, connection handoff |
| [`docs/PROTOCOL-CONFORMANCE.md`](docs/PROTOCOL-CONFORMANCE.md) | Spec section → code mapping |
| [`docs/CONTROL-API.md`](docs/CONTROL-API.md) | L4 control plane (offline, groups, directory) |
| [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) | What the operator can and cannot do |
| [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) | Build, run, systemd, hardening |
| [`cn/README.md`](cn/README.md) | 中文完整说明 |
| [`en/README.md`](en/README.md) | English full description |

## Repository map

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
zt_dsp_sv_py/    Python implementation of the same server role (pytest suite inside)
```

## Security

Report vulnerabilities privately — see [`SECURITY.md`](SECURITY.md).

The short version: the server cannot read message content, cannot forge session keys,
and cannot distinguish a rejected handshake from a closed port. Its worst case is
denial of service against specific sessions, which the specification already
acknowledges as the operator's ceiling.

See [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md).

## Licence

Dual-licensed:

- **Open source:** [AGPL-3.0-or-later](LICENSE) — network use counts as distribution.
- **Commercial:** see [`LICENSING.md`](LICENSING.md) for the paid alternative.

Additional terms (trademark, attribution) are in [`ADDITIONAL-TERMS.md`](ADDITIONAL-TERMS.md)
and [`TRADEMARK.md`](TRADEMARK.md).

The copyright holder reserves the right to offer the software under different terms.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Every file you add or modify must keep its
`SPDX-License-Identifier` header; contributions require CLA sign-off.
