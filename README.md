# ModelSwitch

**ModelSwitch** is a Rust-based AI inference gateway that gives
applications one API for multiple LLM providers, with explicit routing,
bounded failover, usage/cost accounting, and operational visibility.

**Status:** Early development (roadmap Stage 0). Only the health
endpoints exist so far; the documents describe the intended system.

## Goals

-   One stable API for multiple providers.
-   Provider adapters behind an internal contract.
-   Explainable, versioned routing policies.
-   Timeouts, bounded retries, and eligible fallback.
-   Request history, latency, usage, and estimated cost.
-   Client authentication, limits, tests, and observable failure
    behavior.

## Proposed stack

-   Backend: Rust, Axum, Tokio
-   Persistence: PostgreSQL 18, SQLx
-   Providers: OpenAI-compatible (Groq, Ollama) and Google Gemini
-   Dashboard (later): React, TypeScript
-   Delivery: Docker, GitHub Actions
-   Testing: unit, integration, contract, failure-injection, and load
    tests

## First vertical slice

1.  `POST /v1/generate` with model aliases and `provider/model` pins
2.  Two provider adapters: OpenAI-compatible and Gemini
3.  One normalized non-streaming response
4.  Authentication and request validation
5.  Persist request outcomes and provider-reported usage
6.  Integration tests for success and failure

Do not start with a dashboard, adaptive routing, streaming, or
microservices. Build a small reliable slice first.

## Development setup

Prerequisites: Rust (stable) and PostgreSQL 18.

1.  Create a database role and development database. In `psql` as a
    superuser, choosing your own password:

    ``` sql
    CREATE ROLE modelswitch LOGIN CREATEDB PASSWORD 'choose-a-password';
    CREATE DATABASE modelswitch_dev OWNER modelswitch;
    ```

    `CREATEDB` lets `#[sqlx::test]` create a throwaway database per test.

    Use only letters and digits in the password (make it long instead).
    `DATABASE_URL` is a URL, so characters such as `@ : / # % ?` would
    otherwise need percent-encoding. If PostgreSQL listens on a
    non-default port, pass `-p <port>` to `psql` and use that port in
    `DATABASE_URL`.

2.  Copy the example files and fill in the password:

    ``` bash
    cp .env.example .env
    cp modelswitch.example.toml modelswitch.toml
    ```

3.  Run the checks and the server:

    ``` bash
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test
    cargo run -- serve
    ```

4.  In another terminal:

    ``` bash
    curl http://127.0.0.1:8080/health/live
    curl http://127.0.0.1:8080/health/ready
    ```

### With Docker (alternative)

``` bash
docker compose up --build
```

This starts PostgreSQL 18 and the gateway on port 8080 with
development-only credentials.

## Continuous integration

Every pull request runs formatting, lint (warnings are errors), tests
against PostgreSQL 18, a Docker image build, a secret scan (gitleaks),
and a dependency audit (`cargo-deny`: known vulnerabilities and a
permissive-license allowlist; see `deny.toml`).

## Documentation

-   [`docs/SDR.md`](docs/SDR.md) --- Software Design Report
-   [`docs/SRS.md`](docs/SRS.md) --- Software Requirements Specification
-   [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) --- components and
    request flow
-   [`docs/API.md`](docs/API.md) --- proposed HTTP contract
-   [`docs/DATA_MODEL.md`](docs/DATA_MODEL.md) --- proposed database
    model
-   [`docs/SECURITY.md`](docs/SECURITY.md) --- security requirements
-   [`docs/TESTING.md`](docs/TESTING.md) --- test strategy
-   [`docs/ROADMAP.md`](docs/ROADMAP.md) --- implementation milestones
-   [`docs/DECISIONS.md`](docs/DECISIONS.md) --- architecture decision
    records

## Core principles

1.  Make routing decisions inspectable.
2.  Bound every upstream call and retry sequence.
3.  Model provider capability differences explicitly.
4.  Separate reported usage from estimated cost.
5.  Do not log secrets or prompt content by default.
6.  Support reliability claims with automated tests.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
