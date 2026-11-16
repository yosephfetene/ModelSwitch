//! The configuration files the repository ships must load: the example
//! that new users copy, and the two baked into the Docker image. The
//! image's admin variant must differ from the default only by `[admin]`,
//! so the two cannot drift apart.

use std::path::{Path, PathBuf};

use modelswitch::config::Config;

const TOKEN: &str = "admin-token-for-tests-0123456789abcdef";

fn env(name: &str) -> Option<String> {
    match name {
        "DATABASE_URL" => Some("postgres://user:pass@localhost/db".to_owned()),
        "MODELSWITCH_ADMIN_TOKEN" => Some(TOKEN.to_owned()),
        _ => None,
    }
}

fn load(file: &str) -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    Config::from_toml(&text, env).unwrap_or_else(|error| panic!("{file}: {error}"))
}

#[test]
fn the_example_config_loads() {
    let config = load("modelswitch.example.toml");
    assert!(config.admin.is_none(), "the dashboard is opt-in");
}

#[test]
fn the_docker_config_loads_without_the_dashboard() {
    let config = load("deploy/modelswitch.docker.toml");
    assert_eq!(config.server.bind.to_string(), "0.0.0.0:8080");
    assert!(
        config.admin.is_none(),
        "the default image needs no admin token"
    );
}

#[test]
fn the_docker_admin_config_serves_the_built_dashboard() {
    let config = load("deploy/modelswitch.docker-admin.toml");
    let admin = config.admin.as_ref().expect("[admin] is configured");
    assert_eq!(admin.bind.to_string(), "0.0.0.0:9091");
    assert_eq!(
        admin.dashboard_dir,
        Some(PathBuf::from("/usr/share/modelswitch/dashboard")),
        "where the Dockerfile copies the dashboard build"
    );
}

#[test]
fn the_docker_configs_differ_only_by_admin() {
    let base = load("deploy/modelswitch.docker.toml");
    let mut admin = load("deploy/modelswitch.docker-admin.toml");
    admin.admin = None;
    assert_eq!(format!("{admin:?}"), format!("{base:?}"));
}
