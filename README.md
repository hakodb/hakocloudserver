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
