# Implementation Roadmap

Complete each stage's exit criteria before expanding scope.

## Releases

Stages are internal checkpoints. Releases are tagged, public promises
with a changelog (ADR-013).

| Release | Stages | Promise |
|---|---|---|
| `v0.1.0` | 0--2 | A reliable gateway for a single operator |
| `v0.2.0` | 3 | Safe to share with other users |
| Later | 4--6 | Dashboard, adaptive routing, hardening |

## Stage 0 --- Repository baseline

-   Rust service skeleton and configuration validation
-   PostgreSQL development setup (native PostgreSQL 18, dedicated
    `modelswitch` role) and migration workflow
-   `GET /health/live` and `GET /health/ready`
-   CI for formatting, lint, tests (PostgreSQL 18 service container),
    build, and Docker image build
-   `Dockerfile` and `docker-compose.yml`
-   `.env.example` and secret-safe configuration
-   `LICENSE` (Apache-2.0), `CLAUDE.md`, and the branch/PR workflow
-   Architecture decision records

**Exit:** a clean clone can run documented checks and CI passes. The
repository can then be made public.

## Stage 1 --- First vertical slice

-   Internal request, response, usage, capability, and typed error
    models
-   `POST /v1/generate` with request IDs and optional client
    correlation IDs
-   Model aliases and `provider/model` pins from configuration
-   Two non-streaming adapters: OpenAI-compatible (Groq, Ollama) and
    Gemini
-   API-key authentication against PostgreSQL, with
    `modelswitch keys create|list|revoke`
-   Request outcome and available usage persistence
-   Unit/integration tests with mock providers

**Exit:** both adapters work end to end; validation and upstream failure
tests pass. No dashboard yet.

## Stage 2 --- Reliability

-   Per-attempt deadline
-   Transient/non-transient error classification
-   Bounded retry and eligible fallback down alias candidates
-   Attempt persistence
-   Split accounting-failure policy (ADR-009)
-   Structured logs and metrics
-   Failure-injection tests

**Exit:** failure behavior is bounded, documented, and verified by
automated tests. Tag `v0.1.0`.

## Stage 3 --- Security and usage controls

-   API-key expiry enforcement and `ms_live_` keys
-   Request-size and concurrency limits
-   Per-key rate limits
-   Versioned pricing and estimated-cost calculation
-   Labeled usage estimates for budget enforcement (ADR-010)
-   Budget consistency guarantees
-   Redaction/security tests

**Exit:** limits cannot be trivially bypassed and estimates are
reproducible. Tag `v0.2.0`.

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
-   Deployment and example configuration, verified with local Docker
-   Reproducible load test
-   Dependency, container, secret, and alerting review
-   Known limitations and compatibility policy

**Exit:** a new operator can deploy, configure credentials securely,
check health, and troubleshoot a simulated failure.

## Scope rule

Do not build dashboard, adaptive routing, streaming, and a complex admin
system in parallel. Finish one vertical slice at a time.
