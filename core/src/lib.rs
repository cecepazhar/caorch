use tokio::time::Instant;

use serde::{Deserialize, Serialize};
use sysinfo::System;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStats {
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub disk_used_gb: u64,
    pub disk_total_gb: u64,
}

/// Collect CPU, RAM and disk stats via sysinfo.
pub fn get_system_stats() -> SystemStats {
    let mut sys = System::new_all();
    sys.refresh_all();
    // CPU: average across all logical CPUs after a brief refresh
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    let cpu_pct = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
        / sys.cpus().len().max(1) as f32;

    let ram_used_mb = sys.used_memory() / 1024 / 1024;
    let ram_total_mb = sys.total_memory() / 1024 / 1024;

    let disks = sysinfo::Disks::new_with_refreshed_list();
    let (disk_used, disk_total) = disks.iter().fold((0u64, 0u64), |(u, t), d| {
        let total = d.total_space();
        let avail = d.available_space();
        (u + (total - avail), t + total)
    });
    let disk_used_gb = disk_used / 1024 / 1024 / 1024;
    let disk_total_gb = disk_total / 1024 / 1024 / 1024;

    SystemStats { cpu_pct, ram_used_mb, ram_total_mb, disk_used_gb, disk_total_gb }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProbeStatus {
    Up,
    Down,
    Timeout,
}

pub struct ProbeResult {
    pub status: ProbeStatus,
    pub latency_ms: Option<u128>,
}

/// TCP connect probe.
pub async fn probe_service(host: &str, port: u16, timeout_ms: u64) -> ProbeResult {
    let addr = format!("{host}:{port}");
    let start = Instant::now();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        tokio::net::TcpStream::connect(&addr),
    )
    .await;
    match outcome {
        Ok(Ok(_)) => ProbeResult {
            status: ProbeStatus::Up,
            latency_ms: Some(start.elapsed().as_millis()),
        },
        Ok(Err(_)) => ProbeResult {
            status: ProbeStatus::Down,
            latency_ms: None,
        },
        Err(_) => ProbeResult {
            status: ProbeStatus::Timeout,
            latency_ms: None,
        },
    }
}

/// Telegram bot API alert.
pub async fn send_telegram_alert(
    bot_token: &str,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let client = reqwest::Client::new();
    let res = client
        .post(&url)
        .json(&serde_json::json!({ "chat_id": chat_id, "text": text }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("telegram status {}", res.status()));
    }
    Ok(())
}

/// Execute remote SSH remediation command.
pub async fn execute_remediation(
    ssh_key: &str,
    user: &str,
    host: &str,
    cmd: &str,
) -> Result<String, String> {
    let out = tokio::process::Command::new("ssh")
        .arg("-i")
        .arg(ssh_key)
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(format!("{user}@{host}"))
        .arg(cmd)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "ssh exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Pre-check: 1.1.1.1 reachable OR tailscale0 interface up.
pub async fn gateway_precheck() -> bool {
    let one_one = probe_service("1.1.1.1", 53, 3000).await.status == ProbeStatus::Up;
    if one_one {
        return true;
    }
    // ponytail: only checks interface exists; upgrade path: tailscale status JSON check
    matches!(
        tokio::process::Command::new("ip").args(["addr", "show", "tailscale0"]).output().await,
        Ok(o) if o.status.success()
    )
}

// ponytail: self-test checks probe statuses work correctly
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_refused_port_down() {
        let r = probe_service("127.0.0.1", 1, 1000).await;
        assert_eq!(r.status, ProbeStatus::Down);
    }

    #[tokio::test]
    async fn probe_timeout() {
        let r = probe_service("192.0.2.1", 80, 200).await;
        assert_eq!(r.status, ProbeStatus::Timeout);
    }
}
