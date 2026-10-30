# Threat Model

Scope: the operator of a isostress server, and anyone who can observe or influence its
traffic. Clients and the TCA are out of scope.

## 1. What the operator cannot do

These follow structurally from the design, not from policy:

| Capability | Why not |
|---|---|
| Read message content | The server never holds a session key; phases 2–4 are opaque bytes. This is structural, not policy. |
| Forge or replay a session | It cannot produce a valid phase-3 tag without the key. |
| Recover plaintext from a stored envelope | Offline envelopes are the client's ciphertext; the server has no key for them. |
| Learn the transcript | It never hashes or re-encodes the frames. |
| Decide *content* by padding analysis | Padding and timing shaping are client responsibilities (Appendix G); the server does not add or strip it. |

| Capability it **does** have | Consequence |
|---|---|
| Learn that two token hashes were paired, and when | This is unavoidable for a relay: it must read the hashes to route. Mitigation is retention discipline, not denial. |
| Link a source IP to a token hash | The connection's IP is visible at accept time. Use of an anonymity network in front of the server is a client decision. |

## 2. What the operator *can* do

Consistent with specification line 972, the operator's ceiling is availability:

| Capability | Mitigation available to clients |
|---|---|
| Refuse to pair | Retry against another server |
| Delay delivery | Client-side timeouts |
| Drop a specific session's traffic | Rotate to a new session |
| Correlate timing of a single session | Traffic padding (Appendix G, client responsibility) |

## 3. What an on-path observer learns

| Observation | Meaning |
|---|---|
| Connection exists | Someone is using this server |
| Frame sizes | Roughly the padding profile (client-side; Appendix G) |
| Timing | Session start/stop, message cadence |
| Server's silence | Nothing — a rejected handshake is indistinguishable from a closed port |

The silent-drop contract exists specifically so that a probe cannot use timing or reset
behaviour to enumerate valid token hashes.

## 4. Attacks the server mitigates

| Attack | Defence |
|---|---|
| Token-hash enumeration by probing | Silent drop: no bytes, no reset, identical timing |
| Memory exhaustion via forged lengths | All length fields checked before allocation |
| Slowloris | The whole phase-1 frame shares one 15 s deadline |
| Impersonation by hash collision | Mutual pairing predicate — both directions must match |
| Connection flooding | Per-IP cap, global concurrent cap, both silent on rejection |
| Presence harvesting | Directory off by default; coarse timestamps when on |

## 5. What the server does **not** protect against

- **Malicious clients.** A client can flood the lobby with valid-shaped frames. Rate
  limits bound the damage but cannot eliminate it.
- **Server collusion.** Two servers, or a server plus an ISP, can correlate more than
  either alone.
- **Global passive adversary.** Traffic analysis at scale is a client concern; the server
  contributes no additional data but also cannot prevent it.
- **Compromised operator.** Anyone with root on the box can observe the same metadata
  the server itself sees. Mitigations are operational: minimised logging (already
  enforced), ephemeral instances, and not co-locating data and control planes.

## 6. Data the server retains

By default:

| Data | Retention |
|---|---|
| Connection metrics | In memory, aggregate only |
| Offline envelopes | `offline.ttl_secs` (1 day beginner, 7 days advanced, disabled for high-anonymity) |
| Presence records | Only if the operator opts in; otherwise nothing is recorded |
| Group log | In memory, capped, lost on restart |
| Directory rows | In memory, swept after `directory.ttl_secs` (default 1 h); the feature is off by default |
| Logs | stderr; no token hashes, no session ids, no payloads. Peer socket addresses appear at `debug` only, never at `info` |

Nothing is written to disk by the server itself.
