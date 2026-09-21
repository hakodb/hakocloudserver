# firelite-cloudserver

Standalone [FireLite](https://github.com/rizaptk/firelite) cloud-sync hub
with admin console: room-scoped document sync over WebSocket, group/user
credential stores, and an HTTP admin API + UI.

## Builtin server plane

The room registry (`__firelite_rooms`) and the credential stores
(`__users`, `__groups`) are declared sync-excluded in the server config —
they never replicate to clients, by exact name rather than by naming
convention.

## Compatibility

| firelite-cloudserver | firelite core |
|---|---|
| 0.1.1 | `cloud_sync` branch (pre-crates.io) |

## Build & test

```sh
cargo build --release
cargo test
```

The `firelite` dependency tracks the core `cloud_sync` branch until the
first crates.io release, then pins to `version = "0.8"`.

## Run

A room-agnostic sync hub plus an admin web console (no JS framework —
embedded HTML + SSE), in one process, two ports.

```bash
firelite-cloudserver \
  --db-path /var/lib/firelite-cloud/db \
  --admin-bind 127.0.0.1:8081 \
  --sync-bind 0.0.0.0:8080
```

Configuration layers (lowest wins last): compiled defaults <
`./firelite-cloud.toml` (auto-loaded when present) < `FL_*` env
(`FL_DB_PATH`, `FL_ADMIN_BIND`, `FL_SYNC_BIND`, `FL_LOG_LEVEL`,
`FL_SECURE_COOKIES=1`, `FL_SERVER_ID`, `FL_SYNC_TOKEN`, `FL_TLS_CERT`,
`FL_TLS_KEY`) < CLI flags. A minimal TOML:

```toml
db_path = "/var/lib/firelite-cloud/db"
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

- Linux: `contrib/firelite-cloudserver.service` (hardened
  systemd unit — `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`,
  `ReadWritePaths` scoped to the DB dir).
- Windows: `--install-service [--service-name NAME]` (requires absolute
  `--db-path`; auto-starts at boot), `--uninstall-service`; Stop from the
  SCM drains cleanly. NSSM remains a valid fallback.
- Never bind the console to `0.0.0.0` without TLS — the server logs a loud
  warning when it sees that combination.
