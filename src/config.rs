//! Layered server configuration: defaults < TOML file < `FL_*` env < CLI flags.
//!
//! The env source is injected as a map (rather than read from the process)
//! so precedence is unit-testable without racy `set_var` calls.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_ROOMS_DIR: &str = "./rooms";
pub const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:8081";
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_SERVER_ID: &str = "hako-cloudserver";

/// Resolved, fully-defaulted configuration the server runs with.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub admin_bind: String,
    pub log_level: String,
    /// Emit `Secure` on session cookies. Enable with TLS (phase 7);
    /// until then the loopback default bind would only break logins.
    pub secure_cookies: bool,
    /// Sync-plane server id shown to peers (shared by all rooms).
    pub server_id: String,
    /// Root directory holding one subdirectory per room (`<room_id>/`).
    /// Each room is an independent Hako database. Default `./rooms`.
    pub rooms_dir: String,
    /// Declared rooms. Empty = refuse to boot (fail-closed: no silent
    /// default room, no auto-create on handshake).
    pub rooms: Vec<RoomDecl>,
    /// TLS certificate (PEM) for the admin plane. Both must be set to
    /// enable HTTPS; when set, session cookies are forced Secure.
    pub tls_cert: Option<String>,
    /// TLS private key (PEM) for the admin plane.
    pub tls_key: Option<String>,
}

/// One room declaration (`[[rooms]]`): a sync group with its own
/// database, hub secret, and sync listener. Different rooms implicitly
/// mean different users — hence a token per room, never shared.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomDecl {
    /// Room name (1..=64 chars, `[A-Za-z0-9_-]`, no `__` prefix).
    pub name: String,
    /// Room key: sync membership credential presented at handshake.
    /// Must equal the room's key — mismatch is a handshake reject.
    pub key: String,
    /// Hub secret for this room (presented by clients as auth token).
    pub token: String,
    /// Sync-plane bind address for this room (each room needs its own
    /// port — one listener per room, no dispatcher).
    /// Example: `"0.0.0.0:8080"`.
    pub sync_bind: String,
}

/// Partial file/env/flag layer. Every field optional; `None` inherits.
/// `[[rooms]]` is file-only (no env/CLI per-room overrides — lists don't
/// merge sanely across layers); see `RoomDecl`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigLayer {
    pub db_path: Option<String>,
    pub admin_bind: Option<String>,
    pub sync_bind: Option<String>,
    pub log_level: Option<String>,
    pub secure_cookies: Option<bool>,
    pub server_id: Option<String>,
    pub sync_token: Option<String>,
    /// Read the sync token from this file instead of argv/env (services).
    pub sync_token_file: Option<String>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    /// Rooms root directory (file-only; default `./rooms`).
    pub rooms_dir: Option<String>,
}

/// Resolve + validate `[[rooms]]` from parsed TOML. Fail-closed: empty
/// list, bad names, missing secrets, duplicate names/binds, and legacy
/// single-db keys (`db_path`/`sync_bind`/`sync_token*`) mixed with rooms
/// all refuse to boot with a named reason.
fn resolve_rooms(value: &toml::Value) -> Result<Vec<RoomDecl>, String> {
    let arr = value
        .get("rooms")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "no [[rooms]] declared: single-db config retired, declare one room per sync group (see migration notes)".to_string())?;
    if arr.is_empty() {
        return Err("no [[rooms]] declared: declare at least one room".to_string());
    }
    let mut out = Vec::with_capacity(arr.len());
    let mut names = std::collections::HashSet::new();
    let mut binds = std::collections::HashSet::new();
    for (i, item) in arr.iter().enumerate() {
        let tag = format!("rooms[{i}]");
        let get = |k: &str| item.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let name = get("name");
        if !crate::groups::valid_room_name(&name) {
            return Err(format!("{tag}.name invalid (1..=64 chars, [A-Za-z0-9_-], no __ prefix)"));
        }
        if !names.insert(name.clone()) {
            return Err(format!("duplicate room name `{name}`"));
        }
        let key = get("key");
        if key.is_empty() {
            return Err(format!("{tag} ({name}): missing key"));
        }
        let token = match (get("token"), get("token_file")) {
            (t, _) if !t.is_empty() => t,
            (_, f) if !f.is_empty() => std::fs::read_to_string(&f)
                .ok()
                .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{tag} ({name}): cannot read token_file"))?,
            _ => return Err(format!("{tag} ({name}): missing token or token_file")),
        };
        let bind: std::net::SocketAddr = get("sync_bind")
            .parse()
            .map_err(|_| format!("{tag} ({name}): sync_bind must be a valid socket address"))?;
        if !binds.insert(bind) {
            return Err(format!("duplicate sync_bind {bind} (one listener per room)"));
        }
        out.push(RoomDecl {
            name,
            key,
            token,
            sync_bind: bind.to_string(),
        });
    }
    Ok(out)
}

