//! Per-room databases: one independent `Hako` per declared room.
//!
//! A room is `(name, key)`; its directory is `<rooms_dir>/<room_id>/`
//! where `room_id = hex(sha256(name\0key))` — opaque, traversal-proof,
//! collision-free across keys sharing a name. No prefixes, no shared
//! registry collection, no cross-room anything. Rooms never see each
//! other's data because they never share a database handle.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use hakodb::config::HakoConfig;
use hakodb::engine::Hako;
use sha2::{Digest, Sha256};

use crate::config::RoomDecl;

/// Opaque room identity: hex SHA-256 over `name\0key`. Same pair always
/// maps to the same directory; different keys never collide, even under
/// one name (that property used to need `name_1` suffix allocation).
pub fn room_id(name: &str, key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b"\x00");
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

/// One open room: its database plus its sync plane (wired by server.rs).
#[derive(Clone)]
pub struct RoomHandle {
    pub name: String,
    pub id: String,
    pub db: Arc<Hako>,
    pub sync: Option<Arc<hakodb::cloud_sync::CloudSync>>,
    /// Engine-to-engine mesh leg (replica rooms dial the writer; optional
    /// listener rooms serve as dial targets). Wired by server.rs.
    pub mesh: Option<Arc<hakodb::net_sync::NetSyncer>>,
    /// True when opened as a mesh replica (engine fence engaged).
    pub read_only: bool,
}

/// Open every declared room. Fail-closed as a unit: a bad declaration
/// never reaches here (config validation), but any per-room open
/// failure refuses the whole boot (a half-open hub would serve some
/// rooms and 404 others with no signal which).
pub fn open_all(
    rooms_dir: &str,
    rooms: &[RoomDecl],
    sync_excluded: Vec<String>,
) -> Result<HashMap<String, RoomHandle>, String> {
    let mut out = HashMap::with_capacity(rooms.len());
    for decl in rooms {
        // Defense in depth: config validation already rejects duplicate
        // names, but a silent overwrite here would point two rooms at one
        // handle — refuse instead.
        if out.contains_key(&decl.name) {
            return Err(format!("duplicate room name `{}`", decl.name));
        }
        let id = room_id(&decl.name, &decl.key);
        let dir = Path::new(rooms_dir).join(&id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("room `{}` dir: {e}", decl.name))?;
        let hc = HakoConfig {
            sync_excluded: sync_excluded.clone(),
            ..Default::default()
        };
        let db = Arc::new(
            Hako::open(
                dir.to_string_lossy().as_ref(),
                hc,
            )
            .map_err(|e| format!("room `{}` open: {e}", decl.name))?,
        );
        if decl.read_only {
            // Fence FIRST: no snapshot of this engine is ever writable by
            // data-plane code — every write path rejects loudly. State
            // arrives exclusively via the mesh tailer from the writer pod.
            db.set_read_only(true);
        }
        out.insert(
            decl.name.clone(),
            RoomHandle {
                name: decl.name.clone(),
                id,
                db,
                sync: None,
                mesh: None,
                read_only: decl.read_only,
            },
        );
    }
    Ok(out)
}

/// Hex helper without pulling another crate (sha2 already required).
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let mut s = String::with_capacity(bytes.as_ref().len() * 2);
        for b in bytes.as_ref() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_id_stable_and_key_sensitive() {
        assert_eq!(room_id("alpha", "k1"), room_id("alpha", "k1"));
        assert_ne!(room_id("alpha", "k1"), room_id("alpha", "k2"));
        assert_ne!(room_id("alpha", "k1"), room_id("beta", "k1"));
        assert_eq!(room_id("alpha", "k1").len(), 64);
        assert!(room_id("alpha", "k1")
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn open_all_creates_isolated_dirs() {
        use crate::config::RoomDecl;
        let root = std::env::temp_dir().join(format!("hc-rooms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let specs = vec![
            RoomDecl {
                name: "alpha".into(),
                key: "k1".into(),
                token: "t".into(),
                sync_bind: "127.0.0.1:0".into(),
                ..Default::default()
            },
            RoomDecl {
                name: "beta".into(),
                key: "k2".into(),
                token: "t".into(),
                sync_bind: "127.0.0.1:0".into(),
                ..Default::default()
            },
        ];
        // Same key material shape, different names: two opaque, disjoint
        // directories — no prefix sharing, no cross-room anything.
        let map = open_all(root.to_string_lossy().as_ref(), &specs, vec![]).unwrap();
        assert_eq!(map.len(), 2);
        assert_ne!(map["alpha"].id, map["beta"].id);
        // Same name + same key reopens the same directory (resume).
        let again = open_all(
            root.to_string_lossy().as_ref(),
            &specs[..1],
            vec![],
        )
        .unwrap();
        assert_eq!(again["alpha"].id, map["alpha"].id);
        // Same name twice in one boot is a hard error, never a silent
        // overwrite of one room by the other.
        let dup = vec![specs[0].clone(), specs[0].clone()];
        assert!(open_all(root.to_string_lossy().as_ref(), &dup, vec![]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
