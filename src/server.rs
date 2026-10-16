//! Server runner shared by console mode and the Windows Service entry.
//!
//! `run(cfg, shutdown)` boots both planes and resolves when either plane
//! fails, or when `shutdown` fires — in which case the sync plane is
//! stopped and `Ok` is returned for a clean exit.

use std::future::Future;

use crate::app::{build_router, AppState};
use crate::config::ServerConfig;

pub async fn run(cfg: ServerConfig, shutdown: impl Future<Output = ()>) -> Result<(), String> {
    // try_init: safe to call from console and service paths alike (second
    // call is a no-op error, not a panic).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&cfg.log_level)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
    if let Some(e) = cfg.tls_error() {
        return Err(e);
    }
    let tls = cfg.tls_enabled();
    if (cfg.secure_cookies || tls) && !cfg.secure_cookies {
        tracing::info!("TLS active: session cookies forced Secure");
    }
    // Loud warning for the dangerous combination: world-reachable console
    // without transport encryption. Default bind stays loopback.
    let bind_warn = match cfg.admin_bind.parse::<std::net::SocketAddr>() {
        Ok(a) => !a.ip().is_loopback(),
        Err(_) => true,
    };
    if bind_warn && !tls {
        tracing::warn!(
            admin_bind = %cfg.admin_bind,
            "admin console bound to a non-loopback address WITHOUT TLS — credentials cross the network in cleartext; use --tls-cert/--tls-key or a reverse proxy"
        );
    }

    // Builtin server plane, declared explicitly (never implied by naming):
    // user credentials and group credentials must not replicate to
    // clients. The core also excludes these by default; this declaration
    // keeps the guarantee local to the server crate so it survives any
    // future change to core defaults. A client naming one of these in a
    // packet is dropped at the ingest choke point and never served,
    // relayed, or tail-broadcast under that name.
    let sync_excluded = vec!["__users".to_string(), "__groups".to_string()];

    // One independent database + one single-room sync server per declared
    // room. Rooms never share a handle, so data can never mix across
    // rooms no matter what the sync plane does.
    let mut rooms = crate::rooms::open_all(&cfg.rooms_dir, &cfg.rooms, sync_excluded)?;
    let mut sync_tasks = Vec::new();
    for decl in &cfg.rooms {
        let handle = rooms.get_mut(&decl.name).expect("just opened");
        tracing::info!(room = %decl.name, bind = %decl.sync_bind, "room open");
        let sync = std::sync::Arc::new(hakodb::cloud_sync::CloudSync::server(
            handle.db.clone(),
            &cfg.server_id,
            &decl.token,
            &decl.name,
            &decl.key,
        ));
        let bind = decl.sync_bind.clone();
        let s = sync.clone();
        sync_tasks.push(tokio::spawn(async move {
            s.start(&bind)
                .await
                .map_err(|e| format!("sync serve {bind}: {e}"))
        }));
        handle.sync = Some(sync);
    }
    tracing::info!(rooms = rooms.len(), admin_bind = %cfg.admin_bind, server_id = %cfg.server_id, "hako-cloudserver starting");

    // Sync handles for the shutdown arm (the AppState below moves into
    // the router; stopping goes through these clones instead).
    let sync_stops: Vec<std::sync::Arc<hakodb::cloud_sync::CloudSync>> = rooms
        .values()
        .filter_map(|h| h.sync.clone())
        .collect();

    let state = std::sync::Arc::new(
        AppState::new(rooms, cfg.secure_cookies || cfg.tls_enabled())
            .with_config(cfg.clone()),
    );
    let app =
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();

    let admin_task = tokio::spawn(async move {
        if cfg.tls_enabled() {
            let cert = cfg.tls_cert.clone().expect("checked");
            let key = cfg.tls_key.clone().expect("checked");
            let rustls_cfg =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                    .await
                    .map_err(|e| format!("load TLS cert/key: {e}"))?;
            let addr: std::net::SocketAddr = cfg
                .admin_bind
                .parse()
                .map_err(|e| format!("parse admin bind {}: {e}", cfg.admin_bind))?;
            axum_server::bind_rustls(addr, rustls_cfg)
                .serve(app)
                .await
                .map_err(|e| format!("admin serve (tls): {e}"))
        } else {
            let listener = tokio::net::TcpListener::bind(&cfg.admin_bind)
                .await
                .map_err(|e| format!("bind {}: {e}", cfg.admin_bind))?;
            axum::serve(listener, app)
                .await
                .map_err(|e| format!("admin serve: {e}"))
        }
    });

    tokio::select! {
        // Any plane dying takes the process down (fail-fast: the service
        // manager restarts us clean rather than half-serving).
        joined = async {
            let mut first_err: Option<String> = None;
            for t in sync_tasks {
                match t.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(format!("task panicked: {e}"));
                        }
                    }
                }
            }
            match admin_task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(format!("task panicked: {e}"));
                    }
                }
            }
            first_err
        } => match joined {
            None => Ok(()),
            Some(e) => Err(e),
        },
        _ = shutdown => {
            sync_stops.iter().for_each(|s| s.stop());
            Ok(())
        }
    }
}
