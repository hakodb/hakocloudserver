//! Layered server configuration: defaults < TOML file < `FL_*` env < CLI flags.
//!
//! The env source is injected as a map (rather than read from the process)
//! so precedence is unit-testable without racy `set_var` calls.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_DB_PATH: &str = "./hako-cloud.db";
pub const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:8081";
pub const DEFAULT_SYNC_BIND: &str = "0.0.0.0:8080";
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_SERVER_ID: &str = "hako-cloudserver";

/// Resolved, fully-defaulted configuration the server runs with.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub db_path: String,
    pub admin_bind: String,
    pub sync_bind: String,
    pub log_level: String,
    /// Emit `Secure` on session cookies. Enable with TLS (phase 7);
    /// until then the loopback default bind would only break logins.
    pub secure_cookies: bool,
    /// Sync-plane server id shown to peers.
    pub server_id: String,
    /// Sync-plane shared token (presented by clients; admission itself is
    /// governed by group policy — see `__groups`).
    pub sync_token: String,
    /// TLS certificate (PEM) for the admin plane. Both must be set to
    /// enable HTTPS; when set, session cookies are forced Secure.
    pub tls_cert: Option<String>,
    /// TLS private key (PEM) for the admin plane.
    pub tls_key: Option<String>,
}

/// Partial file/env/flag layer. Every field optional; `None` inherits.
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
        self
    }

    fn resolve(self) -> ServerConfig {
        // Token from file beats nothing but loses to an explicit token:
        // keeps secrets out of argv/env while staying overridable.
        let file_token = self
            .sync_token_file
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().next().unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty());
        ServerConfig {
            db_path: self.db_path.unwrap_or_else(|| DEFAULT_DB_PATH.into()),
            admin_bind: self
                .admin_bind
                .unwrap_or_else(|| DEFAULT_ADMIN_BIND.into()),
            sync_bind: self.sync_bind.unwrap_or_else(|| DEFAULT_SYNC_BIND.into()),
            log_level: self.log_level.unwrap_or_else(|| DEFAULT_LOG_LEVEL.into()),
            secure_cookies: self.secure_cookies.unwrap_or(false),
            server_id: self.server_id.unwrap_or_else(|| DEFAULT_SERVER_ID.into()),
            sync_token: self.sync_token.or(file_token).unwrap_or_default(),
            tls_cert: self.tls_cert,
            tls_key: self.tls_key,
        }
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
        let file: ConfigLayer =
            toml::from_str(&text).map_err(|e| format!("parse config {p}: {e}"))?;
        base = base.merged(file);
    }
    Ok(base.merged(env_layer(vars)).merged(cli).resolve())
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

    #[test]
    fn defaults_when_nothing_set() {
        let cfg = load_config(None, ConfigLayer::default(), &vars(&[])).unwrap();
        assert_eq!(cfg.db_path, DEFAULT_DB_PATH);
        assert_eq!(cfg.admin_bind, DEFAULT_ADMIN_BIND);
        assert_eq!(cfg.sync_bind, DEFAULT_SYNC_BIND);
        assert_eq!(cfg.log_level, DEFAULT_LOG_LEVEL);
    }

    #[test]
    fn cli_beats_env_beats_file() {
        let dir = std::env::temp_dir().join("fl-cfg-precedence.toml");
        std::fs::write(
            &dir,
            "db_path = \"/file.db\"\nadmin_bind = \"1.1.1.1:1\"\nsync_bind = \"2.2.2.2:2\"\nlog_level = \"debug\"\n",
        )
        .unwrap();
        let env = vars(&[("HK_DB_PATH", "/env.db"), ("HK_ADMIN_BIND", "3.3.3.3:3")]);
        let cli = ConfigLayer {
            admin_bind: Some("4.4.4.4:4".into()),
            ..Default::default()
        };
        let cfg = load_config(Some(dir.to_str().unwrap()), cli, &env).unwrap();
        assert_eq!(cfg.db_path, "/env.db"); // env over file
        assert_eq!(cfg.admin_bind, "4.4.4.4:4"); // cli over env
        assert_eq!(cfg.sync_bind, "2.2.2.2:2"); // file survives where nothing overrides
        assert_eq!(cfg.log_level, "debug");
        std::fs::remove_file(&dir).ok();
    }

    #[test]
    fn empty_env_values_ignored_and_missing_file_errors() {
        let env = vars(&[("HK_DB_PATH", "   ")]);
        let cfg = load_config(None, ConfigLayer::default(), &env).unwrap();
        assert_eq!(cfg.db_path, DEFAULT_DB_PATH);
        assert!(load_config(
            Some("/no/such/file.toml"),
            ConfigLayer::default(),
            &vars(&[])
        )
        .is_err());
    }

    #[test]
    fn malformed_toml_errors() {
        let dir = std::env::temp_dir().join("fl-cfg-bad.toml");
        std::fs::write(&dir, "db_path = [unclosed").unwrap();
        assert!(load_config(
            Some(dir.to_str().unwrap()),
            ConfigLayer::default(),
            &vars(&[])
        )
        .is_err());
        std::fs::remove_file(&dir).ok();
    }

    #[test]
    fn tls_fields_and_fail_closed() {
        // Defaults: off, no error.
        let cfg = load_config(None, ConfigLayer::default(), &vars(&[])).unwrap();
        assert!(!cfg.tls_enabled());
        assert!(cfg.tls_error().is_none());
        assert!(!cfg.secure_cookies);

        // Env + file merge for the new fields.
        let dir = std::env::temp_dir().join("fl-cfg-tls.toml");
        std::fs::write(&dir, "tls_cert = \"/c.pem\"\nserver_id = \"hub-1\"\n").unwrap();
        let env = vars(&[("HK_TLS_KEY", "/k.pem"), ("HK_SECURE_COOKIES", "true")]);
        let cfg = load_config(Some(dir.to_str().unwrap()), ConfigLayer::default(), &env).unwrap();
        assert!(cfg.tls_enabled());
        assert!(cfg.tls_error().is_none());
        assert!(cfg.secure_cookies);
        assert_eq!(cfg.server_id, "hub-1");
        std::fs::remove_file(&dir).ok();

        // Lone halves fail closed.
        let half = ConfigLayer {
            tls_cert: Some("/c.pem".into()),
            ..Default::default()
        };
        let cfg = load_config(None, half, &vars(&[])).unwrap();
        assert!(!cfg.tls_enabled());
        assert_eq!(
            cfg.tls_error(),
            Some("tls_cert set without tls_key".to_string())
        );
    }

    #[test]
    fn legacy_fl_env_still_honored() {
        // Pre-rebrand spellings work (checked second); HK_ wins on conflict.
        let env = vars(&[("FL_DB_PATH", "/legacy.db"), ("FL_LOG_LEVEL", "warn")]);
        let cfg = load_config(None, ConfigLayer::default(), &env).unwrap();
        assert_eq!(cfg.db_path, "/legacy.db");
        assert_eq!(cfg.log_level, "warn");
        let both = vars(&[("HK_DB_PATH", "/new.db"), ("FL_DB_PATH", "/legacy.db")]);
        let cfg = load_config(None, ConfigLayer::default(), &both).unwrap();
        assert_eq!(cfg.db_path, "/new.db");
    }
}
