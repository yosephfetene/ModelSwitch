# Architecture Decision Records

## ADR-001 --- Modular monolith first

**Status:** Accepted for initial release.

**Context:** The project needs clean boundaries without microservice
deployment overhead.

**Decision:** One Rust service with separate API, domain, routing,
provider, reliability, accounting, and storage modules.

**Consequences:** Easier local development and integration testing;
module dependency direction must stay clear.

## ADR-002 --- Explicit routing before adaptive routing

**Status:** Accepted.

**Context:** Adaptive routing needs representative data and a defined
objective.

**Decision:** Begin with deterministic, versioned rules. Add
benchmark-driven routing after a repeatable workload exists.

**Consequences:** Initial behavior is explainable and testable; adaptive
routing is deferred.

## ADR-003 --- Separate requests from attempts

**Status:** Accepted.

**Context:** A logical request can create multiple upstream attempts.

**Decision:** Store one request record and individual attempt records.

**Consequences:** More accurate failure/cost analysis, with somewhat
more persistence logic.

## ADR-004 --- Label estimated cost

**Status:** Accepted.

**Context:** Usage and prices vary across providers and change over
time.

**Decision:** Keep reported usage distinct from estimated cost and
record the pricing version.

**Consequences:** Historical estimates are explainable; pricing data
needs maintenance.

## ADR-005 --- No content logging by default

**Status:** Accepted.

**Context:** Prompts and completions may contain sensitive data.

**Decision:** Log metadata, timing, usage, and outcomes by default, not
prompt/completion content.

**Consequences:** Debugging should use safe fixtures or explicitly
enabled time-limited diagnostic capture.

## ADR-006 --- Non-streaming text generation first

**Status:** Accepted for first vertical slice.

**Context:** Streaming and tool calls introduce more protocol/capability
complexity.

**Decision:** Prove request, provider, persistence, and error paths
before adding them.

**Consequences:** Smaller first milestone; capability expansion requires
deliberate API evolution.

## ADR-007 --- Model aliases with optional pinning

**Status:** Accepted.

**Context:** Fallback needs more than one acceptable target, but some
clients need one exact model (testing, benchmarking, prompts tuned for a
specific model).

**Decision:** The request `model` field accepts either an operator-defined
alias (for example `fast-cheap`), which maps to an ordered candidate list
with capability requirements, or a `provider/model` pin (for example
`groq/llama-3.3-70b`). Aliases allow fallback down the candidate list.
Pins never fall back to a different provider or model; same-target
retries remain policy-controlled. Clients may disable fallback on an
alias with `routing.fallback = false`. Every response reports the
provider and resolved model that actually ran.

**Consequences:** Aliases give availability; pins give control. Fallback
can change the model, so output quality may vary within an alias; the
response makes this visible rather than hiding it.

## ADR-008 --- Initial providers: OpenAI-compatible and Gemini

**Status:** Accepted.

**Context:** The project has no paid API budget. The two first adapters
should exercise genuinely different provider formats so the internal
contract is not shaped around one vendor.

**Decision:** Adapter 1 targets the OpenAI-compatible chat API and is
used for Groq (free tier) and local Ollama. Adapter 2 targets the Google
Gemini API (free tier). An Anthropic adapter may follow, built and tested
against mock servers; live smoke tests only if API credit is added.

**Consequences:** One adapter serves several providers, which separates
the *adapter* (wire format) from the *provider* (endpoint, credentials,
models). Free tiers have tight, changing limits and terms that must be
verified when each adapter is built. Free-tier Gemini traffic may be used
by the provider, so only test data is sent to it.

## ADR-009 --- Split accounting-failure policy

**Status:** Accepted.

**Context:** A provider call and a database write can fail independently.
Failing a request after a successful, already-paid generation invites
client retries and duplicate cost.

