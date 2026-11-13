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
(`estimated_cost`) tied to a `pricing_version`. ADR-035 later decided that
budgets need no estimated token columns.

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

## ADR-024 --- Built-in echo provider; first candidate only until Stage 2

**Status:** Accepted.

**Context:** `POST /v1/generate` should be usable end to end before the
first real adapter exists, and anyone cloning the repository should be
able to try it without a provider account. Fallback down an alias's
candidates belongs to the reliability executor, which has its own
design work in Stage 2.

**Decision:** An `echo` adapter is built in: it returns the input
unchanged and reports no usage. Operators configure it like any other
provider (`adapter = "echo"`). Routing resolves an alias to its full
ordered candidate list, but until the executor exists the handler calls
only the first candidate and returns its failure (502) or timeout (504)
without trying another. Provider names are limited to `a-z`, `0-9`, `-`,
and `_`; alias names may not contain `/`, so an alias can never be
mistaken for a pin. `metadata` and `routing.policy` are rejected until a
slice gives them meaning.

**Consequences:** The full request path (auth, validation, routing,
deadline, response shape) is exercised without network access or
credentials, in tests and in demos. Aliases do not yet improve
availability; that arrives with Stage 2 without changing the API.

## ADR-025 --- Hardened outbound calls to OpenAI-compatible providers

**Status:** Accepted.

**Context:** The first real adapter sends a provider API key and client
prompts to a URL taken from configuration, and returns text from a
system we do not control. A misconfiguration or a misbehaving endpoint
should not leak the key or the prompt, or exhaust memory.

**Decision:** The OpenAI-compatible adapter calls
`POST {base_url}/chat/completions` with one user message and
`stream: false`, using `max_tokens` (accepted by both Groq and Ollama).
`base_url` must be `https`, except plain `http` to this machine (local
Ollama); credentials, queries, and fragments in the URL are refused.
Provider keys come from the environment variable named by `api_key_env`
and are sent only as a bearer token. HTTP redirects are never followed.
Response bodies are capped at 4 MiB. On an error status only the
provider's short error code (`error.code`, else `error.type`, when it
looks like an identifier) is kept; the message text is discarded
because it can quote the prompt. TLS uses rustls with the `ring`
backend, shared with sqlx.

**Consequences:** A key cannot be bounced to another host, sent in clear
text over the network, or written to logs through a URL. Error logs
keep enough to classify failures (status and code, which Stage 2 needs
for retry decisions) without prompt content. A provider that relies on
redirects will fail visibly with its 3xx status.

## ADR-026 --- Gemini adapter: key, model names, thinking, and refusals

**Status:** Accepted.

**Context:** The second adapter speaks Google's `generateContent`
format, which differs from the OpenAI one in ways that touch security
and accounting: the model name is part of the URL, Google accepts the
key in the query string, thinking models report reasoning separately
from the answer, and Google can refuse to answer at all.

**Decision:** The Gemini adapter calls
`POST {base_url}/models/{model}:generateContent` with one user turn and
`generationConfig.maxOutputTokens` when a limit is given. The key is
required and sent only in the `x-goog-api-key` header, never as `?key=`.
Gemini model names in configuration are limited to 1-128 of
`A-Z a-z 0-9 . - _`, and the adapter also percent-encodes the name as
one path segment. Parts marked `thought` are left out of the output.
Output tokens are `candidatesTokenCount + thoughtsTokenCount`, both as
reported (one missing half counts as zero, since Google omits zero
counts; both missing stays unknown); this matches how Google bills and
how OpenAI-compatible providers report `completion_tokens`. A response
with no candidates and a `promptFeedback.blockReason`, or a candidate
with no answer text and a finish reason other than `STOP` or
`MAX_TOKENS`, becomes `ProviderError::Blocked` with Google's reason code
(kept only if it looks like an identifier). Running out of tokens before
any answer text is an empty answer with its usage, not a failure. On an
error status the code is `error.code` when it is a string, else
`error.status`. The redirect policy, size cap, and error-text rules of
ADR-025 move to a shared HTTP module used by both adapters, which also
strips the URL from transport errors.

**Consequences:** A Gemini key cannot reach logs through a URL, and a
model name cannot redirect the call to another endpoint. Usage for
thinking models is comparable across providers. Refusals are
distinguishable in logs and, from Stage 2, in retry decisions; clients
still see the generic `provider_error` (502), since the reason belongs
to the logs (ADR-019). Free-tier Gemini traffic may be used by Google,
so only test data is sent to it (ADR-008).

## ADR-027 --- Request records: what, when, and under which deadline

