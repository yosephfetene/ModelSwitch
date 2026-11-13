use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use clap::{Parser, Subcommand};
use uuid::Uuid;

use modelswitch::api::admin::AdminState;
use modelswitch::auth::api_key::KeyEnvironment;
use modelswitch::cli::keys::CreateOptions;
use modelswitch::cli::pricing::SetPrice;
use modelswitch::concurrency::ConcurrencyLimiter;
use modelswitch::config::Config;
use modelswitch::providers::Registry;
use modelswitch::rate_limit::TokenBucketLimiter;
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::budgets::PgBudgetStore;
use modelswitch::storage::history::PgHistoryStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::storage::{self, PgDatabaseCheck};
use modelswitch::{
    AppState, build_admin_router, build_metrics_router, build_router, cli, observability,
};

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
    /// Manage the prices behind cost estimates.
    #[command(subcommand)]
    Pricing(PricingCommand),
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Create a key and print it once.
    Create {
        /// Label for the key, such as the client or machine using it.
        #[arg(long)]
        name: String,
        /// Make an `ms_live_` key instead of an `ms_dev_` one.
        #[arg(long)]
        live: bool,
        /// Days until the key stops working (1-3650). Without this, it
        /// works until revoked.
        #[arg(long, value_name = "DAYS")]
        expires_in_days: Option<u32>,
    },
    /// List keys (prefixes only, never full keys).
    List,
    /// Set or clear a key's monthly budget.
    Budget {
        /// The key's ID, as shown by `keys list`.
        id: Uuid,
        /// USD per calendar month (UTC), such as 20.
        #[arg(
            long,
            value_name = "USD",
            required_unless_present = "clear",
            conflicts_with = "clear"
        )]
        monthly_usd: Option<String>,
        /// Remove the budget.
        #[arg(long)]
        clear: bool,
    },
    /// Revoke a key immediately.
    Revoke {
        /// The key's ID, as shown by `keys list`.
        id: Uuid,
    },
}

#[derive(Subcommand)]
enum PricingCommand {
    /// Set a model's current price, closing any previous one.
    Set {
        /// A configured provider, such as `groq`.
        #[arg(long)]
        provider: String,
        /// A model configured for that provider.
        #[arg(long)]
        model: String,
        /// USD per million input tokens, such as 0.10.
        #[arg(long, value_name = "USD")]
        input_per_million: String,
        /// USD per million output tokens, such as 0.50.
        #[arg(long, value_name = "USD")]
        output_per_million: String,
        /// Where the price came from, such as the provider's pricing page.
        #[arg(long)]
        source: String,
    },
    /// List every price, current and past.
    List,
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
        Command::Pricing(command) => with_deadline(pricing(config, command)).await,
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
        KeysCommand::Create {
            name,
            live,
            expires_in_days,
        } => {
            let environment = if live {
                KeyEnvironment::Live
            } else {
                KeyEnvironment::Dev
            };
            let options = CreateOptions {
                environment,
                expires_in_days,
            };
            cli::keys::create(&pool, &name, options, &mut out).await
        }
        KeysCommand::List => cli::keys::list(&pool, Utc::now(), &mut out).await,
        KeysCommand::Budget {
            id, monthly_usd, ..
        } => cli::keys::set_budget(&pool, id, monthly_usd.as_deref(), &mut out).await,
        KeysCommand::Revoke { id } => cli::keys::revoke(&pool, id, &mut out).await,
    }
}

async fn pricing(config: Config, command: PricingCommand) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;
    let mut out = std::io::stdout().lock();
    match command {
        PricingCommand::Set {
            provider,
            model,
            input_per_million,
            output_per_million,
            source,
        } => {
            let price = SetPrice {
                provider: &provider,
                model: &model,
                input_per_million: &input_per_million,
                output_per_million: &output_per_million,
                source: &source,
            };
            cli::pricing::set(&pool, &config.catalog(), price, &mut out).await
        }
        PricingCommand::List => cli::pricing::list(&pool, &mut out).await,
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;

    let state = AppState {
        db: Arc::new(PgDatabaseCheck::new(pool.clone())),
        db_check_timeout: config.health.database_timeout,
        keys: Arc::new(PgKeyStore::new(pool.clone())),
        auth_timeout: config.auth.database_timeout,
        budgets: Arc::new(PgBudgetStore::new(pool.clone())),
        records: Arc::new(PgRecordStore::new(pool.clone())),
        accounting_timeout: config.accounting.database_timeout,
        limits: config.reliability,
        metrics: Arc::default(),
        rate_limiter: Arc::new(TokenBucketLimiter::new(config.rate_limit)),
        concurrency: Arc::new(ConcurrencyLimiter::new(config.concurrency_per_key)),
        catalog: Arc::new(config.catalog()),
        providers: Arc::new(
            Registry::from_config(&config.providers).context("failed to set up providers")?,
        ),
    };

    // Bound before serving, so a port clash stops startup instead of
    // leaving the service running unmonitored.
    if let Some(address) = config.metrics.bind {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .with_context(|| format!("failed to bind metrics address {address}"))?;
        tracing::info!(%address, "metrics listening");
        let app = build_metrics_router(Arc::clone(&state.metrics));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(%error, "metrics listener stopped");
            }
        });
    }

    if let Some(admin) = &config.admin {
        let listener = tokio::net::TcpListener::bind(admin.bind)
            .await
            .with_context(|| format!("failed to bind admin address {}", admin.bind))?;
        tracing::info!(address = %admin.bind, "admin listening");
        let history = Arc::new(PgHistoryStore::new(pool.clone()));
        let app = build_admin_router(AdminState::new(admin, Arc::clone(&state.metrics), history));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(%error, "admin listener stopped");
            }
        });
    }

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