**Decision:** Fail closed before the provider call: if a dependency
needed for authentication or limit checks is unavailable, reject the
request. Fail open after a successful generation: if the record cannot be
written, return the response with `X-ModelSwitch-Accounting: failed`,
emit a structured error log and an `accounting_failures_total` metric,
and optionally retry the write once in the background. Rule: refuse when
refusing is free; deliver when the work is already paid for.

**Consequences:** No duplicate generation caused by accounting failures.
During a database outage after the limit check, some usage may go
unrecorded; operators must alert on the accounting-failure metric.

## ADR-010 --- Missing usage stays unknown; estimates are separate

**Status:** Accepted.

**Context:** Providers usually report token usage, but not on timeouts,
dropped connections, or malformed responses, even though work and cost
may have occurred.

**Decision:** In Release 0.1, missing usage is stored as null with
`usage_source = unavailable`. Fact columns are named `reported_*`. When
budgets arrive (Stage 3), estimates are added in separate `estimated_*`
columns with an `estimate_method`, used only for budget enforcement and
counting timeouts conservatively. Cost is always an estimate
(`estimated_cost`) tied to a `pricing_version`.

**Consequences:** Stored usage is always either a provider fact or
explicitly unknown. Budgets before Stage 3 cannot account for unreported
usage.

## ADR-011 --- Gateway-generated request IDs; idempotency deferred

**Status:** Accepted; idempotency is an open decision.

**Context:** Request IDs tie together responses, logs, and records.
Client-chosen IDs can collide or carry junk. Idempotency keys would
prevent duplicate generation on client retries, but replaying a result
requires storing model output, which conflicts with ADR-005.

**Decision:** The gateway always generates a time-sortable request ID:
`req_` followed by a UUIDv7 (chosen over ULID in Stage 1 because
PostgreSQL 18 generates and stores UUIDv7 natively as `uuid`). A
client-sent `X-Request-Id` is ignored. Clients may send an optional
`X-Client-Request-Id` correlation ID, which is validated (length and
character set), stored, logged, and echoed. Idempotency keys are not
supported in Release 0.1.

**Consequences:** Every request has a trustworthy identifier and client
logs can be joined to gateway logs. Revisit idempotency when there is a
concrete need; candidate approaches are brief, encrypted, opt-in output
storage, or rejecting a duplicate key with `409` only while the first
request is still in flight.

## ADR-012 --- Client API keys in PostgreSQL, hashed with SHA-256

**Status:** Accepted.

**Context:** Keys must be revocable, attributable to request records, and
verified on every request.

**Decision:** Keys are stored in PostgreSQL from the first slice. Keys
are 32 random bytes encoded in base62 with an environment prefix
(`ms_dev_` or `ms_live_`). Only a SHA-256 hash and a short display
prefix are stored. Keys are managed with CLI subcommands of the service
binary (`modelswitch keys create|list|revoke`) and shown once on
creation. There is no admin HTTP API yet. The database is checked on
every request, so revocation takes effect immediately.

**Consequences:** A fast hash is appropriate because keys are
high-entropy machine-generated secrets; slow password hashes (Argon2,
bcrypt) protect low-entropy human secrets and would only add per-request
latency here. The prefix makes leaked keys recognizable by secret
scanners. Key management requires server access until an authenticated
admin interface exists.

## ADR-013 --- Release scope

**Status:** Accepted.

**Context:** The first milestone was defined differently across
documents: Stage 1 omitted timeouts and retries, while the Release 0.1
acceptance criteria required them.

**Decision:** Stages are internal checkpoints. Release `v0.1.0` covers
Stages 0--2 (a reliable gateway). Release `v0.2.0` covers Stage 3 (safe
to share with other users). Health endpoints move into Stage 0.

**Consequences:** The first public release delivers the core reliability
promise. Releases are tagged with a changelog.

## ADR-014 --- Native PostgreSQL for development; Docker deferred

**Status:** Accepted.

