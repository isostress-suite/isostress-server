# Deployment

## 1. Build

```bash
cargo build --release
```

The result is a single stripped binary (~2 MB) with no runtime dependencies.

Cross-compilation from an aarch64 host to x86_64:

```bash
rustup target add x86_64-unknown-linux-gnu
# a linker is required:
#   Debian/Ubuntu: apt install gcc-x86-64-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

`.cargo/config.toml` wires `x86_64-linux-gnu-gcc` as the linker for that target.

## 2. Minimal run

```bash
./isostress-server --listen 0.0.0.0:7443
```

Data plane only. No control plane, no storage, no state.

## 3. Production run

```bash
./isostress-server \
  --listen 0.0.0.0:7443 \
  --control 127.0.0.1:<port> \
  --control-token "$(head -c 32 /dev/urandom | base64)" \
  --profile advanced \
  --config /etc/isostress/server.json
```

`<port>` is chosen by the deployment or the implementation default; the example value is not a convention. The same applies to the unit file and the JSON below.

The token is mandatory whenever `--control` is given; the process exits immediately
otherwise. Keep the control plane on loopback unless you have a separate authenticated
network.

**Reverse proxies:** the per-IP limit uses the socket peer address. Behind a proxy every
connection appears to come from the proxy, so the limit becomes global. Either terminate
connections directly, or raise `max_per_ip` deliberately.

## 4. systemd unit

```ini
[Unit]
Description=isostress server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=isostress
Group=isostress
ExecStart=/usr/local/bin/isostress-server \
    --listen 0.0.0.0:7443 \
    --control 127.0.0.1:<port> \
    --profile advanced
Environment=ISOSTRESS_CONTROL_TOKEN=<long random secret>

# Hardening
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallFilter=@system-service
SystemCallArchitectures=native

# The server writes nothing to disk
ReadWritePaths=/dev/null

Restart=on-failure
RestartSec=2
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

## 5. Configuration file

`--config` takes a JSON file. **Every field must be present.** The loader does not
merge a partial file with the profile defaults, so one missing field is a startup
error, not a fallback:

```
Error: Parse { path: "...", source: Error("missing field `max_recipients`", ...) }
```

Generate a complete file with `--dump-config`, then edit what you need. The example
below is the output of

```
--dump-config --profile advanced --control 127.0.0.1:7444
```

field for field. The `--control` flag is part of the recipe: without it the dump
prints `"control_listen": null`, because the control plane is off by default and
`--dump-config` reports the configuration as it would be started. The example
also formats `directory` on one line where the dump spreads it over four; the
values are identical.

```json
{
  "listen": "0.0.0.0:7443",
  "control_listen": "127.0.0.1:<port>",
  "profile": "advanced",
  "handshake_timeout_secs": 15,
  "lobby_timeout_secs": 30,
  "idle_timeout_secs": 0,
  "max_connections": 20000,
  "max_per_ip": 128,
  "buffer_bytes": 32768,
  "offline": {
    "enabled": true,
    "max_payload": 262144,
    "max_per_recipient": 100,
    "max_recipients": 100000,
    "ttl_secs": 604800
  },
  "group": {
    "enabled": true,
    "max_members": 256,
    "max_groups": 50000,
    "max_payload": 262144
  },
  "directory": { "enabled": false, "ttl_secs": 3600 },
  "control_read_timeout_secs": 30,
  "control_write_timeout_secs": 30,
  "relay_write_timeout_secs": 30,
  "accept_backoff_ms": 50,
  "sweep_lobby_secs": 5,
  "sweep_storage_secs": 60
}
```

Note that `--profile` is ignored when `--config` is also given: the file's
`profile` field wins for everything the file specifies, and the flag is not
reported as unused.


Print the effective configuration with `--dump-config`.

## 6. Tuning

| Symptom | Knob |
|---|---|
| Too many lobby timeouts | `lobby_timeout_secs` (but raise only with a reason — it is a privacy surface) |
| Memory growth | lower `max_connections`, `offline.max_per_recipient`, `group.max_members` |
| SYN backlog / file descriptors | `LimitNOFILE`, kernel `somaxconn` |
| NAT peers failing | This is what the relay exists for; check `max_per_ip` |

## 7. Observability

The server logs to **stderr** only. `RUST_LOG=info` gives lifecycle events; `debug` adds
per-connection rejections. There is no metrics endpoint in the data plane; use the
control plane's `stats` operation.

What the logs never contain: token hashes, session identifiers, payload bytes, peer IPs
at `info` level.

## 8. Upgrades

The server holds no durable state, so the upgrade procedure is: stop, replace the
binary, start. In-flight sessions drop; clients reconnect. Offline envelopes and group
logs are lost — drain them via the control plane first if that matters.
