# Contributing

Thanks for your interest. This repository is the server edition of isostress.

## Before you start

- Read the [isostress master guide](https://github.com/isostress-suite/isostress-docs). The server
  implements the wire specification; changes that contradict it will not be merged.
- Read [`docs/PROTOCOL-CONFORMANCE.md`](docs/PROTOCOL-CONFORMANCE.md) to see which
  specification sections are already covered.

## Building and testing

```bash
cargo build --release      # single static binary
cargo test                 # unit tests
python3 tests/e2e.py       # end-to-end: pairing, relay, large transfer
python3 tests/l3_l4.py     # silent drop + control plane
```

MSRV is 1.80. The `release` profile uses thin LTO and strips symbols.

Cross-compiling to x86_64 from an aarch64 host:

```bash
rustup target add x86_64-unknown-linux-gnu
apt install gcc-x86-64-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

## What we will merge

- Fixes and tests for the existing behaviour.
- New configuration profiles or operational tooling that does not change the wire format.
- Documentation corrections, especially where a statement can be traced to a
  specification line and the current text contradicts it.
- Additions to the L4 control plane, provided they stay off the data plane.

## When we review and merge

Review and merge usually happen between Friday and Sunday, UTC+8. The window is a
habit of two maintainers with other work, not a service commitment. A pull request
that waits over a weekday is waiting for a person, not rejected.

## What we will not merge

- Any change that makes the server decrypt, or hold, session material.
- Any change that adds a frame type or control frame to the isostress wire format. The format
  has no type field; adding one breaks every existing client.
- Any change that lets a probe distinguish a rejected handshake from a closed port.
- Any change that logs token hashes, session identifiers, or payload bytes.
- Any file whose copyright header or `SPDX-License-Identifier` line has been removed or
  altered.
- Changes that turn the identity directory on by default or make it return precise
  timestamps.

## Licence and CLA

Contributions require CLA sign-off. The pull request description carries one line,
`CLA-Agreed: yes`, and every commit carries a `Signed-off-by` line. Every new file must start with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (c) 2026 isostress Suite maintainers
```

Code is dual-licensed AGPL-3.0-or-later / commercial; see [`LICENSING.md`](LICENSING.md).

## Style

- `cargo fmt`, `cargo clippy -- -D warnings`.
- Comments state the constraint, not a narrative. Prefer citing the line or clause a
  behaviour exists to satisfy over explaining the history of the change.
- Errors returned to a client never reveal why a connection was refused.
