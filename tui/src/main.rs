fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let config_path = args.get(1).cloned().unwrap_or_else(|| "~/.caorch/caorch.toml".into());
    let config = caorch_config::load_config(expand_home(&config_path))?;
    tokio::runtime::Runtime::new()?.block_on(caorch_tui::CaorchTui::new(config).run())?;
    Ok(())
}
fn expand_home(p: &str) -> std::path::PathBuf {
    if p.starts_with("~") {
        std::path::PathBuf::from(p.strip_prefix('~').unwrap_or(p).trim_start_matches('/'))
    } else {
        std::path::PathBuf::from(p)
    }
}