**Status:** Accepted.

**Context:** ADR-009 fixed what happens when accounting fails, but not
which requests are recorded, when the write happens, or what a record
holds. These choices set the cost of every request and what the
database knows about clients.

**Decision:** Every request that reached a provider gets exactly one
row in `request_records`: success, provider failure, or timeout.
Requests rejected before any provider call (401, 400, 413, 422) get
none: they did no work, and a client with no valid key has no account
to charge. The row is written before responding, under its own deadline
(`accounting.database_timeout_ms`, default 1000 ms). If the write fails
or times out, the client still receives the full response (success or
error) with `X-ModelSwitch-Accounting: failed`, and an ERROR log
`accounting_write_failed` carries the request ID. A row holds the
request and client correlation IDs, the client key, the requested and
the selected provider and model, the outcome, the failure kind with the
provider's short code and HTTP status, latency, and the token counts
exactly as reported; never the prompt or the output. `created_at` is
the database clock at insert minus the measured latency. The optional
background retry from ADR-009 and the `accounting_failures_total`
metric are deferred until metrics exist (Stage 2).

**Consequences:** The extra latency is one indexed insert in the normal
case and at most the accounting deadline during an outage. A server
restart cannot lose records that were acknowledged, because nothing is
queued in memory. If a client disconnects while the provider is still
working, the request is cancelled and no row is written; recording
abandoned work is left to the Stage 2 executor, which owns attempts.

## ADR-028 --- Retry and fallback policy

**Status:** Accepted.

**Context:** ADR-007 allows aliases to fall back and pins to retry the
same target, and FR-08 requires retries and fallback to be finite and
recorded. The executor needs exact rules: which failures are worth
another try, where, how many, how long, and what the client sees when
everything fails.

**Decision:** Each failed attempt is classified:

-   *Transient* (connection failure, provider 5xx, unusable response):
    retry the same target once, then fall back to the next candidate.
    One retry gives a passing glitch a chance to clear; a second failure
    in a row suggests a real problem, and more retries would let one
    failing provider use up the budget before a healthy one is tried.
-   *Target problem* (timeout, 401, 403, 404, 429, other unexpected
    statuses): skip to the next candidate. A timeout already used its
    whole deadline and may have been billed, and a rate limit or a
    missing model will not change in a moment, but another provider may
    succeed.
-   *Request problem* (blocked by a safety filter, other 4xx): stop.
    Sending a refused prompt to other providers until one accepts it
    would route around safety filters.

Gemini sends a bad API key as a 400 with the catch-all status
`INVALID_ARGUMENT`, and names the problem only in `details`
(`API_KEY_INVALID`). Its adapter records that reason as the code and
keeps the real 400, and a 400 with that code is a target problem: the
provider's key is at fault, not the request.

A request makes at most `max_attempts` attempts (default 3) and runs at
most `request_timeout` in total (default 60 s), whichever ends first.
Each attempt's deadline is the smaller of its provider's timeout and
the time remaining. Before retrying the same target the executor waits
a random 100--300 ms, doubling the range on each further retry of that
target, never past the deadline: if the wait would not fit in the time
left, the retry is skipped and the executor falls back, which needs no
wait. Pins and
`routing.fallback = false` use the same rules with a one-target list.
When every attempt fails, the client receives the last attempt's error
(502 `provider_error` or 504 `upstream_timeout`); provider names and
codes go only to logs and records (ADR-019). Every response that reached
a provider carries `X-ModelSwitch-Attempts: <n>`. The limits are
configured under `[reliability]`. This replaces the first-candidate-only
behavior of ADR-024.

**Consequences:** Failure handling is bounded in both attempts and time,
so a fast-failing provider cannot cause a retry storm and a slow one
cannot hold a client past the request deadline. Jittered backoff keeps
retries from many clients from arriving as one wave. Fallback can
change the model that answers; responses already report which one ran
(ADR-007).

## ADR-029 --- Attempt records are written with their request

**Status:** Accepted.

**Context:** ADR-003 separates a request from its attempts. With
retries and fallback, the request record alone shows only the last
attempt, so "why did this request take three tries?" could be answered
only from logs.

**Decision:** Each provider call gets a row in `request_attempts`,
written in the same transaction as the `request_records` row, under the
same accounting deadline (ADR-027). If any row fails, nothing is stored
and the usual accounting failure applies (ADR-009). A row holds the
target, how the call ended, its failure class (ADR-028), and what
happened next: `retry`, `fallback`, `exhausted` (another try was allowed
but nothing was left to try), or `not_retried`. That decision is derived
from the following attempt, so the executor does not track it.