**Context:** The primary development machine runs Windows with a native
PostgreSQL 18 service and no Docker or WSL.

**Decision:** Develop against the native PostgreSQL 18 server with a
dedicated `modelswitch` role. Integration tests use `#[sqlx::test]`,
which creates a disposable database per test. CI uses a PostgreSQL 18
service container. A `Dockerfile` and `docker-compose.yml` are still
committed, and CI builds the image. Local Docker is deferred to release
hardening.

**Consequences:** No local setup beyond PostgreSQL. Local and CI
database versions are pinned to match.

## ADR-015 --- Apache-2.0 license

**Status:** Accepted.

**Context:** The project is open source and portfolio-focused now, with
possible commercialization later.

**Decision:** License the project under Apache-2.0.

**Consequences:** Broad adoption and an explicit patent grant.
Contributions are accepted under the same license without a CLA. Future
versions can still be relicensed or extended with paid features and
hosting; past releases remain Apache-2.0.

## ADR-016 --- AI-assisted development workflow

**Status:** Accepted.

**Context:** The project is built with AI assistance and is also meant
to show a disciplined, repeatable workflow.

**Decision:** One branch and pull request per slice; the default
branch is protected by CI; changes get an independent code review before merge;
commits follow Conventional Commits; behavior is specified as tests
before implementation; every change must be explainable by its author
before merge. A `CLAUDE.md` file records project rules for AI sessions.

Division of work: the AI writes most code directly in files and runs
build, lint, and tests before handing over; the author reviews the diff,
runs all git commands, and always performs the merge. Interview-critical
pieces (the reliability executor, and possibly one adapter) are typed by
the author from an agreed design and reviewed by the AI.
The repository stays private until Stage 0 is complete.

**Consequences:** Slower than unreviewed generation, but the history
shows small, reviewed, tested changes, and the workflow is reusable
across projects.

## ADR-017 --- Single crate: library plus thin binary

**Status:** Accepted.

**Context:** Integration tests in `tests/` can only import a library
crate. A Cargo workspace would let the compiler enforce module
boundaries, but the real boundaries are not known yet.

**Decision:** One crate, `modelswitch`. `src/lib.rs` holds the
application (`build_router`, state, modules); `src/main.rs` only parses
the CLI, loads configuration, connects resources, and starts the server.
Split modules into workspace crates only when a boundary has proven
itself.

**Consequences:** Integration tests build the real router in-process.
Module boundaries rely on visibility and the dependency rules in
`ARCHITECTURE.md` rather than the compiler.

## ADR-018 --- Configuration: TOML structure, secrets from environment

**Status:** Accepted.

**Context:** Configuration mixes nested, non-secret structure (server,
providers, aliases) with a few secrets (database URL, provider keys).

**Decision:** A required TOML file (`modelswitch.toml`, path overridable
with `--config` or `MODELSWITCH_CONFIG`) holds structure. Secrets come
from environment variables that the file names (`url_env`,
`api_key_env`), never values. `.env` is loaded in development only.
Parsing uses `serde` and `toml` with unknown keys rejected; no
configuration framework. Secrets are `secrecy::SecretString`. Startup
validation collects every problem into one error and never prints
secret values. Example files with placeholders are committed.

**Consequences:** Secrets cannot be committed through the config file,
and accidental debug logging prints `[REDACTED]`. A missing or invalid
file stops startup with an actionable message.

## ADR-019 --- Typed errors inside, sanitized at the edge

**Status:** Accepted; `ApiError` implemented in Stage 1. Request
middleware completes its JSON body with the request ID, so 404, 405,
invalid headers, and recovered panics share one shape.

**Context:** Internal code must branch on error kinds (retryable or
not, which status code), while clients must only see stable, sanitized
codes (FR-09).

