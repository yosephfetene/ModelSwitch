# Implementation Roadmap

Complete each stage's exit criteria before expanding scope.

## Stage 0 --- Repository baseline

-   Rust service skeleton and configuration validation
-   PostgreSQL development setup and migration workflow
-   CI for formatting, lint, tests, and build
-   `.env.example` and secret-safe configuration
-   Architecture decision records

**Exit:** a clean clone can run documented checks and CI passes.

## Stage 1 --- First vertical slice

-   Internal request, response, usage, capability, and typed error
    models
-   `POST /v1/generate`
-   Two non-streaming provider adapters
-   Development API-key authentication
-   Request outcome and available usage persistence
-   Unit/integration tests with mock providers

**Exit:** both providers work end to end; validation and upstream
failure tests pass. No dashboard yet.

## Stage 2 --- Reliability

-   Per-attempt deadline
-   Transient/non-transient error classification
-   Bounded retry and eligible fallback
-   Attempt persistence
-   Health/readiness endpoints and structured logs
-   Failure-injection tests

**Exit:** failure behavior is bounded, documented, and verified by
automated tests.

## Stage 3 --- Security and usage controls

-   API-key creation, hashing, revocation, optional expiry
-   Request-size and concurrency limits
-   Per-key rate limits
-   Versioned pricing and estimated-cost calculation
-   Budget consistency and accounting-failure policy
-   Redaction/security tests

**Exit:** limits cannot be trivially bypassed and estimates are
reproducible.

## Stage 4 --- Dashboard

-   React + TypeScript
-   Request history, provider/model, status, latency, usage, cost,
    routing reason
-   Time/provider/status filters
-   Authenticated access and clear loading/error/empty states
-   No raw keys or prompts exposed

**Exit:** dashboard values reconcile with stored records.

## Stage 5 --- Benchmark-informed routing

-   Reproducible workload
-   Defined quality checks and optimization objective
-   Compare latency, failure rate, usage/cost, and task success
-   Policy decisions based on measured constraints
-   Safe static-policy fallback

**Exit:** publish evidence of where adaptive routing helps and where it
does not.

## Stage 6 --- Release hardening

-   Migration/index/retention/backup review
-   Deployment and example configuration
-   Reproducible load test
-   Dependency, container, secret, and alerting review
-   Known limitations and compatibility policy

**Exit:** a new operator can deploy, configure credentials securely,
check health, and troubleshoot a simulated failure.

## Scope rule

Do not build dashboard, adaptive routing, streaming, and a complex admin
system in parallel. Finish one vertical slice at a time.
