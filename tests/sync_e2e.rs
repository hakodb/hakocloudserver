//! End-to-end sync tests: real `CloudSync` server + clients over loopback,
//! exercising group admission (admit with key, reject without, open by
//! default) with actual document flow — not just the decision function.

use hakodb::config::HakoConfig;
use hakodb::document::hako_doc::HakoDoc;
use hakodb::document::value::Value;
use hakodb::engine::Hako;
use hakodb::cloud_sync::CloudSync;
use hakocloudserver::groups::{create_group, GroupMode};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn temp_db(tag: &str) -> (Arc<Hako>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "fl-cs-e2e-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    // Default (Interval) durability: the sync tailers read the WAL file,
    // so buffered-only Manual mode would starve them.
    (Arc::new(Hako::open(&dir, HakoConfig::default()).unwrap()), dir)
}

fn put_doc(db: &Hako, col: &str, id: &str) {
    let mut doc = HakoDoc::default();
    doc.insert("v", Value::Int(1));
    db.put(col, id, &doc).unwrap();
    db.flush().ok();
}

async fn start_server(db: Arc<Hako>, port: u16, room: &str) -> CloudSync {
    // Single-room server: serves exactly `room` (key "k"). Group rows in
    // the room db still gate admission (absent/open = admit, registered
    // = API key required); the handshake token itself is not checked.
    let sync = CloudSync::server(db, "e2e-server", "t", room, "k");
    sync.start(&format!("127.0.0.1:{port}")).await.unwrap();
    sync
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_for<F>(mut cond: F, timeout: Duration, what: &str)
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn keyed_client_syncs_into_registered_group() {
    let (server_db, sdir) = temp_db("srv");
    let (client_db, cdir) = temp_db("cli");
    let port = free_port();
    let server = start_server(server_db.clone(), port, "e2e").await;

    let (_, key) = create_group(&server_db, "e2e", GroupMode::Registered, || {
        "e2e-test-key-0123456789abcdef".to_string()
    })
    .map(|(v, k)| (v, k.unwrap()))
    .unwrap();

    // Room pair must match the server's; the group API key rides separately.
    let client = CloudSync::client(client_db.clone(), "c1", "e2e", "k", "t");
    client.set_api_key(Some(key));
    client
        .start(&format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();

    put_doc(&client_db, "notes", "n1");
    // Single-room storage: plain collection names, no prefixes.
    wait_for(
        || {
            client_db.flush().ok();
            server_db.get("notes", "n1").ok().flatten().is_some()
        },
        Duration::from_secs(20),
        "server to receive n1",
    )
    .await;

    client.stop();
    server.stop();
    std::fs::remove_dir_all(&sdir).ok();
    std::fs::remove_dir_all(&cdir).ok();
}

#[tokio::test]
async fn keyless_client_rejected_from_registered_group() {
    let (server_db, sdir) = temp_db("srv2");
    let (client_db, cdir) = temp_db("cli2");
    let port = free_port();
    let server = start_server(server_db.clone(), port, "e2e").await;

    create_group(&server_db, "e2e", GroupMode::Registered, || {
        "e2e-test-key-0123456789abcdef".to_string()
    })
    .unwrap();

    // No API key presented: handshake must fail closed.
    let client = CloudSync::client(client_db.clone(), "c2", "e2e", "k", "t");
    client
        .start(&format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    put_doc(&client_db, "notes", "n2");
    client_db.flush().ok();
    tokio::time::sleep(Duration::from_secs(6)).await;
    // Stop the planes first: a later assert panic must not skip the
    // stops (the rejected client's reconnect loop would hold the
    // runtime open and hang the binary).
    client.stop();
    server.stop();
    // The rejected peer's writes created nothing server-side. Collection
    // check first: `get` on a missing collection materializes its shard,
    // so a get-then-list order would false-positive here.
    assert!(
        server_db
            .list_collections()
            .map(|cols| !cols.iter().any(|c| c == "notes"))
            .unwrap_or(true),
        "rejected peer must not create collections"
    );
    assert!(
        server_db.get("notes", "n2").ok().flatten().is_none(),
        "keyless peer leaked into registered group"
    );

    std::fs::remove_dir_all(&sdir).ok();
    std::fs::remove_dir_all(&cdir).ok();
}

#[tokio::test]
async fn open_group_admits_anonymous_legacy_client() {
    let (server_db, sdir) = temp_db("srv3");
    let (client_db, cdir) = temp_db("cli3");
    let port = free_port();
    let server = start_server(server_db.clone(), port, "openroom").await;

    // No group row at all: historic open behavior (absent row admits).
    let client = CloudSync::client(client_db.clone(), "c3", "openroom", "k", "t");
    client
        .start(&format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    put_doc(&client_db, "notes", "n3");
    wait_for(
        || {
            client_db.flush().ok();
            server_db.get("notes", "n3").ok().flatten().is_some()
        },
        Duration::from_secs(20),
        "server to receive n3 in open group",
    )
    .await;

    client.stop();
    server.stop();
    std::fs::remove_dir_all(&sdir).ok();
    std::fs::remove_dir_all(&cdir).ok();
}
