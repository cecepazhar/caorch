use caorch_config::Config;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Terminal,
};
use std::io;
use tokio::task::JoinSet;

pub struct CaorchTui {
    config: Config,
}

impl CaorchTui {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<(), io::Error> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut res = self.probe_all().await;

        loop {
            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(5),
                        Constraint::Length(3),
                    ])
                    .split(f.size());

                // Title
                let title = Paragraph::new(" CAOrch Dashboard (Q to quit) ")
                    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                    .block(Block::default().borders(Borders::ALL).title("CAOrch"));
                f.render_widget(title, chunks[0]);

                // Table
                let mut rows = Vec::new();
                for (node, svc, st, lat) in &res {
                    let st_cell = if *st {
                        Cell::from("UP").style(Style::default().fg(Color::Green))
                    } else {
                        Cell::from("DOWN").style(Style::default().fg(Color::Red))
                    };
                    let lat_cell = match lat {
                        Some(l) => Cell::from(format!("{l}ms")),
                        None => Cell::from("—"),
                    };
                    rows.push(Row::new(vec![
                        Cell::from(svc.clone()),
                        Cell::from(node.clone()),
                        st_cell,
                        lat_cell,
                    ]));
                }

                let table = Table::new(
                    rows,
                    [
                        Constraint::Length(20),
                        Constraint::Length(15),
                        Constraint::Length(10),
                        Constraint::Length(10),
                    ],
                )
                .header(
                    Row::new(vec!["SERVICE", "NODE", "STATUS", "LATENCY"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                )
                .block(Block::default().borders(Borders::ALL).title("Services"));
                f.render_widget(table, chunks[1]);

                // Footer
                let footer = Paragraph::new(" Press 'q' to exit | Press 'r' to refresh ")
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(footer, chunks[2]);
            })?;

            if event::poll(std::time::Duration::from_millis(500))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('r') => {
                            res = self.probe_all().await;
                        }
                        _ => {}
                    }
                }
            }
        }

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    }

    async fn probe_all(&self) -> Vec<(String, String, bool, Option<u128>)> {
        let mut set = JoinSet::new();
        for (node_name, node) in &self.config.nodes {
            for svc in &node.services {
                let host = node.ip.clone();
                let port = svc.port;
                let n = node_name.clone();
                let s = svc.name.clone();
                set.spawn(async move {
                    let r = caorch_core::probe_service(&host, port, 3000).await;
                    (n, s, r.status == caorch_core::ProbeStatus::Up, r.latency_ms)
                });
            }
        }
        let mut out = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(item) = res {
                out.push(item);
            }
        }
        out.sort();
        out
    }
}
