# ModelSwitch

**ModelSwitch** is a Rust-based AI inference gateway that gives
applications one API for multiple LLM providers, with explicit routing,
bounded failover, usage/cost accounting, and operational visibility.

**Status:** Design-stage specification. These documents describe the
intended system, not features already implemented.

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
-   Persistence: PostgreSQL, SQLx
-   Dashboard (later): React, TypeScript
-   Delivery: Docker, GitHub Actions
-   Testing: unit, integration, contract, failure-injection, and load
    tests

## First vertical slice

1.  `POST /v1/generate`
2.  Two provider adapters
3.  One normalized non-streaming response
4.  Authentication and request validation
5.  Persist request outcomes and provider-reported usage
6.  Integration tests for success and failure

Do not start with a dashboard, adaptive routing, streaming, or
microservices. Build a small reliable slice first.

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
