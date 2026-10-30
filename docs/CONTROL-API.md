# Control Plane API (L4 extension)

> **This is not part of the isostress specification.** The specification defines only the peer-to-peer
> protocol. Offline delivery, multi-party groups and identity directories are listed in
> the repository README as *intended* server capabilities but are not specified. This
> document is the contract this implementation offers for them.

The control plane listens on a **separate port** from the data plane. It never touches
the isostress wire format. With it disabled (no `--control`), the server is a pure relay.

## Transport

- TCP, one JSON object per line (`\n` terminated).
- Request and response are each one line.
- Every `payload` field is **hex-encoded opaque bytes**. The server does not decrypt,
  does not parse, and does not inspect them.
- Every token hash is 64 lowercase/uppercase hex characters (32 bytes).

## Authentication

```
{"op":"auth","token":"<shared secret>"}
```

Rules:

0. **The token is mandatory.** The control plane refuses to start without
   `--control-token`. There is no unauthenticated mode and no loopback exemption.
1. The first request on every connection must be `auth`; every other operation then
   returns `{"ok":false,...}` until it succeeds.
2. Five failed attempts on one connection close it. Comparison is constant-time.
3. The control connection has its own concurrency cap, idle timeout and line-length cap.




Treat the token as a full administrative credential: it can read group logs and drain
offline queues.

## Response shape

```json
{"ok": true, "data": { … }}
{"ok": false, "error_code": "not_found", "error": "human-readable reason"}
```

`data` is omitted on failure. `error` and `error_code` are omitted on success.
`error_code` is the stable machine-readable value; `error` is prose and may change.
Requests carrying unknown fields are rejected outright.

The codes are `bad_request`, `disabled`, `payload_too_large`, `not_found`,
`not_a_member`, `already_exists`, `group_full`, `group_limit`, `recipient_limit`
and `internal`.

## Operations

### `ping`

```json
→ {"op":"ping"}
← {"ok":true,"data":{"pong":true}}
```

### `stats`

```json
← {"ok":true,"data":{
     "accepted": 12, "paired": 5, "active_pairs": 2,
     "rejected_silent": 3, "lobby_timeout": 2,
     "bytes_in": 40960, "bytes_out": 40960,
     "offline_depth": 0, "offline_stored": 7, "group_broadcasts": 1}}
```

All values are aggregate counters. There are no identifiers of any kind.

### `offline_push`

Queue an envelope for a recipient that is not currently connected.

```json
→ {"op":"offline_push","dst":"<64-hex>","payload":"<hex>"}
← {"ok":true,"data":{"depth":1}}
```

- Envelope size is capped by `offline.max_payload` per profile.
- When a recipient's queue is full, the **oldest** envelope is evicted (FIFO), so a
  newly arriving message is never lost to a stale one.
- Envelopes expire after `offline.ttl_secs`; the reaper scans them every
  `sweep_storage_secs` (60 s by default), not every 5 seconds — 5 s is the lobby
  sweep cadence.

### `offline_pull`

```json
→ {"op":"offline_pull","dst":"<64-hex>","max":10}
← {"ok":true,"data":{"items":["<hex>","<hex>"]}}
```

Pulling **removes** the envelopes. There is no acknowledgement protocol; a crash between
pull and delivery loses the envelope. Callers that need at-least-once semantics must
keep their own copy.

### `group_create`

```json
→ {"op":"group_create","name":"g1","owner":"<64-hex>"}
← {"ok":true,"data":{"created":"g1"}}
```

Group names are operator-chosen strings. They carry no identity information; members are
represented only by token hashes.

### `group_join`

```json
→ {"op":"group_join","name":"g1","member":"<64-hex>"}
← {"ok":true,"data":{"members":2}}
```

### `group_send`

```json
→ {"op":"group_send","name":"g1","sender":"<64-hex>","payload":"<hex>"}
← {"ok":true,"data":{"cursor":1}}
```

Appends to the group log. The sender must be a member. The log is capped at
`max_members × 64` entries and evicts oldest-first.

### `group_pull`

```json
→ {"op":"group_pull","name":"g1","member":"<64-hex>"}
← {"ok":true,"data":{"items":[{"cursor":1,"payload":"<hex>"}]}}
```

Returns entries after the member's cursor and advances it. Pulling twice returns the
second call empty.

### `dir_lookup`

```json
→ {"op":"dir_lookup","hash":"<64-hex>"}
← {"ok":true,"data":{"alive":true,"ago_secs":120}}
```

**Disabled by default in every profile.** The directory answers only "has this hash been
seen recently", never "who is this". Timings are floored to whole minutes.

It is off by default because it records exactly what specification line 979 forbids: the
appearance of a token hash, i.e. pairing metadata. Enabling it (`directory.enabled`) is an
operator decision that deviates from that requirement; the server logs a warning at
startup when it is on. Access control is the control-plane token — there is no separate
per-caller opt-in.

## Limits and failure modes

| Condition | Response |
|---|---|
| Malformed JSON | `{"ok":false,"error_code":"bad_request","error":"请求解析失败…"}`; connection stays open |
| Unknown `op` | Parse error (serde rejects unknown variants) |
| Unknown field in an object | Rejected outright (`deny_unknown_fields`) |
| Feature disabled by profile | `{"ok":false,"error_code":"disabled","error":"该功能在当前档位被关闭"}` |
| Oversized payload | `{"ok":false,"error_code":"payload_too_large","error":"载荷超过上限 N 字节"}` |
| Non-member group operation | `{"ok":false,"error_code":"not_a_member","error":"不是该群成员"}` — deliberately not `not_found`, so a probe cannot learn whether the group exists |