**Consequences:** A request never exists without its attempts, or the
reverse, at the cost of a few more inserts per request. Token counts
stay on the request: providers report none for a failed call, so usage
spent on failed attempts remains unknown (ADR-010).

## ADR-030 --- Metrics on a separate internal listener

**Status:** Accepted.

**Context:** ADR-009 requires an `accounting_failures_total` metric, and
operators need to see provider health. These totals reveal traffic,
providers, and failure rates, so they should not be public; but they
must stay readable when the database, and so API key checks, are down.

**Decision:** `GET /metrics` is served in the Prometheus text format on
its own listener, `metrics.bind`, never on the public port, and needs no
API key. Without `[metrics]` no second port is opened; `metrics.bind`
must differ from `server.bind`, and it is bound before the public port,
so a clash stops startup. The counters are `requests_total` by final
outcome, `provider_attempts_total` by provider and outcome, and
`accounting_failures_total`. They count requests that reached a provider,
like request records (ADR-027). Labels are only provider names and
outcomes, never client keys, aliases, or anything else a client sends.
Counters are kept in memory with no metrics dependency. This completes
the metric that ADR-027 deferred; the optional background retry from
ADR-009 remains deferred.

**Consequences:** The public port exposes nothing new, and monitoring
keeps working during a database outage. Network placement becomes the
protection: an operator who binds `metrics.bind` to a public address
exposes the page. Counters restart from zero with the process, which
Prometheus handles. Latency histograms can be added when needed.

## ADR-031 --- Per-key rate limits: in-memory token buckets

**Status:** Accepted.

**Context:** Any valid API key can send unlimited requests, and each one
can spend provider credit. A buggy client loop or a leaked key needs a
fast, cheap stop. FR-12 requires per-key limits; the storage and counting
method were open.

**Decision:** Each API key has a token bucket holding up to `burst`
tokens, refilled at `requests_per_minute` (both default to 60). Buckets
live in memory behind a `RateLimiter` trait. The limit is checked right
after authentication and before any other work. Every authenticated
request spends one token, including requests later rejected as invalid;
the gateway's retries and fallback spend none, since a provider's
failure is not the client's fault, and `max_attempts` already bounds
them. Over the limit, the client gets `429 rate_limited` with
`Retry-After` in whole seconds, rounded up; nothing reaches a provider,
no request record is written, and `rate_limited_total` is incremented.
All keys share the configured limit until per-key policies exist.

**Consequences:** The check adds no database work and no failure mode,
and it keeps working when PostgreSQL is down. A token bucket has no
window boundary to exploit: no moment lets through more than `burst`.
Only authenticated keys get a bucket, so memory is bounded by the number
of real keys. A restart refills every bucket, and each server counts
separately, so several servers multiply the effective limit; running
more than one needs a shared `RateLimiter`, with its own deadline.
This fits a limit that protects the next few seconds. Budgets, which
protect money over longer periods, must be durable and will use
PostgreSQL.

## ADR-032 --- Per-key concurrency limits: refuse, never queue

**Status:** Accepted.

**Context:** A rate limit counts arrivals, not work in progress. A
request can run for up to `request_timeout`, so when a provider hangs, an
allowed arrival rate piles up as open requests, each holding a connection
and memory. SECURITY.md requires bounded concurrency; what to count, and
what to do at the limit, were open.

**Decision:** Each API key may have `concurrency.per_key` requests running
at once (default 10). The count lives in memory, in a concrete
`ConcurrencyLimiter`: requests running on a server are that server's own
load, so no shared store will replace it, and it needs no trait. A request
takes a seat after the rate limit, and holds it until it ends; the seat
frees itself when dropped, so success, failure, a panic, and a client
disconnect all return it. Retries and fallback share the request's seat.
With no free seat the client gets an immediate `429 concurrency_limited`:
no queue, no provider call, no request record, and
`concurrency_limited_total` is incremented. A refused request has already
spent its rate-limit token, since it did arrive. There is no `Retry-After`,
because nobody knows when a running request will finish, and a guess would
tell clients to retry on a schedule that may not work; `Retry-After` keeps
meaning that waiting that long will work.

**Consequences:** A key never has more than `per_key` requests running on
one server, and one bad or leaked key cannot crowd out the others. A
separate code and counter tell a slow provider
(`concurrency_limited_total`) apart from a busy client
(`rate_limited_total`). A client that sends more than `per_key` at once is
refused even on a healthy day, so the default needs headroom. There is no
server-wide cap: the total is at most keys × `per_key`, and only an
operator can create keys. One can be added later without changing this
limit, and should be if the number of keys grows. Today's responses are
complete when the handler returns, so that is when the seat frees;
streaming must hold the seat until the stream ends.

