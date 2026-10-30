# Protocol Conformance

Maps each normative requirement the server must satisfy onto the code that satisfies it.
Every row should be checkable by reading the cited file.

Reference: **isostress 1.0.0**, `cn/spec/isostress-1.0.0-总指导.md`
(English mirror: `en/spec/isostress-1.0.0-Master_Guide.md`).

---

## 1. Server role

| Requirement | Implementation |
|---|---|
| Server only forwards envelopes; cannot decrypt (§1.3, Principle Four) | After pairing, `relay_pair` moves bytes and never inspects payload |
| Server reads only phase-1 routing material | `frame::read_phase1` extracts the two 32-byte token hashes |
| Must not log pairing information (§5.1) | No token hash or session id is ever formatted into a log line. Peer socket addresses appear **only at `debug`**, never at `info`; see `docs/THREAT-MODEL.md` §6. **Conclusion:** nothing that can reconstruct a pairing is logged, since the hashes and session ids that would be needed never reach a log line. Whether a bare socket address counts as pairing information is a question the text leaves open; §10.1 records it as a known deviation, together with the one-line-per-site fix. |
| Worst case is availability, not confidentiality (§5.1) | Structural: no key material reaches the server. **Conclusion:** met for the data plane, because nothing the server holds would disclose a payload. It does **not** follow for the L4 inbox: `offline_pull` does not prove ownership of the destination, so the worst case there includes metadata and queue integrity. §10.2 states exactly what that does and does not expose. |

## 2. Appendix F.0 — framing

| Requirement | Implementation |
|---|---|
| TCP byte stream, no message boundaries | `TcpStream` + length-prefixed reads |
| Network byte order | `u32::from_be_bytes` / `u64::from_be_bytes` |
| Variable fields are `[length:4B][payload]` | `frame::read_phase1` |
| No magic, no frame-type field, no frame CRC | The parser assumes position, as specified |
| Validate length before reading the payload | Each bound is checked before `vec![0u8; n]` |

## 3. Appendix F.1 — phase-1 frame

```
[param_len:4][param_json:param_len][identity_len:4][identity_blob][nonce_len:4][nonce]
```

| Check | Code |
|---|---|
| `param_len <= 4096` | `FrameError::LengthExceeded` |
| `identity_len <= 8192` | `FrameError::LengthExceeded` |
| `identity_len >= 64` | `FrameError::IdentityTooShort` |
| `nonce_len <= 512` | `FrameError::LengthExceeded` |
| param JSON is valid UTF-8 and parses as the 7-item set | `frame::validate_params` via `ParamSet` |
| `aead` / `hash` non-empty | `validate_params` |
| `key_len ∈ {128, 192, 256}` | `validate_params` |
| `random_len` a positive multiple of 8, `<= nonce cap` | `validate_params` |
| `random_rate > 0`, `window > 0` | `validate_params` |
| `nonce_len × 8 == random_len` | `FrameError::NonceLengthMismatch` |
| `module_flags` depth ≤ 32, nodes ≤ 1024 (F.6) | `check_flags_depth` |
| `src != dst` | `FrameError::SelfAddressed` |
| Default frame length 228 bytes | asserted in `frame::tests::parses_default_phase1` |

The raw bytes of the phase-1 frame are preserved verbatim in `Phase1Frame::raw` and
replayed to the peer without re-encoding, because those bytes feed the phase-3
transcript.

## 4. Appendix F.2 — strict silent drop

| # | Element | Implementation |
|---|---|---|
| 1 | Send no bytes | `silent::drain_silently` never writes |
| 2 | Do not actively close | Socket is drained until the deadline, then dropped |
| 3 | Drain at 8192-byte chunks until the deadline | `silent::DRAIN_CHUNK` |
| 4 | Diagnostics to local stderr only | `tracing` initialised with `std::io::stderr` |
| 5 | Indistinguishable from a closed port | Every rejection path drains for `handshake_timeout_secs` — one constant, not per-path values |

Trigger points: parse failure, oversized length, short identity, nonce over cap,
invalid parameter set, nonce/`random_len` mismatch, self-addressed identity, and lobby
timeout.

## 5. Appendix F.6 — hard limits

| Field | Cap | Code |
|---|---|---|
| Parameter JSON | 4096 | `frame::PARAM_JSON_MAX` |
| Identity declaration | 8192 | `frame::IDENTITY_MAX` |
| Nonce | 512 | `frame::NONCE_MAX` |
| `module_flags` depth | 32 | `frame::check_flags_depth` |
| `module_flags` nodes | 1024 | `frame::check_flags_depth` |

