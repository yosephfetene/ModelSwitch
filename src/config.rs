//! Configuration: structure from a required TOML file, secrets from
//! environment variables named in that file (ADR-018).
//!
//! Loading happens in two steps. Parsing turns TOML into `FileConfig`
//! (shape errors). Validation turns `FileConfig` plus the environment
//! into `Config` (meaning errors), collecting every problem so the
//! operator can fix them all at once.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::domain::Target;
use crate::routing::Catalog;

/// Validated configuration used by the running service.
#[derive(Debug)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub health: HealthConfig,
    pub auth: AuthConfig,
    pub log: LogConfig,
    /// Configured providers by name.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Aliases by name, each an ordered, non-empty candidate list of
    /// configured targets.
    pub aliases: BTreeMap<String, Vec<Target>>,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub bind: SocketAddr,
}

#[derive(Debug)]
pub struct DatabaseConfig {
    /// Contains a password. `SecretString` prints `[REDACTED]` in debug
    /// output, so logging the config cannot leak it.
    pub url: SecretString,
    pub max_connections: u32,
}

#[derive(Debug)]
pub struct HealthConfig {
    pub database_timeout: Duration,
}

#[derive(Debug)]
pub struct AuthConfig {
    /// Upper bound on the API key lookup for each request.
    pub database_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    Pretty,
    Json,
}

#[derive(Debug)]
pub struct LogConfig {
    pub format: LogFormat,
}

/// Which wire format a provider speaks. More adapters arrive with their
/// slices (OpenAI-compatible, Gemini).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    /// Built-in: returns the input unchanged; needs no credentials.
    Echo,
}

#[derive(Debug)]
pub struct ProviderConfig {
    pub adapter: AdapterKind,
    pub models: Vec<String>,
    /// Deadline for one call to this provider.
    pub timeout: Duration,
}

