use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub alerts: AlertsConfig,
    #[serde(default)]
    pub nodes: HashMap<String, NodeConfig>,
    #[serde(default)]
    pub remediation: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DaemonConfig {
    pub interval_secs: u64,
    pub retry_threshold: u32,
    pub log_dir: String,
    pub ssh_key_path: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AlertsConfig {
    pub telegram_chat_id: String,
    pub telegram_bot_token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NodeConfig {
    pub ip: String,
    pub user: String,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServiceConfig {
    pub name: String,
    pub port: u16,
    pub protocol: String,
}

pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Config, ConfigError> {
    let content = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}
