# hako-cloudserver

Standalone HakoDB cloud-sync hub
with admin console: room-scoped document sync over WebSocket, group/user
credential stores, and an HTTP admin API + UI.

## Builtin server plane

The room registry (`__hako_rooms`, plus the pre-rebrand `__firelite_rooms`
alias) and the credential stores
(`__users`, `__groups`) are declared sync-excluded in the server config —
they never replicate to clients, by exact name rather than by naming
convention.

## Compatibility

| hako-cloudserver | hako core |
|---|---|
| 0.1.1 | `cloud_sync` branch (pre-crates.io) |

## Build & test

```sh
cargo build --release
cargo test
```

The `hakodb` dependency tracks the core `cloud_sync` branch until the
first crates.io release, then pins to `version = "0.8"`.

## Run

A room-agnostic sync hub plus an admin web console (no JS framework —
embedded HTML + SSE), in one process, two ports.

```bash
hako-cloudserver \
  --db-path /var/lib/hako-cloud/db \
  --admin-bind 127.0.0.1:8081 \
  --sync-bind 0.0.0.0:8080
```

Configuration layers (lowest wins last): compiled defaults <
`./hako-cloud.toml` (auto-loaded when present; legacy
`./firelite-cloud.toml` still honored) < `HK_*` env
(`HK_DB_PATH`, `HK_ADMIN_BIND`, `HK_SYNC_BIND`, `HK_LOG_LEVEL`,
`HK_SECURE_COOKIES=1`, `HK_SERVER_ID`, `HK_SYNC_TOKEN`, `HK_TLS_CERT`,
`HK_TLS_KEY`; pre-rebrand `FL_*` spellings still work as fallback) <
CLI flags. A minimal TOML:

```toml
db_path = "/var/lib/hako-cloud/db"
admin_bind = "127.0.0.1:8081"
sync_bind = "0.0.0.0:8080"
log_level = "info"
```

### First run

Open the console (`http://127.0.0.1:8081`). With no admin account present,
only the setup wizard is reachable — create the initial administrator and
the wizard disables itself permanently. Roles: `viewer` (read),
`operator` (read + write data), `admin` (everything incl. users, groups,
maintenance).

### Groups: open by default, registered when you mean it

Rooms accept anonymous peers unless you create a **group** for the room
name (Groups view): `registered` mode issues an API key (shown once —
only its hash persists) that peers present at handshake; an optional
member list pins allowed `client_id`s. Absent groups stay open, so
existing deployments keep working untouched. Rotating a key is one click;
switching a group back to `open` destroys the stored hash.

### Topology advice: hub 1–2 peers, mesh the rest

Point one (two for redundancy) always-on peer per site at the cloud hub;
let the remaining devices sync peer-to-peer over net_sync locally. The
hubs converge through the server; LAN traffic never leaves the site.
The dashboard's room/peer view shows whether the topology holds.

### TLS and services

Direct TLS for the admin console: `--tls-cert fullchain.pem --tls-key
privkey.pem` (session cookies flip `Secure` automatically; HSTS follows).
The sync plane stays `ws://` behind a reverse proxy, or terminate there
too — both are documented deployments. Refusing to start with only half
the TLS pair is deliberate (fail-closed).

- Linux: `contrib/hako-cloudserver.service` (hardened
  systemd unit — `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`,
  `ReadWritePaths` scoped to the DB dir).
- Windows: `--install-service [--service-name NAME]` (requires absolute
  `--db-path`; auto-starts at boot), `--uninstall-service`; Stop from the
  SCM drains cleanly. NSSM remains a valid fallback.
- Never bind the console to `0.0.0.0` without TLS — the server logs a loud
  warning when it sees that combination.
