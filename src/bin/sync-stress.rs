//! Cloud sync stress tool (issue hakodb#69 knob calibration).
//!
//! Modes:
//! - `pull`: join, drain every replication collection locally, report docs
//!   and wall time (docs/s). Quiet-window policy decides when drain is done.
//! - `push`: write local batches and probe the hub admin API for sampled
//!   ids, reporting propagation latency percentiles (write -> hub-visible).
//!
//! Readout prefixes: `PROGRESS`, `SCORE`, `PUSH-STATUS`, single final
//! `RESULT mode=...` line (machine-parse friendly).

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use hakodb::cloud_sync::CloudSync;
use hakodb::config::HakoConfig;
use hakodb::document::hako_doc::HakoDoc;
use hakodb::document::value::Value;
use hakodb::engine::Hako;

#[derive(Parser, Debug)]
struct Args {
    /// Hub sync endpoint (room's sync bind), ws://host:port.
    #[arg(long)]
    url: String,
    /// Room name to join.
    #[arg(long)]
    room: String,
    /// Room key.
    #[arg(long)]
    key: String,
    /// Room token ("" = open room).
    #[arg(long, default_value = "")]
    token: String,
    /// Peer id label (default stress-<pid>).
    #[arg(long)]
    id: Option<String>,
    /// pull or push.
    #[arg(long, default_value = "pull")]
    mode: String,
    /// pull: count only these collections (comma list; default: all non-__).
    #[arg(long)]
    collections: Option<String>,
    /// pull: silence window that closes the drain (s).
    #[arg(long, default_value_t = 8)]
    quiet_window: u64,
    /// pull: hard cap before TIMEOUT (s).
    #[arg(long, default_value_t = 600)]
    max_seconds: u64,
    /// push: docs per write batch.
    #[arg(long, default_value_t = 1000)]
    batch: usize,
    /// push: total docs.
    #[arg(long, default_value_t = 10_000)]
    docs: usize,
    /// push: sleep between batches (ms; 0 = fly).
    #[arg(long, default_value_t = 0)]
    push_sleep_ms: u64,
    /// push: hub admin base url (REQUIRED for push).
    #[arg(long)]
    admin: Option<String>,
    /// push: admin username.
    #[arg(long)]
    user: Option<String>,
    /// push: admin password.
    #[arg(long)]
    pass: Option<String>,
    /// push: collection name (default latprobe).
    #[arg(long, default_value = "latprobe")]
    collection: String,
    /// push: payload size per doc in bytes (default 256).
    #[arg(long, default_value_t = 256)]
    doc_bytes: usize,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn make_id(prefix: &str, n: usize) -> String {
    format!("{prefix}-doc-{n:08}")
}

fn cookie_path() -> PathBuf {
    // ponytail: single shared jar name per box run is fine (one stress at a time).
    std::env::temp_dir().join("hako-stress-cookie.jar")
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Args = Parser::parse();
    match args.mode.as_str() {
        "pull" => pull(&args).await,
        "push" => push(&args).await,
        other => {
            eprintln!("unknown mode {other} (pull|push)");
            std::process::exit(2);
        }
    }
}

async fn join(args: &Args, label: &str) -> (Arc<Hako>, CloudSync) {
    let dir = std::env::temp_dir().join(format!(
        "hko-stress-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Arc::new(Hako::open(&dir, HakoConfig::default()).unwrap());
    let id = args
        .id
        .clone()
        .unwrap_or_else(|| format!("stress-{}", std::process::id()));
    let client = CloudSync::client(db.clone(), &id, &args.room, &args.key, &args.token);
    client.start(&args.url).await.expect("client start");
    eprintln!("PROGRESS {id} joined {} as {label}", args.room);
    (db, client)
}

async fn pull(args: &Args) {
    let (db, client) = join(args, "pull").await;
    let wanted: Vec<String> = args
        .collections
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();

    let start = Instant::now();
    let quiet = Duration::from_secs(args.quiet_window.max(1));
    let deadline = Duration::from_secs(args.max_seconds.max(1));
    let mut last_total: usize = 0;
    let mut last_move = Instant::now();
    let mut watch: Vec<String> = Vec::new();
    let mut timed_out = false;

    loop {
        let stats = db.get_stats();
        let mut per_col: Vec<(String, usize)> = stats
            .iter()
            .filter_map(|(k, v)| {
                k.strip_suffix("_count").map(|name| (name.to_string(), *v))
            })
            .filter(|(name, _)| !name.starts_with("__"))
            .filter(|(name, _)| {
                wanted.is_empty() || wanted.iter().any(|w| w == name)
            })
            .collect();
        per_col.sort();
        let total: usize = per_col.iter().map(|(_, n)| *n).sum();

        if total != last_total {
            watch.clear();
            for (name, _) in &per_col {
                watch.push(name.clone());
            }
            last_total = total;
            last_move = Instant::now();
        }

        if last_move.elapsed() >= quiet || start.elapsed() > deadline {
            timed_out = start.elapsed() > deadline;
            let secs = start.elapsed().as_secs_f64().max(0.001);
            let stable = if timed_out {
                // A final recount right before the report, so TIMEOUT runs
                // still print their actual gains.
                let s2 = db.get_stats();
                watch
                    .iter()
                    .map(|c| (c.clone(), s2.get(&format!("{c}_count")).copied().unwrap_or(0)))
                    .collect::<Vec<_>>()
            } else {
                per_col
            };
            for (c, n) in &stable {
                eprintln!("SCORE col {c} docs {n}");
            }
            let real: usize = stable.iter().map(|(_, n)| *n).sum();
            println!(
                "RESULT mode=pull timeout={} docs={} elapsed={:.2}s docs_per_sec={:.0}",
                timed_out,
                real,
                secs,
                real as f64 / secs
            );
            client.stop();
            return;
        }

        eprint!(
            "\rPROGRESS total={} elapsed={:.1}s  ",
            last_total,
            start.elapsed().as_secs_f64()
        );
        let _ = std::io::stderr().flush();
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn push(args: &Args) {
    let (admin_base, admin_user, admin_pass) = match (&args.admin, &args.user, &args.pass) {
        (Some(a), Some(u), Some(p)) if !a.is_empty() && !u.is_empty() => {
            (a.clone(), u.clone(), p.clone())
        }
        _ => {
            eprintln!("push requires --admin --user --pass (hub admin API probes)");
            std::process::exit(2);
        }
    };
    let (db, client) = join(args, "push").await;

    // Admin login (separate plane from the ws sync), cookie jar for probes.
    let login_body = format!(
        r#"{{"username":"USER","password":"PASS","room":"ROOM"}}"#
    )
    .replace("USER", &cli_escape(&admin_user))
    .replace("PASS", &cli_escape(&admin_pass))
    .replace("ROOM", &cli_escape(&args.room));
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "-o",
            "NUL",
            "-w",
            "%{http_code}",
            "-X",
            "POST",
            &format!("{admin_base}/api/login"),
            "-H",
            "Content-Type: application/json",
            "-d",
            &login_body,
            "-c",
            &cookie_path().to_string_lossy(),
        ])
        .output()
        .expect("curl login");
    let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if code != "200" {
        eprintln!("admin login failed: HTTP {code}");
        client.stop();
        std::process::exit(1);
    }

    let payload = "x".repeat(args.doc_bytes.max(1));
    let peers = args.batch.max(1);
    let total = args.docs.max(1);
    let probe_every = peers;
    let mut j = 0usize;
    let mut latencies: Vec<u64> = Vec::new();
    let mut since_status = Instant::now();
    let start = Instant::now();

    while j < total {
        let written = (j + probe_every).min(total) - j;
        for k in j..(j + written) {
            let id = make_id("stress", k);
            let mut doc = HakoDoc::default();
            doc.insert("from", Value::String(args.id.clone().unwrap_or_else(|| "stress".to_string())));
            doc.insert("n", Value::Int(k as i64));
            doc.insert("payload", Value::String(payload.clone()));
            db.put(&args.collection, &id, &doc).unwrap();
        }

        // One propagation probe per batch (id = last of batch).
        let id = make_id("stress", j + written - 1);
        match roundtrip(&admin_base, &args.collection, &id, Duration::from_secs(15)) {
            Ok(ms) => latencies.push(ms),
            Err(()) => eprintln!("\nPUSH-DROP probe {id} no-arrive within 15s"),
        }

        if since_status.elapsed() >= Duration::from_secs(30) {
            since_status = Instant::now();
            let mut s = latencies.clone();
            s.sort();
            eprintln!(
                "PUSH-STATUS written={} probes={} p50={} p95={}",
                j + written,
                latencies.len(),
                percentile(&s, 0.50),
                percentile(&s, 0.95),
            );
        }
        j += written;
        if args.push_sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(args.push_sleep_ms)).await;
        }
    }

    let secs = start.elapsed().as_secs_f64().max(0.001);
    let mut s = latencies.clone();
    s.sort();
    println!(
        "RESULT mode=push docs={} seconds={:.2} write_docs_per_sec={:.0} probes={} p50={} p95={} p99={} max={} (latency_ms)",
        total,
        secs,
        total as f64 / secs,
        latencies.len(),
        percentile(&s, 0.50),
        percentile(&s, 0.95),
        percentile(&s, 0.99),
        percentile(&s, 1.0),
    );
    client.stop();
}

fn cli_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Poll the hub admin API for the doc until 200 (arrival) or the cap.
fn roundtrip(admin_base: &str, col: &str, id: &str, cap: Duration) -> Result<u64, ()> {
    let url = format!("{admin_base}/api/docs/{col}/{id}");
    let jar = cookie_path().to_string_lossy().to_string();
    let started = Instant::now();
    loop {
        let out = std::process::Command::new("curl")
            .args([
                "-s",
                "-o",
                "NUL",
                "-w",
                "%{http_code}",
                "-b",
                &jar,
                &url,
            ])
            .output()
            .expect("curl probe");
        let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if code == "200" {
            return Ok(started.elapsed().as_millis() as u64);
        }
        if code != "404" && !code.is_empty() {
            eprint!("\nHTTP probe unexpected {code} ");
        }
        if started.elapsed() > cap {
            return Err(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
