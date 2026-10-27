use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

use modelswitch::config::Config;
use modelswitch::storage::{self, PgDatabaseCheck};
use modelswitch::{AppState, build_router, observability};

#[derive(Parser)]
#[command(version, about = "Multi-provider LLM inference gateway")]
struct Cli {
    /// Path to the configuration file.
    #[arg(
        long,
        env = "MODELSWITCH_CONFIG",
        default_value = "modelswitch.toml",
        global = true
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Development convenience only: production injects real env vars.
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    observability::init(config.log.format);

    match cli.command {
        Command::Serve => serve(config).await,
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;

    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool)),
        db_check_timeout: config.health.database_timeout,
    };

    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.server.bind))?;
    tracing::info!(address = %config.server.bind, "listening");

    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to listen for shutdown signal");
    }
    tracing::info!("shutting down");
}
