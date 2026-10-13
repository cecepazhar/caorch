use caorch_config::Config;
use colored::Colorize;
use std::collections::BTreeMap;
use tokio::task::JoinSet;

pub async fn run(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if !caorch_core::gateway_precheck().await {
        eprintln!("{}", "gateway precheck failed: no internet, no tailscale".red());
    }

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
                println!(
                    "{} {key} down ({} failures)",
                    "⚠".yellow(),
                    n
                );
                if *n == config.daemon.retry_threshold {
                    let msg = format!("❌ {key} DOWN after {n} consecutive failures");
                    match caorch_core::send_telegram_alert(
                        &config.alerts.telegram_bot_token,
                        &config.alerts.telegram_chat_id,
                        &msg,
                    )
                    .await {
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
                        .await {
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