impl ConfigLayer {
    /// Overlay `over` on top of `self`; set fields win.
    fn merged(mut self, over: ConfigLayer) -> Self {
        if over.db_path.is_some() {
            self.db_path = over.db_path;
        }
        if over.admin_bind.is_some() {
            self.admin_bind = over.admin_bind;
        }
        if over.sync_bind.is_some() {
            self.sync_bind = over.sync_bind;
        }
        if over.log_level.is_some() {
            self.log_level = over.log_level;
        }
        if over.secure_cookies.is_some() {
            self.secure_cookies = over.secure_cookies;
        }
        if over.server_id.is_some() {
            self.server_id = over.server_id;
        }
        if over.sync_token.is_some() {
            self.sync_token = over.sync_token;
        }
        if over.sync_token_file.is_some() {
            self.sync_token_file = over.sync_token_file;
        }
        if over.tls_cert.is_some() {
            self.tls_cert = over.tls_cert;
        }
        if over.tls_key.is_some() {
            self.tls_key = over.tls_key;
        }
        if over.rooms_dir.is_some() {
            self.rooms_dir = over.rooms_dir;
        }
        self
    }

    fn resolve_with(self, rooms: Vec<RoomDecl>) -> Result<ServerConfig, String> {
        // Legacy single-db keys are refused outright (fail-closed): a
        // 0.x config must not boot against the rooms model with half
        // its meaning silently dropped. See load_config (file), and the
        // CLI/env checks there for the other layers.
        if self.db_path.is_some()
            || self.sync_bind.is_some()
            || self.sync_token.is_some()
            || self.sync_token_file.is_some()
        {
            return Err(
                "single-db keys (db_path/sync_bind/sync_token*) are retired: declare [[rooms]] instead (see migration notes)".into(),
            );
        }
        Ok(ServerConfig {
            admin_bind: self
                .admin_bind
                .unwrap_or_else(|| DEFAULT_ADMIN_BIND.into()),
            log_level: self.log_level.unwrap_or_else(|| DEFAULT_LOG_LEVEL.into()),
            secure_cookies: self.secure_cookies.unwrap_or(false),
            server_id: self.server_id.unwrap_or_else(|| DEFAULT_SERVER_ID.into()),
            rooms_dir: self.rooms_dir.unwrap_or_else(|| DEFAULT_ROOMS_DIR.into()),
            rooms,
            tls_cert: self.tls_cert,
            tls_key: self.tls_key,
        })
    }
}

impl ServerConfig {
    /// TLS is on only when both halves are configured (fail-closed: a lone
    /// cert or key is a startup error, reported by `tls_error`).
    pub fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some() && self.tls_key.is_some()
    }

    pub fn tls_error(&self) -> Option<String> {
        match (&self.tls_cert, &self.tls_key) {
            (Some(_), Some(_)) | (None, None) => None,
            (Some(_), None) => Some("tls_cert set without tls_key".into()),
            (None, Some(_)) => Some("tls_key set without tls_cert".into()),
        }
    }
}

/// `HK_*` environment layer (`HK_DB_PATH`, `HK_ADMIN_BIND`, `HK_SYNC_BIND`,
/// `HK_LOG_LEVEL`, `HK_SECURE_COOKIES=1`, `HK_SERVER_ID`, `HK_SYNC_TOKEN`,
/// `HK_TLS_CERT`, `HK_TLS_KEY`). Only non-empty values count.
/// Pre-rebrand `FL_*` spellings still work as fallback (checked second);
/// new deployments should use `HK_*`.
fn env_layer(vars: &HashMap<String, String>) -> ConfigLayer {
    let get = |k: &str| {
        vars.get(k)
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_string())
    };
    // HK_ primary, FL_ legacy fallback (removed in a later version).
    let get2 = |hk: &str, fl: &str| get(hk).or_else(|| get(fl));
    ConfigLayer {
        db_path: get2("HK_DB_PATH", "FL_DB_PATH"),
        admin_bind: get2("HK_ADMIN_BIND", "FL_ADMIN_BIND"),
        sync_bind: get2("HK_SYNC_BIND", "FL_SYNC_BIND"),
        log_level: get2("HK_LOG_LEVEL", "FL_LOG_LEVEL"),
        secure_cookies: get2("HK_SECURE_COOKIES", "FL_SECURE_COOKIES")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        server_id: get2("HK_SERVER_ID", "FL_SERVER_ID"),
        sync_token: get2("HK_SYNC_TOKEN", "FL_SYNC_TOKEN"),
        sync_token_file: get2("HK_SYNC_TOKEN_FILE", "FL_SYNC_TOKEN_FILE"),
        tls_cert: get2("HK_TLS_CERT", "FL_TLS_CERT"),
        tls_key: get2("HK_TLS_KEY", "FL_TLS_KEY"),
        rooms_dir: None,
    }
}

