//! Remote-hub probe: one-shot cloud-sync client for live hub tests.
//!
//! Gated by `HAKO_CLOUD_URL`: unset = skip (CI stays green). When set,
//! joins the room, writes a marker doc, then waits for a hub-written doc
//! (`HAKO_CLOUD_EXPECT_ID` in collection `probe`) and exits nonzero on
//! timeout. Both directions are covered in one run: the marker proves
//! client→hub (verified hub-side via the admin console), the expected
//! doc proves hub→client (this process's exit code).
//!
//! Run (any box with this repo + cargo):
//!   HAKO_CLOUD_URL=ws://<hub>:<port> HAKO_CLOUD_ROOM=<room>
//!   HAKO_CLOUD_KEY=<key> HAKO_CLOUD_TOKEN=<token>
//!   HAKO_CLOUD_EXPECT_ID=<id-the-hub-wrote> cargo test --test cloud_probe

use hakodb::config::HakoConfig;
use hakodb::document::hako_doc::HakoDoc;
use hakodb::document::value::Value;
use hakodb::engine::Hako;
use hakodb::cloud_sync::CloudSync;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_string())
}

#[tokio::test]
async fn remote_hub_probe() {
    let url = std::env::var("HAKO_CLOUD_URL").unwrap_or_default();
    if url.trim().is_empty() {
        println!("cloud_probe: HAKO_CLOUD_URL unset, skipping");
        return;
    }
    let room = env("HAKO_CLOUD_ROOM", "");
    let key = env("HAKO_CLOUD_KEY", "");
    let token = env("HAKO_CLOUD_TOKEN", "");
    let expect_id = env("HAKO_CLOUD_EXPECT_ID", "");
    assert!(!room.is_empty() && !key.is_empty(), "room + key required");
    assert!(!expect_id.is_empty(), "HAKO_CLOUD_EXPECT_ID required");

    let dir = std::env::temp_dir().join(format!(
        "hc-probe-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Arc::new(Hako::open(&dir, HakoConfig::default()).unwrap());

    let me = format!("probe-{}", std::process::id());
    let client = CloudSync::client(db.clone(), &me, &room, &key, &token);
    client.start(&url).await.expect("client start");
    println!("cloud_probe: {me} joining room {room} via {url}");

    // client -> hub marker (verified hub-side through the admin console).
    // Own collection: same-collection version ties (client 1 == hub 1)
    // block the hub's catch-up pull; separate collections stay untied.
    let mut doc = HakoDoc::default();
    doc.insert("from", Value::String(me.clone()));
    doc.insert(
        "at",
        Value::Int(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        ),
    );
    db.put("marker", &format!("{me}-here"), &doc).unwrap();
    db.flush().ok();

    // hub -> client: wait for the doc the operator wrote hub-side.
    let deadline = Instant::now() + Duration::from_secs(60);
    let got = loop {
        if db.get("probe", &expect_id).ok().flatten().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    client.stop();
    std::fs::remove_dir_all(&dir).ok();
    assert!(got, "timed out waiting for hub doc probe/{expect_id}");
    println!("cloud_probe: hub doc probe/{expect_id} arrived, both directions green");
}