/// The file as written, before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub server: FileServer,
    pub database: FileDatabase,
    #[serde(default)]
    pub health: FileHealth,
    #[serde(default)]
    pub auth: FileAuth,
    #[serde(default)]
    pub log: FileLog,
    #[serde(default)]
    pub providers: BTreeMap<String, FileProvider>,
    #[serde(default)]
    pub aliases: BTreeMap<String, FileAlias>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileServer {
    pub bind: SocketAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDatabase {
    /// Name of the environment variable holding the connection URL.
    pub url_env: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHealth {
    #[serde(default = "default_database_timeout_ms")]
    pub database_timeout_ms: u64,
}

impl Default for FileHealth {
    fn default() -> Self {
        Self {
            database_timeout_ms: default_database_timeout_ms(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileAuth {
    #[serde(default = "default_database_timeout_ms")]
    pub database_timeout_ms: u64,
}

impl Default for FileAuth {
    fn default() -> Self {
        Self {
            database_timeout_ms: default_database_timeout_ms(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLog {
    #[serde(default = "default_log_format")]
    pub format: LogFormat,
}

impl Default for FileLog {
    fn default() -> Self {
        Self {
            format: default_log_format(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProvider {
    pub adapter: AdapterKind,
    pub models: Vec<String>,
    #[serde(default = "default_provider_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileAlias {
    /// Ordered `provider/model` targets.
    pub candidates: Vec<String>,
}

fn default_provider_timeout_ms() -> u64 {
    30_000
}

fn default_max_connections() -> u32 {
    10
}

fn default_database_timeout_ms() -> u64 {
    1_000
}

fn default_log_format() -> LogFormat {
    LogFormat::Pretty
}

const MAX_CONNECTIONS_RANGE: std::ops::RangeInclusive<u32> = 1..=100;
const DATABASE_TIMEOUT_MS_RANGE: std::ops::RangeInclusive<u64> = 100..=10_000;
const PROVIDER_TIMEOUT_MS_RANGE: std::ops::RangeInclusive<u64> = 1_000..=300_000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found at {}", .0.display())]
    NotFound(PathBuf),
    #[error("could not read config file {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not parse config file: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("{0}")]
    Invalid(Problems),
}

/// Every validation problem found, reported together.
#[derive(Debug)]
pub struct Problems(pub Vec<String>);

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.0.len();
        let noun = if count == 1 { "problem" } else { "problems" };
        write!(f, "invalid configuration ({count} {noun}):")?;
        for problem in &self.0 {
            write!(f, "\n  - {problem}")?;
        }
        Ok(())
    }
}

impl Config {
    /// Reads the file at `path` and resolves secrets from the process
    /// environment.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound(path.to_path_buf())
            } else {
                ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;
        Self::from_toml(&text, |name| std::env::var(name).ok())
    }

    /// Parses and validates TOML text. `env` looks up environment
    /// variables; tests pass a fake instead of mutating the process
    /// environment.
    pub fn from_toml(
        text: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let file: FileConfig = toml::from_str(text)?;
        Self::validate(file, env)
    }

    fn validate(
        file: FileConfig,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let mut problems = Vec::new();

        let url_env = file.database.url_env.trim();
        let url = if url_env.is_empty() {
            problems.push("database.url_env: must name an environment variable".to_owned());
            None
        } else {
            match env(url_env) {
                Some(value) if !value.trim().is_empty() => Some(SecretString::from(value)),
                // Name the variable, never print a value.
                _ => {
                    problems.push(format!(
                        "database.url_env: environment variable {url_env} is not set"
                    ));
                    None
                }
            }
        };

        if !MAX_CONNECTIONS_RANGE.contains(&file.database.max_connections) {
            problems.push(format!(
                "database.max_connections: must be between {} and {}",
                MAX_CONNECTIONS_RANGE.start(),
                MAX_CONNECTIONS_RANGE.end()
            ));
        }

        if !DATABASE_TIMEOUT_MS_RANGE.contains(&file.health.database_timeout_ms) {
            problems.push(format!(
                "health.database_timeout_ms: must be between {} and {}",
                DATABASE_TIMEOUT_MS_RANGE.start(),
                DATABASE_TIMEOUT_MS_RANGE.end()
            ));
        }

        if !DATABASE_TIMEOUT_MS_RANGE.contains(&file.auth.database_timeout_ms) {
            problems.push(format!(
                "auth.database_timeout_ms: must be between {} and {}",
                DATABASE_TIMEOUT_MS_RANGE.start(),
                DATABASE_TIMEOUT_MS_RANGE.end()
            ));
        }

        let providers = validate_providers(&file.providers, &mut problems);
        let aliases = validate_aliases(&file.aliases, &file.providers, &mut problems);

        match url {
            Some(url) if problems.is_empty() => Ok(Self {
                server: ServerConfig {
                    bind: file.server.bind,
                },
                database: DatabaseConfig {
                    url,
                    max_connections: file.database.max_connections,
                },
                health: HealthConfig {
                    database_timeout: Duration::from_millis(file.health.database_timeout_ms),
                },
                auth: AuthConfig {
                    database_timeout: Duration::from_millis(file.auth.database_timeout_ms),
                },
                log: LogConfig {
                    format: file.log.format,
                },
                providers,
                aliases,
            }),
            _ => Err(ConfigError::Invalid(Problems(problems))),
        }
    }
}

impl Config {
    /// The routing view of the configured providers and aliases.
    pub fn catalog(&self) -> Catalog {
        Catalog::new(
            self.providers
                .iter()
                .map(|(name, provider)| (name.clone(), provider.models.clone())),
            self.aliases.clone(),
        )
    }
}

/// Provider names appear in pins (`name/model`), logs, and responses, so
/// they are kept to a plain identifier.
fn is_valid_provider_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn validate_providers(
    providers: &BTreeMap<String, FileProvider>,
    problems: &mut Vec<String>,
) -> BTreeMap<String, ProviderConfig> {
    let mut valid = BTreeMap::new();
    for (name, provider) in providers {
        let before = problems.len();
        if !is_valid_provider_name(name) {
            problems.push(format!(
                "providers.{name}: name must use only a-z, 0-9, - and _"
            ));
        }
        if provider.models.is_empty() {
            problems.push(format!(
                "providers.{name}.models: must list at least one model"
            ));
        }
        let mut seen = BTreeSet::new();
        for model in &provider.models {
            if model.trim().is_empty() || model.trim() != model {
                problems.push(format!(
                    "providers.{name}.models: model names must be non-empty without surrounding spaces"
                ));
            } else if !seen.insert(model) {
                problems.push(format!(
                    "providers.{name}.models: {model:?} is listed twice"
                ));
            }
        }
        if !PROVIDER_TIMEOUT_MS_RANGE.contains(&provider.timeout_ms) {
            problems.push(format!(
                "providers.{name}.timeout_ms: must be between {} and {}",
                PROVIDER_TIMEOUT_MS_RANGE.start(),
                PROVIDER_TIMEOUT_MS_RANGE.end()
            ));
        }
        if problems.len() == before {
            valid.insert(
                name.clone(),
                ProviderConfig {
                    adapter: provider.adapter,
                    models: provider.models.clone(),
                    timeout: Duration::from_millis(provider.timeout_ms),
                },
            );
        }
    }
    valid
}

fn validate_aliases(
    aliases: &BTreeMap<String, FileAlias>,
    providers: &BTreeMap<String, FileProvider>,
    problems: &mut Vec<String>,
) -> BTreeMap<String, Vec<Target>> {
    let is_configured = |target: &Target| {
        providers
            .get(&target.provider)
            .is_some_and(|provider| provider.models.contains(&target.model))
    };
    let mut valid = BTreeMap::new();
    for (name, alias) in aliases {
        let before = problems.len();
        // A `/` would make the alias indistinguishable from a pin.
        if name.is_empty() || name.contains('/') {
            problems.push(format!(
                "aliases.{name}: name must be non-empty and must not contain '/'"
            ));
        }
        if alias.candidates.is_empty() {
            problems.push(format!(
                "aliases.{name}.candidates: must list at least one target"
            ));
        }
        let mut targets = Vec::new();
        for candidate in &alias.candidates {
            match Target::parse(candidate) {
                Some(target) if is_configured(&target) => targets.push(target),
                Some(_) => problems.push(format!(
                    "aliases.{name}.candidates: {candidate:?} is not a configured provider and model"
                )),
                None => problems.push(format!(
                    "aliases.{name}.candidates: {candidate:?} must have the form provider/model"
                )),
            }
        }
        if problems.len() == before {
            valid.insert(name.clone(), targets);
        }
    }
    valid
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_URL: &str = "postgres://modelswitch:hunter2@localhost:5432/modelswitch_dev";

    const VALID: &str = r#"
        [server]
        bind = "127.0.0.1:8080"

        [database]
        url_env = "DATABASE_URL"
    "#;

    fn env_with_url(name: &str) -> Option<String> {
        (name == "DATABASE_URL").then(|| SECRET_URL.to_owned())
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn valid_file_uses_defaults_for_optional_sections() {
        let config = Config::from_toml(VALID, env_with_url).unwrap();

        assert_eq!(config.server.bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.database.max_connections, 10);
        assert_eq!(config.health.database_timeout, Duration::from_millis(1_000));
        assert_eq!(config.auth.database_timeout, Duration::from_millis(1_000));
        assert_eq!(config.log.format, LogFormat::Pretty);
    }

    #[test]
    fn missing_secret_names_the_variable() {
        let error = Config::from_toml(VALID, no_env).unwrap_err().to_string();

        assert!(error.contains("DATABASE_URL is not set"), "{error}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let text = r#"
            [server]
            bind = "127.0.0.1:8080"

            [database]
            url_env = "DATABASE_URL"
            max_connections = 0

            [health]
            database_timeout_ms = 5
        "#;

        let error = Config::from_toml(text, no_env).unwrap_err().to_string();

        assert!(error.contains("3 problems"), "{error}");
        assert!(error.contains("DATABASE_URL"), "{error}");
        assert!(error.contains("database.max_connections"), "{error}");
        assert!(error.contains("health.database_timeout_ms"), "{error}");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let text = format!("{VALID}\n[server2]\nbind = \"x\"\n");

        assert!(matches!(
            Config::from_toml(&text, env_with_url),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn debug_output_redacts_the_database_url() {
        let config = Config::from_toml(VALID, env_with_url).unwrap();
        let debug = format!("{config:?}");

        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains(SECRET_URL), "{debug}");
    }

    #[test]
    fn providers_and_aliases_are_loaded() {
        let text = format!(
            "{VALID}
[providers.echo]
adapter = \"echo\"
models = [\"echo-1\"]

\n             [aliases.default]
candidates = [\"echo/echo-1\"]
"
        );

        let config = Config::from_toml(&text, env_with_url).unwrap();

        let echo = &config.providers["echo"];
        assert_eq!(echo.adapter, AdapterKind::Echo);
        assert_eq!(echo.timeout, Duration::from_secs(30));
        assert_eq!(
            config.catalog().resolve("default", true),
            Ok(vec![Target::parse("echo/echo-1").unwrap()])
        );
    }

    #[test]
    fn provider_and_alias_problems_are_reported_together() {
        let text = format!(
            r#"{VALID}
            [providers.Bad-Name]
            adapter = "echo"
            models = []
            timeout_ms = 5

            [providers.echo]
            adapter = "echo"
            models = ["echo-1", "echo-1"]

            [aliases."has/slash"]
            candidates = ["echo/echo-1"]

            [aliases.broken]
            candidates = ["nope/model", "no-slash"]

            [aliases.empty]
            candidates = []
            "#
        );

        let error = Config::from_toml(&text, env_with_url)
            .unwrap_err()
            .to_string();

        for expected in [
            "providers.Bad-Name: name",
            "providers.Bad-Name.models: must list",
            "providers.Bad-Name.timeout_ms",
            "providers.echo.models: \"echo-1\" is listed twice",
            "aliases.has/slash: name",
            "\"nope/model\" is not a configured",
            "\"no-slash\" must have the form",
            "aliases.empty.candidates: must list",
        ] {
            assert!(
                error.contains(expected),
                "missing {expected:?} in:
{error}"
            );
        }
        assert!(error.contains("8 problems"), "{error}");
    }

    #[test]
    fn unknown_adapter_is_rejected() {
        let text = format!(
            "{VALID}
[providers.x]
adapter = \"carrier-pigeon\"
models = [\"a\"]
"
        );

        assert!(matches!(
            Config::from_toml(&text, env_with_url),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn missing_file_reports_its_path() {
        let error = Config::load(Path::new("does-not-exist.toml"))
            .unwrap_err()
            .to_string();

        assert!(error.contains("does-not-exist.toml"), "{error}");
    }
}