**Decision:** Library modules define typed errors with `thiserror`.
`anyhow` is used only in `main.rs` and CLI commands. A single `ApiError`
type implementing Axum's `IntoResponse` is the only place internal
errors become HTTP responses, mapping to the codes in `API.md`; full
detail goes to logs with the request ID. Error values never carry
secrets or raw provider bodies.

**Consequences:** Leaking internals requires bypassing one well-tested
mapping. Every new error variant needs an explicit public mapping.

## ADR-020 --- Structured logging with tracing

**Status:** Accepted.

**Context:** Logs must be filterable by request, provider, and error
class, and must never contain secrets or prompt content (NFR-04).

**Decision:** Use `tracing` and `tracing-subscriber`. Output is `pretty`
by default and `json` when configured (Docker and CI). The level comes
from `RUST_LOG`, defaulting to `info`. Every request runs in a
`request` span carrying `request_id` and, when valid,
`client_request_id`; `tower-http`'s `TraceLayer` adds request and
response events at `debug`, without headers or bodies. Errors are
logged once, at the level chosen by the code that knows the cause. A test will assert that a known key and prompt never appear in
captured logs once the generate endpoint exists. Metrics arrive in
Stage 2.

**Consequences:** Every log line inside a request span carries its
context, and the span model extends to OpenTelemetry later.

## ADR-021 --- Liveness checks nothing; readiness checks only the database

**Status:** Accepted.

**Context:** Platforms restart instances that fail liveness and stop
routing to instances that fail readiness.

**Decision:** `/health/live` checks no dependencies. `/health/ready`
pings PostgreSQL (`SELECT 1`) under a configurable deadline (default
1 s) and returns `503` when it fails or times out. Providers are not
checked. Neither endpoint needs an API key, and failure details go to
logs, not response bodies. Readiness reaches the database through a
`DatabaseCheck` trait so tests can simulate outages, hangs, and
recovery. The database pool connects lazily, so the service starts while
the database is down and reports it through readiness.

**Consequences:** A database outage does not trigger restart storms, a
provider outage does not take the gateway out of rotation, and health
polling costs no provider quota.

## ADR-022 --- Embedded migrations, applied by an explicit command

**Status:** Accepted.

**Context:** The schema must match the binary that uses it, and schema
changes must be deliberate. The server also starts while the database
is down (ADR-021), so it cannot depend on migrating at startup.

**Decision:** SQL migrations live in `migrations/` and are embedded in
the binary at compile time with `sqlx::migrate!`. They are applied only
by `modelswitch migrate`; `serve` never migrates. Tests get a fresh,
migrated database per test from `#[sqlx::test]`. Key status is derived
from `revoked_at` rather than stored in a separate column.

**Consequences:** A deployed binary always carries the schema it
expects, and several instances starting at once never race to migrate.
Operators must run `migrate` before starting a new version that adds a
migration. Deriving status gives one source of truth: a key cannot be
"active" with a revocation time.

## ADR-023 --- Uniform 401 for bad keys; fail closed on lookup failure

**Status:** Accepted.

**Context:** Authentication can fail because a key is missing,
malformed, unknown, or revoked, or because the key store cannot be
reached. Each answer tells the client something and triggers different
client behavior.

**Decision:** Missing, malformed, unknown, and revoked keys all receive
the same `401 unauthorized` with `WWW-Authenticate: Bearer`; the reason
is logged (with the key ID when known), never returned. Keys that do not
have the exact key shape are rejected before any database query. If the
lookup fails or exceeds `auth.database_timeout_ms`, the request is
refused with `503 service_unavailable`: the gateway fails closed and
never serves a request it could not authenticate. Authentication is a
route layer on `/v1/*`, so unknown paths still return 404.

**Consequences:** An attacker holding an old key cannot learn whether it
was ever valid. Clients receive an actionable code: 401 means fix the
key, 503 means retry later. Junk credentials cannot load PostgreSQL. A
database outage stops all `/v1` traffic, which is the intended trade
for never serving unauthenticated requests.
