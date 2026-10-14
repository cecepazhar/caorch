use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use caorch_config::Config;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::task::JoinSet;

// ── shared state ────────────────────────────────────────────────────────────

struct AppState {
    token: String,
}

// ── auth helper ──────────────────────────────────────────────────────────────

fn check_auth(state: &AppState, headers: &HeaderMap) -> bool {
    if state.token.is_empty() {
        return true; // no token configured → open (dev mode)
    }
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == state.token)
        .unwrap_or(false)
}

macro_rules! require_auth {
    ($state:expr, $headers:expr) => {
        if !check_auth(&$state, &$headers) {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"}))).into_response();
        }
    };
}

// ── response types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ContainerInfo {
    name: String,
    status: String,
}

#[derive(Deserialize)]
struct ActionRequest {
    action: String,
    target: String,
}

#[derive(Serialize)]
struct ActionResponse {
    ok: bool,
    output: String,
}

// ── handlers ─────────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn stats(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    require_auth!(state, headers);
    // ponytail: blocking thread for sysinfo sleep; upgrade path: spawn_blocking pool
    let s = tokio::task::spawn_blocking(caorch_core::get_system_stats)
        .await
        .unwrap_or(caorch_core::SystemStats {
            cpu_pct: 0.0,
            ram_used_mb: 0,
            ram_total_mb: 0,
            disk_used_gb: 0,
            disk_total_gb: 0,
        });
    Json(s).into_response()
}

async fn containers(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    require_auth!(state, headers);
    let out = tokio::process::Command::new("docker")
        .args(["ps", "-a", "--format", "{{json .}}"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let list: Vec<ContainerInfo> = stdout
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|line| {
                    let v: serde_json::Value = serde_json::from_str(line).ok()?;
                    Some(ContainerInfo {
                        name: v["Names"].as_str().unwrap_or("").to_string(),
                        status: v["Status"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect();
            Json(list).into_response()
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": err}))).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response()
        }
    }
}

async fn action(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ActionRequest>,
) -> impl IntoResponse {
    require_auth!(state, headers);
    let (program, args): (&str, Vec<String>) = match body.action.as_str() {
        "restart" => ("docker", vec!["restart".into(), body.target.clone()]),
        "stop"    => ("docker", vec!["stop".into(),    body.target.clone()]),
        "start"   => ("docker", vec!["start".into(),   body.target.clone()]),
        "backup"  => ("tar", vec![
            "czf".into(),
            format!("/tmp/{}-backup.tar.gz", body.target),
            body.target.clone(),
        ]),
        other => {
            return Json(ActionResponse {
                ok: false,
                output: format!("unknown action: {other}"),
            }).into_response();
        }
    };

    let result = tokio::process::Command::new(program)
        .args(&args)
        .output()
        .await;

    match result {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let output = if stdout.is_empty() { stderr } else { stdout };
            Json(ActionResponse { ok: o.status.success(), output }).into_response()
        }
        Err(e) => Json(ActionResponse { ok: false, output: e.to_string() }).into_response(),
    }
}

// ── API router ───────────────────────────────────────────────────────────────

pub fn build_router(token: String) -> Router {
    let state = Arc::new(AppState { token });
    Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/containers", get(containers))
        .route("/api/action", post(action))
        .with_state(state)
}

// ── main entry ───────────────────────────────────────────────────────────────

pub async fn run(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if !caorch_core::gateway_precheck().await {
        eprintln!("{}", "gateway precheck failed: no internet, no tailscale".red());
    }

    // spawn Axum API server
    let port = config.api.port;
    let token = config.api.token.clone();
    tokio::spawn(async move {
        let app = build_router(token);
        let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
        println!("{} http://0.0.0.0:{port}/api/health", "API server started:".yellow());
        let listener = tokio::net::TcpListener::bind(addr).await.expect("bind api port");
        axum::serve(listener, app).await.expect("axum serve");
    });

    // existing monitoring loop
    let mut fails: BTreeMap<String, u32> = BTreeMap::new();
    println!(
        "{} interval={}s threshold={}",
        "CAOrch daemon started".yellow(),
        config.daemon.interval_secs,
        config.daemon.retry_threshold
    );

    loop {
        let results = probe_all(config).await?;
        for (key, r) in &results {
            if r.status == caorch_core::ProbeStatus::Up {
                fails.remove(key);
            } else {
                let n = fails.entry(key.clone()).or_insert(0);
                *n += 1;
                println!("{} {key} down ({} failures)", "⚠".yellow(), n);
                if *n == config.daemon.retry_threshold {
                    let msg = format!("❌ {key} DOWN after {n} consecutive failures");
                    match caorch_core::send_telegram_alert(
                        &config.alerts.telegram_bot_token,
                        &config.alerts.telegram_chat_id,
                        &msg,
                    )
                    .await
                    {
                        Ok(_) => println!("{}", "  alert sent".yellow()),
                        Err(e) => eprintln!("  alert error: {e}"),
                    }

                    if let Some(cmd) = config.remediation.get(key) {
                        let node_name = key.split('.').next().unwrap();
                        let node = config.nodes.get(node_name).unwrap();
                        match caorch_core::execute_remediation(
                            &config.daemon.ssh_key_path,
                            &node.user,
                            &node.ip,
                            cmd,
                        )
                        .await
                        {
                            Ok(o) => println!("  remediated {key}: {o}"),
                            Err(e) => eprintln!("  remediation {key} failed: {e}"),
                        }
                        fails.remove(key);
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(config.daemon.interval_secs)).await;
    }
}

async fn probe_all(config: &Config) -> Result<Vec<(String, caorch_core::ProbeResult)>, Box<dyn std::error::Error>> {
    let mut set: JoinSet<(String, caorch_core::ProbeResult)> = JoinSet::new();
    for (node_name, node) in &config.nodes {
        for svc in &node.services {
            let key = format!("{node_name}.{}", svc.name);
            let host = node.ip.clone();
            let port = svc.port;
            set.spawn(async move {
                let r = caorch_core::probe_service(&host, port, 5000).await;
                (key, r)
            });
        }
    }
    let mut out = Vec::new();
    while let Some(item) = set.join_next().await {
        out.push(item?);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
