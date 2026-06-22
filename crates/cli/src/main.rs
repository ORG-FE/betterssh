use anyhow::Result;
use betterssh_core::{config_path, load_default, save};
use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "betterssh",
    version,
    about = "TUI SSH manager",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    Edit,
    Print,
    Init,
    Tui,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Edit => {
            let path = config_path()?;
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let status = std::process::Command::new(editor).arg(&path).status()?;
            if !status.success() {
                anyhow::bail!("editor exited with {:?}", status.code());
            }
        }
        Cmd::Print => {
            let cfg = load_default()?;
            println!("{}", toml::to_string_pretty(&cfg)?);
        }
        Cmd::Init => {
            let cfg = load_default().unwrap_or_default();
            save(config_path()?, &cfg)?;
            println!("wrote {}", config_path()?.display());
        }
        Cmd::Tui => {
            let cfg = load_default().unwrap_or_default();
            betterssh_tui::app::run(cfg.host, cfg.settings, cfg.snippets).await?;
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("BETTERSSH_LOG")
        .unwrap_or_else(|_| EnvFilter::new("betterssh=warn,russh=error"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_target(false)
                .without_time()
                .with_writer(std::io::stderr),
        )
        .init();
}
