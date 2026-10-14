use caorch_config::load_config;
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use tokio::task::JoinSet;

#[derive(Parser, Debug)]
#[command(name = "caorch", about = "CAOrch service orchestrator")]
struct Cli {
    /// Path to caorch.toml
    #[arg(short, long, default_value = "~/.caorch/caorch.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show live status of all services
    Status,
    /// Restart a specific service on a node
    Restart {
        node: String,
        service: String,
    },
    /// Run as monitoring daemon
    Daemon,
    /// Launch interactive TUI dashboard
    Tui,
    /// Server management via daemon HTTP API
    Server {
        #[command(subcommand)]
        sub: ServerCommand,
    },
}

#[derive(Subcommand, Debug)]
enum ServerCommand {
    /// Show system stats (CPU, RAM, Disk) from server
    Stats,
    /// List all docker containers
    Containers,
    /// Run an action on target (restart, stop, start, backup)
    Action {
        action: String,
        target: String,
    },
}

fn expand_home(p: &PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with('~') {
        if let Some(home) = std::env::var_os("HOME") {
            let mut pb = PathBuf::from(home);
            pb.push(s.strip_prefix('~').unwrap_or(&s).trim_start_matches('/'));
            return pb;
        }
    }
    p.clone()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config_path = expand_home(&cli.config);
    let config = load_config(&config_path)?;

    match cli.command {
        Command::Status => status(&config).await?,
        Command::Restart { node, service } => restart(&config, &node, &service).await?,
        Command::Daemon => daemon::run(&config).await?,
        Command::Tui => tui::run(&config).await?,
        Command::Server { sub } => server_cmd(&config, sub).await?,
    }
    Ok(())
}

async fn server_cmd(
    config: &caorch_config::Config,
    cmd: ServerCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = format!("http://127.0.0.1:{}", config.api.port);
    let client = reqwest::Client::new();
    let mut req_builder = |method: reqwest::Method, path: &str| {
        let mut req = client.request(method, format!("{base}{path}"));
        if !config.api.token.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", config.api.token));
        }
        req
    };

    match cmd {
        ServerCommand::Stats => {
            let res = req_builder(reqwest::Method::GET, "/api/stats")
                .send()
                .await?;
            if !res.status().is_success() {
                eprintln!("{}: {}", "Error".red(), res.status());
                std::process::exit(1);
            }
            let s: caorch_core::SystemStats = res.json().await?;
            println!("{}", "SYSTEM STATS".green().bold());
            println!("{:<14} {:.1}%", "CPU Usage:", s.cpu_pct);
            println!("{:<14} {} MB / {} MB", "RAM:", s.ram_used_mb, s.ram_total_mb);
            println!("{:<14} {} GB / {} GB", "Disk:", s.disk_used_gb, s.disk_total_gb);
        }
        ServerCommand::Containers => {
            let res = req_builder(reqwest::Method::GET, "/api/containers")
                .send()
                .await?;
            if !res.status().is_success() {
                eprintln!("{}: {}", "Error".red(), res.status());
                std::process::exit(1);
            }
            #[derive(serde::Deserialize)]
            struct Container {
                name: String,
                status: String,
            }
            let list: Vec<Container> = res.json().await?;
            let header = format!("{:<30} {:<30}", "CONTAINER", "STATUS");
            println!("{}", header.green().bold());
            println!("{}", "-".repeat(62).blue());
            for c in list {
                println!("{:<30} {:<30}", c.name, c.status);
            }
        }
        ServerCommand::Action { action, target } => {
            let res = req_builder(reqwest::Method::POST, "/api/action")
                .json(&serde_json::json!({ "action": action, "target": target }))
                .send()
                .await?;
            if !res.status().is_success() {
                eprintln!("{}: {}", "Error".red(), res.status());
                std::process::exit(1);
            }
            #[derive(serde::Deserialize)]
            struct ActRes {
                ok: bool,
                output: String,
            }
            let r: ActRes = res.json().await?;
            if r.ok {
                println!("{} {}", "SUCCESS:".green().bold(), r.output.trim());
            } else {
                println!("{} {}", "FAILED:".red().bold(), r.output.trim());
            }
        }
    }
    Ok(())
}

async fn status(config: &caorch_config::Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut rows: Vec<(String, String, caorch_core::ProbeStatus, Option<u128>)> =
        Vec::new();

    let mut probes = Vec::new();
    for (node_name, node) in &config.nodes {
        for svc in &node.services {
            let host = node.ip.clone();
            let port = svc.port;
            let svc_name = svc.name.clone();
            probes.push((node_name.clone(), svc_name, host, port));
        }
    }

    let mut set: JoinSet<(String, String, caorch_core::ProbeResult)> = JoinSet::new();
    for (node_name, svc_name, host, port) in probes {
        set.spawn(async move {
            let r = caorch_core::probe_service(&host, port, 5000).await;
            (node_name, svc_name, r)
        });
    }
    while let Some(res) = set.join_next().await {
        let (node_name, svc_name, r) = res?;
        rows.push((
            node_name,
            svc_name,
            r.status,
            r.latency_ms,
        ));
    }
    rows.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));

    let header = format!(
        "{:<24} {:<12} {:<8} {:<10}",
        "SERVICE", "NODE", "STATUS", "LATENCY"
    );
    println!("{}", header.green().bold());
    println!("{}", "-".repeat(56).blue());

    for (node_name, svc_name, st, lat) in &rows {
        let label = if st == &caorch_core::ProbeStatus::Up {
            "OK".green()
        } else {
            "DOWN".red()
        };
        let lat_str = match lat {
            Some(ms) => format!("{ms}ms"),
            None => "—".to_string(),
        };
        println!(
            "{:<24} {:<12} {label:<8} {lat_str:<10}",
            svc_name, node_name
        );
    }
    Ok(())
}

async fn restart(
    config: &caorch_config::Config,
    node_name: &str,
    svc_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let key = format!("{node_name}.{svc_name}");
    let cmd = config
        .remediation
        .get(&key)
        .ok_or_else(|| format!("no remediation configured for {key}"))?;
    let node = config
        .nodes
        .get(node_name)
        .ok_or_else(|| format!("node {node_name} not found"))?;
    let out = caorch_core::execute_remediation(
        &config.daemon.ssh_key_path,
        &node.user,
        &node.ip,
        cmd,
    )
    .await
    .map_err(|e| e)?;
    println!("{}", "restart complete:".green());
    println!("{out}");
    Ok(())
}

mod daemon {
    pub async fn run(config: &caorch_config::Config) -> Result<(), Box<dyn std::error::Error>> {
        caorch_daemon::run(config).await
    }
}

mod tui {
    pub async fn run(config: &caorch_config::Config) -> Result<(), Box<dyn std::error::Error>> {
        let tui = caorch_tui::CaorchTui::new(config.clone());
        tui.run().await?;
        Ok(())
    }
}
