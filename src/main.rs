mod api;
mod app;
mod article;
mod config;
mod event;
mod input;
mod models;
mod ui;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use tokio::sync::mpsc;

use crate::app::App;
use crate::config::Config;

/// A terminal client for reading and managing Readeck bookmarks.
#[derive(Debug, Parser)]
#[command(
    name = "eckly",
    version,
    about = "Read and manage your Readeck bookmarks from the terminal",
    after_help = "Configuration is read from the config file, then overridden by --server/--token \
                  and the READECK_URL, READECK_TOKEN and READECK_API_TOKEN environment variables.\n\
                  Themes: opaline builtins; use --theme, ECKLY_THEME or --list-themes.\n\
                  Run without credentials to use the interactive setup screen."
)]
struct Cli {
    /// Readeck server URL (for example https://readeck.example.com)
    #[arg(long, value_name = "URL")]
    server: Option<String>,

    /// Readeck API token
    #[arg(long, value_name = "TOKEN")]
    token: Option<String>,

    /// Path to the configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Color theme (opaline builtin ID or display name)
    #[arg(long, value_name = "THEME")]
    theme: Option<String>,

    /// List built-in themes and exit
    #[arg(long)]
    list_themes: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list_themes {
        let mut stdout = std::io::stdout().lock();
        for (id, name) in ui::theme::all() {
            if writeln!(stdout, "{id:<24} {name}").is_err() {
                break;
            }
        }
        return Ok(());
    }
    let config_path = cli.config.unwrap_or_else(config::default_path);

    let mut config = match Config::load(&config_path) {
        Ok(Some(config)) => config,
        Ok(None) => Config::default(),
        Err(err) => {
            eprintln!("Warning: {err:#}");
            Config::default()
        }
    };

    if let Some(server) = cli.server.or_else(|| env_value("READECK_URL")) {
        config.server = server;
    }
    if let Some(token) = cli
        .token
        .or_else(|| env_value("READECK_TOKEN"))
        .or_else(|| env_value("READECK_API_TOKEN"))
    {
        config.token = token;
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "eckly must be run in an interactive terminal (try `eckly --help` for options)"
        );
    }

    let theme_name = cli
        .theme
        .or_else(|| env_value("ECKLY_THEME"))
        .unwrap_or_else(|| config.theme.clone());
    let theme_name = if theme_name.trim().is_empty() {
        "default".to_string()
    } else {
        theme_name
    };
    match ui::theme::set_name(&theme_name) {
        Ok(info) => config.theme = info.id,
        Err(err) => {
            eprintln!(
                "Warning: {err}. Using the default theme. Run `eckly --list-themes` to see available themes."
            );
            if let Ok(info) = ui::theme::set_name("default") {
                config.theme = info.id;
            }
        }
    }

    let (tx, rx) = mpsc::unbounded_channel();
    event::spawn_input_thread(tx.clone());

    let mut app = App::new(config, config_path, tx);
    app.bootstrap();

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, rx).await;
    ratatui::restore();
    result
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut rx: mpsc::UnboundedReceiver<event::Event>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if app.quit {
            break;
        }

        match rx.recv().await {
            Some(event) => app.handle_event(event),
            None => break,
        }
    }
    Ok(())
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
