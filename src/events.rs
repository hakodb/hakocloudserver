//! Server-sent realtime stream (`GET /api/events`).
//!
//! Multiplexes three sources into one SSE stream, per connection:
//!   1. document changes via `watch_collection` (one pump thread per
//!      requested collection — admin connections are rare, so no global
//!      hub; dead receivers are pruned by the engine on next write),
//!   2. peer membership diffs (polled, peers only change on connect/drop),
//!   3. version-clock diffs (polled; the version map has no push).
//!
//! All three are scoped to the caller's login-bound room: the stream can
//! never leak another room's writes, peers, or clocks. An initial
//! snapshot (peers + versions) is sent first so the UI renders without
//! waiting for the first change.
//!
//! Lifecycle without leaks: pumps push into a *bounded* channel and exit
//! when the send fails (client gone, receiver dropped); the poll loop
//! checks `is_closed()` every tick. No global state, nothing outlives the
//! connection.

use axum::{
    extract::{Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::app::{require_room, AppState, AuthedUser};

const CHANNEL_DEPTH: usize = 128;
const PEER_POLL: Duration = Duration::from_secs(2);
const VERSION_POLL: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize, Default)]
pub struct EventsQuery {
    /// Comma-separated collections to watch. Default: all current
    /// non-internal collections at connect time.
    collections: Option<String>,
}

/// Minimal `Stream` adapter over a tokio mpsc receiver (avoids a
/// stream-adapter dependency for one use site).
struct MpscStream {
    rx: mpsc::Receiver<Result<Event, Infallible>>,
}

impl futures_core::Stream for MpscStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Snapshot of room peer membership for diffing (this room's sync plane
/// only — never another room's).
fn peer_snapshot(sync: Option<&Arc<hakodb::cloud_sync::CloudSync>>) -> HashSet<String> {
    match sync {
        Some(s) => s.peer_list().into_iter().map(|p| p.peer_key).collect(),
        None => HashSet::new(),
    }
}

/// Snapshot of this room's version clock: collection -> version. Single
/// map (one database, one room); the poll loop diffs it whole.
fn version_snapshot_synced(
    sync: Option<&Arc<hakodb::cloud_sync::CloudSync>>,
) -> HashMap<String, i64> {
    match sync {
        Some(s) => s.room_versions(),
        None => HashMap::new(),
    }
}

fn send_event(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    kind: &str,
    payload: serde_json::Value,
) -> bool {
    let ev = match Event::default().event(kind).json_data(payload) {
        Ok(e) => e,
        Err(_) => return false,
    };
    tx.blocking_send(Ok(ev)).is_ok()
}

async fn send_event_async(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    kind: &str,
    payload: serde_json::Value,
) -> bool {
    let ev = match Event::default().event(kind).json_data(payload) {
        Ok(e) => e,
        Err(_) => return false,
    };
    tx.send(Ok(ev)).await.is_ok()
}

pub async fn events(
    State(state): State<Arc<AppState>>,
    user: AuthedUser,
    Query(q): Query<EventsQuery>,
) -> Response {
    let room = match require_room(&state, &user) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let room_name = room.name().to_string();
    let db = room.db().clone();
    let sync = room.sync().cloned();
    let wanted: Option<HashSet<String>> = q.collections.map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    // Bound the fan-out: one blocking thread per watched collection.
    if wanted.as_ref().is_some_and(|w| w.len() > 16) {
        use axum::{http::StatusCode, response::IntoResponse, Json};
        use serde_json::json;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "at most 16 collections per events stream"})),
        )
            .into_response();
    }
    if wanted.as_ref().is_some_and(|w| {
        w.iter().any(|c| c.split('/').next().is_some_and(|s| s.starts_with("__")))
    }) {
        use axum::{http::StatusCode, response::IntoResponse, Json};
        use serde_json::json;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "internal collection"})),
        )
            .into_response();
    }
    // Explicitly requested collections are watched verbatim — even ones
    // that don't exist yet (first write creates them and fires). Without
    // the parameter we watch what exists now; collections born later on an
    // open-ended stream can't be anticipated.
    let collections: Vec<String> = match wanted {
        Some(w) => w.into_iter().collect(),
        None => db
            .list_collections()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !c.starts_with('_'))
            .collect(),
    };

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(CHANNEL_DEPTH);

    // 1. Document pumps: one blocking thread per collection. Exits when the
    // client goes away (send fails on the dropped receiver).
    for col in collections {
        let tx_pump = tx.clone();
        let db_pump = db.clone();
        std::thread::spawn(move || {
            let rx_watch = db_pump.watch_collection(&col);
            while let Ok(ev) = rx_watch.recv() {
                let kind = match ev.kind {
                    hakodb::engine::ChangeKind::Put => "put",
                    hakodb::engine::ChangeKind::Delete => "delete",
                };
                if !send_event(
                    &tx_pump,
                    "doc",
                    json!({"collection": col, "id": ev.path, "kind": kind}),
                ) {
                    break;
                }
            }
        });
    }

    // Initial snapshot so the UI renders immediately.
    let sync_ref = sync.as_ref();
    let peers_now = peer_snapshot(sync_ref);
    let versions_now = version_snapshot_synced(sync_ref);
    let _ = send_event_async(&tx, "peers", json!({ "room": &room_name, "peers": peers_now.iter().collect::<Vec<_>>() })).await;
    let _ = send_event_async(&tx, "versions", json!({ "room": &room_name, "versions": versions_now })).await;

    // 2+3. Poll loop for peer and version diffs. Ends when the client
    // disconnects (receiver dropped => is_closed).
    let tx_poll = tx.clone();
    tokio::spawn(async move {
        let mut last_peers = peers_now;
        let mut last_versions = versions_now;
        let mut peer_tick = tokio::time::interval(PEER_POLL);
        let mut ver_tick = tokio::time::interval(VERSION_POLL);
        // Skip the immediate first ticks (snapshot already sent).
        peer_tick.tick().await;
        ver_tick.tick().await;
        loop {
            tokio::select! {
                _ = peer_tick.tick() => {
                    if tx_poll.is_closed() {
                        break;
                    }
                    let now = peer_snapshot(sync.as_ref());
                    if now != last_peers {
                        last_peers = now.clone();
                        if !send_event_async(&tx_poll, "peers", json!({ "room": room_name, "peers": now.into_iter().collect::<Vec<_>>() })).await {
                            break;
                        }
                    }
                }
                _ = ver_tick.tick() => {
                    if tx_poll.is_closed() {
                        break;
                    }
                    let now = version_snapshot_synced(sync.as_ref());
                    if now != last_versions {
                        last_versions = now.clone();
                        if !send_event_async(&tx_poll, "versions", json!({ "room": room_name, "versions": now })).await {
                            break;
                        }
                    }
                }
            }
        }
    });

    Sse::new(MpscStream { rx })
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}
