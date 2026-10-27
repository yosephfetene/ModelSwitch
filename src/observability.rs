//! Structured logging setup (ADR-020).

use tracing_subscriber::EnvFilter;

use crate::config::LogFormat;

/// Installs the global tracing subscriber. The level comes from
/// `RUST_LOG` (default `info`); the format comes from configuration.
pub fn init(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);

    match format {
        LogFormat::Pretty => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}