The ciphertext cap (1 MiB) applies to the phase-4 payload, which this server never
parses; it is enforced by the clients.

## 6. §4.2 server processing flow

| Step | Spec text | Implementation |
|---|---|---|
| 1. Parameter check | Reject on shape mismatch | `frame::validate_params` |
| 2. Token verification | TCA signature + CRL | **Not implemented — TCA/CRL role** |
| 3. Lobby matching | "Look for a resident holding H(Token_A)" | `lobby::Lobby::register` |
| 4. Lobby management | 30 s auto-clear, no broadcast | `lobby_timeout_secs`; `Lobby::withdraw`, `Lobby::sweep` |

Pairing predicate:

```
newcomer.src == waiter.dst   &&   newcomer.dst == waiter.src
```

Covered by `lobby::tests::mutual_match_pairs_both`, `same_target_without_reverse_match_does_not_pair`,
`impersonation_rejected`.

## 7. Appendix B.2 — profiles

| Profile | Concurrent | Per-IP | Offline | Groups | Directory | Idle timeout |
|---|---|---|---|---|---|---|
| `beginner` | 2000 | 32 | 1 day | 32 members | **off** | none |
| `advanced` | 20000 | 128 | 7 days | 256 members | **off** | none |
| `high-anonymity` | 500 | 4 | **off** | **off** | **off** | 300 s |

The identity directory is off in **every** profile: it is a record of when a token hash
appeared, which §5.1 forbids. Enabling it is an explicit operator choice that
deviates from that requirement, and the server logs a warning at startup when it is on.

## 8. Deliberately out of scope

| Item | Reason |
|---|---|
| TCA token issuance | Separate authority with its own keys and audit trail |
| CRL distribution | Same |
| Phase-2/3/4 cryptographic processing | The server must not hold session keys |
| Persistence of offline envelopes | In-memory is the safe default |
| Authenticated client identity at the server | Would require knowing who is who |

## 9. Where the specification is silent

Implemented as documented extensions that never affect the data plane:

- offline envelope queue (`l4::OfflineStore`)
- group log with per-member cursors (`l4::Groups`)
- identity directory (`l4::Directory`), off by default

See [`CONTROL-API.md`](CONTROL-API.md).

## 10. Known deviations

Two places where the implementation cannot satisfy a requirement as literally
written, or where the requirement does not decide the case. Stated here rather
than left implied, so a reviewer can accept them or send them back.

### 10.1 `debug`-level lines carry the peer socket address

The row in §1 says peer addresses appear only at `debug`. The specification
forbids logging *pairing information* (§5.1). Whether an IP address and
port count as pairing information is not something the text decides.

Our reading is that it does not. An address says which endpoint opened a
socket; it does not say who they are trying to reach, and the token hashes and
session ids that would be needed to reconstruct a pairing are never logged at
any level. The address is what tells an operator where a connection was
rejected.

If the document owner reads it the other way, the fix is to drop the `peer`
field from the six `debug!` sites and keep the error cause. That is one line
per site and touches nothing on the data plane.

### 10.2 `offline_pull` does not prove ownership of the destination

`control::State::handle` serves `OfflinePull { dst, max }` to any caller that
presents the control-plane token, without checking that the caller owns `dst`.
The token guards the whole control plane rather than each mailbox within it.

What that does and does not expose:

- **Not confidentiality.** The payload is opaque. The server holds no key and
  never has, so what a caller receives is the ciphertext the sender left,
  not plaintext.
- **Metadata.** Which destinations are receiving mail, how much, and when.
  That is the pairing-adjacent information §5.1 warns about.
- **Integrity and availability.** `pull` removes what it takes (`pop_front`),
  so a caller can destroy mail that was not addressed to them; `push` accepts
  bytes for any destination, so a caller can inject.

The asymmetry is the point. Group pulls do check membership — `Groups::pull`
returns `NotAMember` without revealing whether the group exists — and the
offline queue has no equivalent check.

There are two ways to close it, and they are not equivalent:

- bind each pull to the destination's own credential. The control plane then
  learns who is asking, which is the same disclosure §5.1 warns about;
- or leave it and state plainly that the control plane is a
  fully-trusted-operator interface, not a multi-tenant one.

The second is what this implementation does, which is why the control plane is
off by default, bound to loopback when enabled, and refuses to start without a
token.
