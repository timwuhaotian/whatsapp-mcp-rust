mod audio;
mod config;
mod extract;
mod mcp;
mod onboard;
mod store;
mod types;
mod wa;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "whatsapp-mcp",
    version,
    about = "WhatsApp MCP server: one Rust binary, stdio transport, no REST/Go/Python"
)]
struct Cli {
    #[arg(long, global = true, env = "WHATSAPP_MCP_DATA_DIR")]
    data_dir: Option<PathBuf>,

    #[arg(long, global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Serve,
    Login {
        #[arg(long)]
        phone: Option<String>,
        #[arg(long, default_value_t = false)]
        full_sync: bool,
    },
    Logout {
        #[arg(long, default_value_t = false)]
        purge: bool,
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    Doctor {
        /// Live connection check. Steals the session from a running serve
        /// instance: stop the MCP client first.
        #[arg(long, default_value_t = false)]
        connect: bool,
    },
    Config {
        #[arg(long)]
        write: Option<String>,
    },
    ImportLegacy {
        #[arg(long)]
        from: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.log_level);
    let cfg = config::AppConfig::resolve(cli.data_dir)?;
    cfg.ensure_dirs()?;
    match cli.command.unwrap_or(Commands::Serve) {
        Commands::Serve => mcp::serve(&cfg).await,
        Commands::Login { phone, full_sync } => onboard::login(&cfg, phone, full_sync).await,
        Commands::Logout { purge, yes } => onboard::logout(&cfg, purge, yes).await,
        Commands::Doctor { connect } => onboard::doctor(&cfg, connect).await,
        Commands::Config { write } => onboard::config(&cfg, write).await,
        Commands::ImportLegacy { from } => onboard::import_legacy(&cfg, &from).await,
    }
}

fn init_logging(default_level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