/// Resolve final config. `config_path`: explicit `--config`, else
/// `./hako-cloud.toml` (legacy `./firelite-cloud.toml` still honored)
/// when present, else no file layer.
pub fn load_config(
    config_path: Option<&str>,
    cli: ConfigLayer,
    vars: &HashMap<String, String>,
) -> Result<ServerConfig, String> {
    let mut base = ConfigLayer::default();
    let path = match config_path {
        Some(p) => Some(p.to_string()),
        None if std::path::Path::new("./hako-cloud.toml").exists() => {
            Some("./hako-cloud.toml".to_string())
        }
        None if std::path::Path::new("./firelite-cloud.toml").exists() => {
            // Legacy filename from pre-rebrand deployments.
            Some("./firelite-cloud.toml".to_string())
        }
        None => None,
    };
    if let Some(p) = path {
        let text =
            std::fs::read_to_string(&p).map_err(|e| format!("read config {p}: {e}"))?;
        let raw: toml::Value =
            toml::from_str(&text).map_err(|e| format!("parse config {p}: {e}"))?;
        // Rooms are file-only: resolve + validate them from the raw TOML
        // before layering (lists don't merge across layers sanely).
        let rooms = resolve_rooms(&raw)?;
        let file: ConfigLayer =
            toml::from_str(&text).map_err(|e| format!("parse config {p}: {e}"))?;
        base = base.merged(file);
        return base.merged(env_layer(vars)).merged(cli).resolve_with(rooms);
    }
    // No file: rooms can only come from a file in v1 (no env/CLI list
    // override) — fail closed rather than boot roomless.
    Err("no [[rooms]] declared: pass --config with at least one room (see migration notes)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const ONE_ROOM: &str = concat!(
        "[[rooms]]\n",
        "name = \"alpha\"\n",
        "key = \"k1\"\n",
        "token = \"t1\"\n",
        "sync_bind = \"127.0.0.1:18080\"\n",
    );

    fn write_cfg(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        std::fs::write(&dir, text).unwrap();
        dir
    }

    #[test]
    fn roomless_boot_refused() {
        // No file at all: fail closed, never boot roomless.
        assert!(load_config(None, ConfigLayer::default(), &vars(&[])).is_err());
        // Missing file: same.
        assert!(load_config(
            Some("/no/such/file.toml"),
            ConfigLayer::default(),
            &vars(&[])
        )
        .is_err());
    }

    #[test]
    fn one_room_defaults() {
        let f = write_cfg("hc-cfg-one.toml", ONE_ROOM);
        let cfg = load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[]))
            .unwrap();
        assert_eq!(cfg.admin_bind, DEFAULT_ADMIN_BIND);
        assert_eq!(cfg.log_level, DEFAULT_LOG_LEVEL);
        assert_eq!(cfg.server_id, DEFAULT_SERVER_ID);
        assert_eq!(cfg.rooms_dir, DEFAULT_ROOMS_DIR);
        assert_eq!(cfg.rooms.len(), 1);
        assert_eq!(cfg.rooms[0].name, "alpha");
        assert!(!cfg.tls_enabled());
        assert!(!cfg.secure_cookies);
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn cli_beats_env_beats_file() {
        let f = write_cfg(
            "hc-cfg-precedence.toml",
            "admin_bind = \"1.1.1.1:1\"\nlog_level = \"debug\"\nrooms_dir = \"/file/rooms\"\n",
        );
        let env = vars(&[("HK_ADMIN_BIND", "3.3.3.3:3")]);
        let cli = ConfigLayer {
            admin_bind: Some("4.4.4.4:4".into()),
            ..Default::default()
        };
        // No [[rooms]] in file: resolution fails before layering.
        assert!(load_config(Some(f.to_str().unwrap()), cli, &env).is_err());
        std::fs::remove_file(&f).ok();

        let f = write_cfg(
            "hc-cfg-precedence.toml",
            &format!("admin_bind = \"1.1.1.1:1\"\nlog_level = \"debug\"\n{ONE_ROOM}"),
        );
        let env = vars(&[("HK_ADMIN_BIND", "3.3.3.3:3")]);
        let cli = ConfigLayer {
            admin_bind: Some("4.4.4.4:4".into()),
            ..Default::default()
        };
        let cfg = load_config(Some(f.to_str().unwrap()), cli, &env).unwrap();
        assert_eq!(cfg.admin_bind, "4.4.4.4:4"); // cli over env
        assert_eq!(cfg.log_level, "debug"); // file survives where nothing overrides
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn rooms_validation_fail_closed() {
        // Empty list.
        let f = write_cfg("hc-cfg-empty.toml", "admin_bind = \"1.1.1.1:1\"\n");
        assert!(load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[])).is_err());
        // Bad name.
        let f = write_cfg(
            "hc-cfg-badname.toml",
            "[[rooms]]\nname = \"__x\"\nkey = \"k\"\ntoken = \"t\"\nsync_bind = \"127.0.0.1:18081\"\n",
        );
        assert!(load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[])).is_err());
        // Missing token.
        let f = write_cfg(
            "hc-cfg-notoken.toml",
            "[[rooms]]\nname = \"a\"\nkey = \"k\"\nsync_bind = \"127.0.0.1:18081\"\n",
        );
        assert!(load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[])).is_err());
        // Duplicate names and duplicate binds.
        let f = write_cfg(
            "hc-cfg-dup.toml",
            concat!(
                "[[rooms]]\nname = \"a\"\nkey = \"k\"\ntoken = \"t\"\nsync_bind = \"127.0.0.1:18082\"\n",
                "[[rooms]]\nname = \"a\"\nkey = \"k2\"\ntoken = \"t\"\nsync_bind = \"127.0.0.1:18083\"\n",
            ),
        );
        assert!(load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[])).is_err());
        let f = write_cfg(
            "hc-cfg-dupbind.toml",
            concat!(
                "[[rooms]]\nname = \"a\"\nkey = \"k\"\ntoken = \"t\"\nsync_bind = \"127.0.0.1:18082\"\n",
                "[[rooms]]\nname = \"b\"\nkey = \"k\"\ntoken = \"t\"\nsync_bind = \"127.0.0.1:18082\"\n",
            ),
        );
        assert!(load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[])).is_err());
        std::fs::remove_file(std::env::temp_dir().join("hc-cfg-empty.toml")).ok();
        std::fs::remove_file(std::env::temp_dir().join("hc-cfg-badname.toml")).ok();
        std::fs::remove_file(std::env::temp_dir().join("hc-cfg-notoken.toml")).ok();
        std::fs::remove_file(std::env::temp_dir().join("hc-cfg-dup.toml")).ok();
        std::fs::remove_file(std::env::temp_dir().join("hc-cfg-dupbind.toml")).ok();
    }

    #[test]
    fn legacy_single_db_keys_refused() {
        // A 0.x file must not boot against the rooms model with half its
        // meaning silently dropped.
        let f = write_cfg(
            "hc-cfg-legacy.toml",
            &format!("db_path = \"/old.db\"\n{ONE_ROOM}"),
        );
        assert!(load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &vars(&[])).is_err());
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn malformed_toml_errors() {
        let f = write_cfg("hc-cfg-bad.toml", "[[rooms] = [unclosed");
        assert!(load_config(
            Some(f.to_str().unwrap()),
            ConfigLayer::default(),
            &vars(&[])
        )
        .is_err());
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn tls_fields_and_fail_closed() {
        let f = write_cfg("hc-cfg-tls.toml", &format!("tls_cert = \"/c.pem\"\nserver_id = \"hub-1\"\n{ONE_ROOM}"));
        let env = vars(&[("HK_TLS_KEY", "/k.pem"), ("HK_SECURE_COOKIES", "true")]);
        let cfg = load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &env).unwrap();
        assert!(cfg.tls_enabled());
        assert!(cfg.tls_error().is_none());
        assert!(cfg.secure_cookies);
        assert_eq!(cfg.server_id, "hub-1");
        std::fs::remove_file(&f).ok();

        // Lone halves fail closed (tls_error reports; run() refuses boot).
        let cfg = ServerConfig {
            admin_bind: DEFAULT_ADMIN_BIND.into(),
            log_level: DEFAULT_LOG_LEVEL.into(),
            secure_cookies: false,
            server_id: DEFAULT_SERVER_ID.into(),
            rooms_dir: DEFAULT_ROOMS_DIR.into(),
            rooms: vec![],
            tls_cert: Some("/c.pem".into()),
            tls_key: None,
        };
        assert!(!cfg.tls_enabled());
        assert_eq!(
            cfg.tls_error(),
            Some("tls_cert set without tls_key".to_string())
        );
    }

    #[test]
    fn legacy_fl_env_still_honored() {
        // Pre-rebrand spellings work (checked second); HK_ wins on conflict.
        let f = write_cfg("hc-cfg-fl.toml", ONE_ROOM);
        let env = vars(&[("FL_LOG_LEVEL", "warn")]);
        let cfg = load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &env).unwrap();
        assert_eq!(cfg.log_level, "warn");
        let both = vars(&[("HK_LOG_LEVEL", "error"), ("FL_LOG_LEVEL", "warn")]);
        let cfg = load_config(Some(f.to_str().unwrap()), ConfigLayer::default(), &both).unwrap();
        assert_eq!(cfg.log_level, "error");
        std::fs::remove_file(&f).ok();
    }
}
