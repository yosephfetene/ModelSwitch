use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use uuid::Uuid;

use modelswitch::config::Config;
use modelswitch::providers::Registry;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::{self, PgDatabaseCheck};
use modelswitch::{AppState, build_router, cli, observability};

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
    /// Apply pending database migrations.
    Migrate,
    /// Manage client API keys.
    #[command(subcommand)]
    Keys(KeysCommand),
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Create a key and print it once.
    Create {
        /// Label for the key, such as the client or machine using it.
        #[arg(long)]
        name: String,
    },
    /// List keys (prefixes only, never full keys).
    List,
    /// Revoke a key immediately.
    Revoke {
        /// The key's ID, as shown by `keys list`.
        id: Uuid,
    },
}

/// Upper bound for one operator command, including database waits.
const COMMAND_DEADLINE: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Development convenience only: production injects real env vars.
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    observability::init(config.log.format);

    match cli.command {
        Command::Serve => serve(config).await,
        Command::Migrate => with_deadline(migrate(config)).await,
        Command::Keys(command) => with_deadline(keys(config, command)).await,
    }
}

async fn with_deadline(command: impl Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
    tokio::time::timeout(COMMAND_DEADLINE, command)
        .await
        .with_context(|| format!("command did not finish within {COMMAND_DEADLINE:?}"))?
}

async fn migrate(config: Config) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;
    storage::MIGRATOR
        .run(&pool)
        .await
        .context("failed to apply migrations")?;
    tracing::info!("migrations applied");
    Ok(())
}

async fn keys(config: Config, command: KeysCommand) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;
    let mut out = std::io::stdout().lock();
    match command {
        KeysCommand::Create { name } => cli::keys::create(&pool, &name, &mut out).await,
        KeysCommand::List => cli::keys::list(&pool, &mut out).await,
        KeysCommand::Revoke { id } => cli::keys::revoke(&pool, id, &mut out).await,
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;

    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: config.health.database_timeout,
        keys: Arc::new(PgKeyStore::new(pool)),
        auth_timeout: config.auth.database_timeout,
        catalog: Arc::new(config.catalog()),
        providers: Arc::new(
            Registry::from_config(&config.providers).context("failed to set up providers")?,
        ),
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