## ADR-033 --- Optional key expiry; `ms_live_` is a label

**Status:** Accepted.

**Context:** The SRS requires rejecting expired keys, and ADR-012 promises
`ms_live_` keys, but keys could only be `ms_dev_` and only ended by
revocation. Open questions were whether keys expire by default, and
whether the server should treat the two prefixes differently.

**Decision:** Expiry is optional. `keys create --expires-in-days N`
(1--3650) sets `expires_at`; without it the column stays NULL and the key
works until revoked, so existing keys are unaffected. An expired key gets
the same `401` as any other bad key, with `expired` logged as the reason;
revoked wins when both apply. Authentication compares `expires_at` with
the database's `now()` in the existing key lookup, and `expires_at` is
set from the same clock, so there is one clock and no extra query.
`keys list` shows an `EXPIRES` column and an `expired` status. `keys create
--live` makes an `ms_live_` key; `ms_dev_` stays the default. The prefix is
a label for people and secret scanners: the server accepts both alike and
has no environment setting.

**Consequences:** A key can only expire because someone chose that, so a
date never surprises a running service, which matters because the gateway
has no way to warn anyone first. A key created without the flag lasts until
revoked, so temporary keys rely on the operator using it; a default limit
can follow once warnings exist. A dev key cannot authenticate on a live
server only because each environment has its own database and so its own
keys. Sharing one database across environments would remove that protection
with nothing to catch it, so it must not be done.

## ADR-034 --- Versioned prices; cost from reported usage only

**Status:** Accepted.

**Context:** ADR-004 and ADR-010 require cost to be a labeled estimate
tied to a pricing version. Where prices live, what to do without a price,
and how to treat failed attempts were open.

**Decision:** Prices live in `pricing_versions`, in USD per million
tokens, and are set with `modelswitch pricing set`, which needs a source
and a configured model. Setting a price closes the previous row at the
same moment; rows are never edited. A request's `estimated_cost` is its
reported input and output tokens × the price in effect when it started,
and `pricing_version` names that price. It covers only the attempt that
answered, because failed attempts report no tokens. If no price applies,
or either token count is missing, both columns stay null: unknown is
never recorded as zero, while a price of zero is a deliberate real zero.
The request is served either way. A request with usage but no price
increments `unpriced_requests_total`. The estimate is calculated in
PostgreSQL inside the record's transaction: `numeric` is exact, so no
decimal library or floating point is involved, the price and `created_at`
share one clock, and the accounting deadline already covers it. Prices
are not exposed to clients.

**Consequences:** An estimate can be reproduced from its record and price
row, and a price change never alters old records. Because the estimate is
in SQL, cost calculation and price selection are tested against
PostgreSQL, not as pure unit tests. Prices are not reviewed in pull
requests, and each environment sets its own; nothing fetches them, so
they go stale unless an operator updates them. Totals leave out unpriced
requests and run low when failed attempts were billed, most likely during
provider trouble; `request_attempts` shows which requests had failed
attempts. Budgets must decide how to treat both gaps.

## ADR-035 --- Monthly budgets: firm, per key, checked before each call

**Status:** Accepted. Replaces ADR-010's plan for `estimated_*` token
columns.

**Context:** Rate and concurrency limits bound speed, not money, so a
leaked key could spend without limit while staying inside both. FR-12
requires budget enforcement to document its consistency and failure
behavior. A request's cost is only known after it finishes, but the
decision to allow it comes before.

**Decision:** Each key may have a `monthly_budget_usd`, set with `keys
budget`; NULL means no budget, so existing keys are unaffected. A budget
covers a calendar month in UTC. Spending is the sum of the key's
`estimated_cost` this month by `created_at`, read from `request_records`,
so nothing is stored twice and a restart loses nothing. The check runs
after model resolution and before any provider call, in one query, under
`accounting_timeout`. Spending at or over the budget gets `429
budget_exceeded`, with `Retry-After` counting down to the next month, and
increments `budget_exceeded_total`. A budgeted key whose candidates are
not all currently priced gets `422 model_not_priced`, because spending it
cannot see would silently bypass the budget; a budget that is already
spent is reported first. Refused requests reach no provider and write no
record. If the check fails or times out, the request is refused with
`503` (ADR-009). Changing a budget applies from the next request.

