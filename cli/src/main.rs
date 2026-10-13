use caorch_config::load_config;
use clap::Parser;
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

#[derive(clap::Subcommand, Debug)]
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
