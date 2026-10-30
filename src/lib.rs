//! ModelSwitch: a multi-provider LLM inference gateway.
//!
//! The library holds the whole application so integration tests in
//! `tests/` can build it in-process; `main.rs` only wires it to the
//! outside world (ADR-017).

pub mod api;
pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod domain;
pub mod observability;
pub mod providers;
pub mod routing;
pub mod storage;

pub use app::{AppState, build_router};
