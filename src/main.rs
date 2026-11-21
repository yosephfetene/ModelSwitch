use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use time::OffsetDateTime;
use uuid::Uuid;

use modelswitch::api::admin::AdminState;
use modelswitch::auth::api_key::KeyEnvironment;
use modelswitch::bench::{Bar, RunOptions, Workload};
use modelswitch::cli::keys::CreateOptions;
use modelswitch::cli::pricing::SetPrice;
use modelswitch::concurrency::ConcurrencyLimiter;
use modelswitch::config::Config;
use modelswitch::domain::Target;
use modelswitch::pricing::UsdAmount;
use modelswitch::providers::Registry;
use modelswitch::rate_limit::TokenBucketLimiter;
use modelswitch::routing::adaptive::{AdaptiveRouter, REFRESH_INTERVAL};
use modelswitch::storage::api_keys::PgKeyStore;
use modelswitch::storage::budgets::PgBudgetStore;
use modelswitch::storage::history::PgHistoryStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::storage::routing::PgRoutingSource;
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
    /// Benchmark an alias's candidates (ADR-038).
    #[command(subcommand)]
    Bench(BenchCommand),
}

#[derive(Subcommand)]
enum BenchCommand {
    /// Ask every candidate of an alias the whole workload, one candidate
    /// at a time, and store the graded results. Never stores prompts or
    /// outputs.
    Run {
        /// A configured alias.
        #[arg(long)]
        alias: String,
        /// The workload file (JSON Lines).
        #[arg(long, default_value = "bench/workload-v1.jsonl")]
        workload: PathBuf,
        /// Times each item is asked of each candidate (1-20).
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=20))]
        repeats: u32,
        /// Least milliseconds between the starts of two calls, to stay
        /// within free-tier request limits (such as 6000 for 10 a minute).
        #[arg(long, value_name = "MS", default_value_t = 0)]
        min_interval_ms: u64,
    },
    /// Show an alias's latest run and which candidates are eligible.
    Report {
        /// A configured alias.
        #[arg(long)]
        alias: String,
        /// Least success rate, in percent (0-100).
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u8).range(0..=100))]
        min_success_percent: u8,
        /// Most mean cost per answered call, in USD, such as 0.0001.
        #[arg(long, value_name = "USD")]
        max_mean_cost: Option<String>,
    },
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
        Command::Bench(command) => bench(config, command).await,
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
        KeysCommand::List => cli::keys::list(&pool, OffsetDateTime::now_utc(), &mut out).await,
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

async fn bench(config: Config, command: BenchCommand) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;
    let mut out = std::io::stdout().lock();
    let candidates = |alias: &str| -> anyhow::Result<Vec<Target>> {
        config.aliases.get(alias).cloned().with_context(|| {
            let known: Vec<_> = config.aliases.keys().map(String::as_str).collect();
            format!(
                "unknown alias {alias:?}; configured aliases: {}",
                known.join(", ")
            )
        })
    };
    match command {
        BenchCommand::Run {
            alias,
            workload,
            repeats,
            min_interval_ms,
        } => {
            let candidates = candidates(&alias)?;
            let text = std::fs::read_to_string(&workload)
                .with_context(|| format!("failed to read {}", workload.display()))?;
            let name = workload.file_name().map_or_else(
                || "workload".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let workload = Workload::parse(&name, &text)
                .with_context(|| format!("invalid workload {name}"))?;
            let registry =
                Registry::from_config(&config.providers).context("failed to set up providers")?;
            let options = RunOptions {
                repeats,
                min_interval: Duration::from_millis(min_interval_ms),
            };
            // Every call has its provider's deadline; bound the whole run
            // too, including storing it.
            let slowest = candidates
                .iter()
                .filter_map(|target| config.providers.get(&target.provider))
                .map(|provider| provider.timeout)
                .max()
                .unwrap_or_default();
            let calls = candidates.len() * workload.items.len() * repeats as usize;
            let per_call = slowest + options.min_interval;
            let deadline = per_call
                .saturating_mul(u32::try_from(calls).unwrap_or(u32::MAX))
                .saturating_add(COMMAND_DEADLINE);
            let run = cli::bench::run(
                &pool,
                &registry,
                &alias,
                &candidates,
                &workload,
                options,
                &mut out,
            );
            tokio::time::timeout(deadline, run)
                .await
                .with_context(|| format!("benchmark did not finish within {deadline:?}"))?
        }
        BenchCommand::Report {
            alias,
            min_success_percent,
            max_mean_cost,
        } => {
            let candidates = candidates(&alias)?;
            let ceiling = max_mean_cost
                .as_deref()
                .map(UsdAmount::parse)
                .transpose()
                .context("--max-mean-cost")?;
            let bar = Bar {
                min_success_percent,
                ..Bar::default()
            };
            with_deadline(cli::bench::report(
                &pool,
                &alias,
                &candidates,
                &bar,
                ceiling.as_ref(),
                &mut out,
            ))
            .await
        }
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = storage::connect_lazy(&config.database).context("invalid database configuration")?;

    // Adaptive aliases reload their routing data in the background, each
    // load under the accounting deadline; requests only read memory
    // (ADR-039).
    let adaptive = Arc::new(AdaptiveRouter::new(config.adaptive.iter().filter_map(
        |(alias, policy)| {
            let candidates = config.aliases.get(alias)?.clone();
            Some((alias.clone(), (candidates, policy.clone())))
        },
    )));
    if !adaptive.is_empty() {
        let router = Arc::clone(&adaptive);
        let source = PgRoutingSource::new(pool.clone());
        let timeout = config.accounting.database_timeout;
        tokio::spawn(async move {
            loop {
                router.refresh(&source, timeout).await;
                tokio::time::sleep(REFRESH_INTERVAL).await;
            }
        });
    }

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
        adaptive,
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
        // A missing build is a setup mistake: say so now, not as a 404 later.
        if let Some(dir) = &admin.dashboard_dir
            && !dir.join("index.html").is_file()
        {
            anyhow::bail!(
                "admin.dashboard_dir: {} has no index.html; build the dashboard \
                 (npm run build in dashboard/) or remove the setting",
                dir.display()
            );
        }
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
