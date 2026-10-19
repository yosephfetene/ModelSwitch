# ModelSwitch Architecture

## 1. High-level request flow

``` text
Client
  -> Axum API: parse, validate, request ID
  -> Authentication and client policy
  -> Capability validation and routing
  -> Reliability executor (deadline / bounded retry / fallback)
  -> Provider adapter (OpenAI-compatible or Gemini)
  -> External model provider

Application service -> Accounting -> PostgreSQL
API and services -> Structured logs and metrics
```

## 2. Suggested Rust module layout

``` text
src/
  main.rs              # server and CLI entry point
  app.rs
  config.rs
  error.rs
  api/
    mod.rs
    generate.rs
    health.rs
  auth/
    mod.rs
    api_key.rs
  cli/
    mod.rs
    keys.rs            # modelswitch keys create|list|revoke
  domain/
    mod.rs
    request.rs
    response.rs
    provider.rs
    usage.rs
  routing/
    mod.rs
    alias.rs           # alias and provider/model pin resolution
    policy.rs
    selector.rs
  providers/
    mod.rs
    openai_compat.rs   # Groq, Ollama
    gemini.rs
  reliability/
    mod.rs
    executor.rs
    retry.rs
  accounting/
    mod.rs
    service.rs
  storage/
    mod.rs
    requests.rs
  observability/
    mod.rs
```

This is a proposed destination, not a requirement to create every file
immediately. Add modules when the first vertical slice needs them.

## 3. Dependency direction

-   HTTP handlers depend on application services and domain types.
-   Routing depends on provider metadata and policy, not Axum.
-   Provider adapters implement the internal provider contract.
-   Domain types should not depend on SQLx or HTTP implementation
    details.
-   Validate configuration at startup; invalid configuration should stop
    startup with a safe, actionable error.

## 4. Provider contract

The internal provider contract should express provider identity,
supported capabilities, supported models, an async generation operation,
normalized usage metadata, and typed errors with retry classification.
Do not force every provider into a fake universal feature set. Requests
requiring unsupported features should fail clearly or route to a
compatible provider.

An *adapter* implements one wire format; a *provider* is a configured
endpoint, credential, and model set that uses an adapter. The
OpenAI-compatible adapter serves both Groq and Ollama (ADR-008).

## 5. Routing

The request `model` is either an alias or a `provider/model` pin
(ADR-007). An alias resolves to an ordered candidate list from
configuration; a pin resolves to exactly one target and never falls
back to another target.

The initial router filters by availability and capabilities, applies
client/operator policy, orders candidates deterministically, records the
decision, and passes eligible alternatives to the reliability executor.
Adaptive routing is later work. First establish a representative
benchmark workload and define the optimization objective, such as
latency under a cost ceiling.

## 6. Failure semantics

-   A timeout does not prove the provider did no work.
-   Retrying may duplicate cost.
-   Fallback can return output from a different model.
-   Record each attempt and distinguish intermediate failures from the
    final outcome.
-   Never return raw provider error bodies to clients.
-   Do not allow an accounting write failure to accidentally trigger
    duplicate generation.
-   Fail closed before the provider call: if the database is needed for
    authentication or limit checks and is unavailable, reject with
    `503`. Fail open after a successful generation: return the response
    with `X-ModelSwitch-Accounting: failed` and emit a log and metric
    (ADR-009).

## 7. Concurrency and state

Use Tokio async execution and SQLx connection pooling. Do not keep
database transactions open while waiting on a provider. Use
database-backed atomic operations for budgets and limits that must hold
across multiple instances. In-memory counters are only suitable for
documented single-instance or best-effort behavior.
