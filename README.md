# hakocloudserver

> Part of [**HakoDB**](https://github.com/hakodb/hakodb) — embedded Firestore-style document DB in Rust. The engine + C ABI live in `hakodb/hakodb`; this repo holds the cloud-sync hub.

Standalone HakoDB cloud-sync hub
with admin console: one independent database + one single-room sync
listener per declared room, and an HTTP admin API + UI scoped to the
room you log into.

## Rooms: one database per sync group

Each `[[rooms]]` entry is a sync group with its own database
(`<rooms_dir>/<room_id>/`), its own hub secret, and its own sync
listener port. Rooms never share a handle, so data can never mix across
rooms no matter what the sync plane does. There is no shared registry
and no default room: an empty room list refuses to boot, and unknown
room names 404/400 fail-closed. Sessions bind to the room named at
login — one session can never touch two rooms.

## Builtin server plane

The credential stores (`__users`, `__groups`, one pair per room) are
declared sync-excluded in the server — they never replicate to
clients, by exact name rather than by naming convention.

## Compatibility

| hakocloudserver | hako core |
|---|---|
| 0.1.1 | `hakodb 0.16` (crates.io, `cloud-sync` feature) |

## Build & test

```sh
cargo build --release
cargo test
```

The `hakodb` dependency comes from crates.io (`cloud-sync` feature).

## Run

One process, N+1 ports: the admin console plus one sync listener per
room (each room needs its own port — one listener per room, no
dispatcher).

```bash
hakocloudserver \
  --config /etc/hako-cloud/hako-cloud.toml \
  --admin-bind 127.0.0.1:8081
```

Configuration layers (lowest wins last): compiled defaults <
`--config` file (`./hako-cloud.toml` auto-loaded when present; legacy
`./firelite-cloud.toml` still honored) < `HK_*` env
(`HK_ADMIN_BIND`, `HK_LOG_LEVEL`, `HK_SECURE_COOKIES=1`,
`HK_SERVER_ID`, `HK_TLS_CERT`, `HK_TLS_KEY`; pre-rebrand `FL_*`
spellings still work as fallback) < CLI flags. `[[rooms]]` is
file-only (no env/CLI per-room overrides — lists don't merge sanely
across layers), and the legacy single-db keys (`db_path`, `sync_bind`,
`sync_token*`) are refused outright: a 0.x config must not boot
against the rooms model with half its meaning silently dropped.
A minimal TOML:

```toml
admin_bind = "127.0.0.1:8081"
rooms_dir = "/var/lib/hako-cloud/rooms"

[[rooms]]
name = "field-team"
key = "room-secret-see-op-notes"
token = "hub-secret-clients-present-this"
sync_bind = "0.0.0.0:8080"

[[rooms]]
name = "hq"
key = "another-room-secret"
token_file = "/run/secrets/hako-hq-token"
sync_bind = "0.0.0.0:8081"
```

Room names: `[A-Za-z0-9_-]`, max 64 chars, no `__` prefix; binds and
names must be unique. Same name + same key reopens the same directory
(resume); a room's directory name is an opaque hash, never the plain
name.

### First run

Open the console (`http://127.0.0.1:8081`). The setup wizard lists the
declared rooms — pick one and create its initial administrator (each
room has its own users; the wizard stays reachable until every room
has an admin). Login names its room; the whole console — data, groups,
users, events, maintenance — is then scoped to it. Roles: `viewer`
(read), `operator` (read + write data), `admin` (everything incl.
users, groups, maintenance).

### Groups: open by default, registered when you mean it

Within each room, peers are admitted by group policy unless you
create a **group** for the sync name (Groups view): `registered` mode
issues an API key (shown once — only its hash persists) that peers
present at handshake; an optional member list pins allowed
`client_id`s. Absent groups stay open, so existing deployments keep
working untouched. Rotating a key is one click; switching a group
back to `open` destroys the stored hash.

### Topology advice: hub 1–2 peers, mesh the rest

Point one (two for redundancy) always-on peer per site at the room's
sync port; let the remaining devices sync peer-to-peer over net_sync
locally. The hubs converge through the server; LAN traffic never
leaves the site. The dashboard's room/peer view shows whether the
topology holds.

### TLS and services

Direct TLS for the admin console: `--tls-cert fullchain.pem --tls-key
privkey.pem` (session cookies flip `Secure` automatically; HSTS follows).
The sync plane stays `ws://` behind a reverse proxy, or terminate there
too — both are documented deployments. Refusing to start with only half
the TLS pair is deliberate (fail-closed).

- Linux: `contrib/hakocloudserver.service` (hardened
  systemd unit — `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`,
  `ReadWritePaths` scoped to the rooms dir).
- Windows: `--install-service [--service-name NAME]` (requires absolute
  `--config`; auto-starts at boot), `--uninstall-service`; Stop from the
  SCM drains cleanly. NSSM remains a valid fallback.
- Never bind the console to `0.0.0.0` without TLS — the server logs a loud
  warning when it sees that combination.

### Security posture (audited)

- `__users` and `__groups` are unreachable over the data plane, even
  for operators; events streams cap 16 collections and refuse `__*`;
  backup paths must be absolute without `..`.
- Login and setup name their room; unknown rooms fail closed (no
  default-room fallback). Sessions are bound to one room each.
- Room names: `[A-Za-z0-9_-]`, max 64, `__`-prefixed reserved; malformed
  group policies fail closed. Absent groups stay open (historic) — create
  a `registered` group per room in production.
- Sync traffic is plaintext `ws://`: terminate TLS at a reverse proxy
  (or run the hub on a trusted LAN) and rotate group API keys after any
  network you don't control sees them.
- Sessions are keyed by token hash; setup is single-flight (no admin race);
  admin plane rate-limited 600/min/IP (login keeps its own 5/min).