**Consistency guarantees:** A budget is firm, not exact. The check is not
atomic with the spending, so requests running at the same moment all see
the same total, and the request that crosses the limit completes. A key
can overshoot by the cost of the requests already running: at most
`concurrency.per_key` per server. All servers read the same records, so
none can reset or hide spending. Reservations were rejected: they need a
guessed worst-case cost, refuse clients who have budget left, and leave
money stuck after a crash.

**Consequences:** A budget tracks known spending, so a real bill can be
higher. Failed attempts report no tokens and an answer without usage has
no cost, so neither is counted, though the rate and concurrency limits
still bound them. A forgotten price stops a budgeted key until `pricing
set` is run, and every candidate needs a price, so a missing fallback price
is found at once instead of during an outage. A key can spend up to twice
its budget across a month boundary. A key with no budget is unlimited, so
capping a total bill needs a budget on every key. Each budgeted request
sums the key's records for the month; a running total can replace that if
volumes make it slow. There is no server-wide budget yet.

## ADR-036 --- Dashboard: operator only, on its own port, with sessions

**Status:** Accepted.

**Context:** Stage 4 adds a dashboard of request history. The records
show every client's traffic and spending, and ADR-012 left key
management on the server until an authenticated admin interface exists.
The dashboard needs a read API, a way to log in, and a way to reach the
browser, without weakening the public port.

**Decision:**

-   *Who:* only the operator. One admin token, at least 32 characters,
    is read from the environment variable `admin.token_env` names. It is
    separate from client API keys, so a leaked dashboard login cannot
    call `/v1/generate`, and a leaked API key cannot open the dashboard.
    The dashboard starts read-only: changing keys or prices from it
    needs audit records first.
-   *Where:* its own listener, `admin.bind` (`127.0.0.1:9091` in the
    example), opened only when `[admin]` is configured, as metrics is
    (ADR-030). It must differ from `server.bind` and `metrics.bind`. The
    network keeps strangers away and the token checks the operator.
    Remote access goes through an SSH tunnel or a reverse proxy.
-   *Login:* `POST /admin/api/login` checks the token once by comparing
    SHA-256 hashes on every byte. The browser then holds only a random
    32-byte session ID in a cookie that is `HttpOnly` (page scripts
    cannot read it), `SameSite=Strict` (other sites cannot make the
    browser send it), limited to `/admin/api`, and expires after
    `admin.session_hours` (default 12, at most 168). Using a session
    does not extend it. Sessions are kept in memory as hashes, at most
    64, so a restart logs the operator out. Login and logout also
    require an `Origin` matching `Host`. After 5 failed logins in a
    minute, every login is refused with `429 login_throttled` and
    `Retry-After` until the oldest failure is a minute old; failures
    are logged and counted in `admin_login_failures_total`. The cookie
    has no `Secure` flag because the listener speaks plain HTTP on this
    machine; a TLS proxy in front can add it.
-   *History API:* `GET /admin/api/requests` lists records newest
    first, filtered by time, provider, and status, and keyset-paged on
    `(created_at, id)` so pages never repeat or skip a request while new
    ones arrive. `GET /admin/api/requests/{id}` adds the attempts and
    the price used. Filters are checked before the database is asked,
    each read runs under `admin.database_timeout_ms` (default 3000), and
    failures are `503`. Costs are exact decimal strings, because JSON
    numbers would round them; times are UTC.
-   *Serving the page:* the dashboard is built with Node into plain
    files, and the admin listener serves them from
    `admin.dashboard_dir`: `index.html` at `/` and `assets/` beneath
    it, nothing else in the folder. Page responses carry a strict
    Content Security Policy (same origin only, no inline scripts, no
    framing). The server refuses to start if the folder has no
    `index.html`. Rust builds and tests never need Node. During
    development, Vite serves the page and forwards `/admin/api` to the
    admin port, keeping the `Host` header so the `Origin` check passes.
-   *Routing reason:* shown from what is already stored: the requested
    model (alias or pin) and the attempt chain, such as "rejected key,
    fell back, answered". A stored `routing_reason` waits for Stage 5,
    when adaptive routing makes decisions that attempts cannot explain.
-   *Frontend:* Vite, React, TypeScript, Vitest with Testing Library,
    plain CSS, and the browser's `fetch`. CI type-checks, tests, builds,
    and audits it.

**Consequences:** The public port and its middleware are unchanged. The
token never reaches page scripts, and a stolen cookie stops working
when it expires or the operator logs out. A restart means logging in
again. Only one operator role exists, so the dashboard has no per-user
views; per-client access would need its own decision. Admin responses
carry `Cache-Control: no-store`.
