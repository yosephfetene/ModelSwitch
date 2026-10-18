# ModelSwitch --- Software Design Report (SDR)

-   Version: 0.1
-   Status: Proposed design
-   Audience: implementers, reviewers, maintainers, and technical
    recruiters

## 1. Executive summary

ModelSwitch is an API gateway for sending inference requests to multiple
large-language-model providers through one entry point. It centralizes
provider integration, policy-based routing, bounded retries, failover,
usage accounting, and operational visibility.

The first version should be a **modular monolith**: one Rust service
with clear internal module boundaries. This keeps deployment and
debugging manageable while allowing the codebase to evolve.

## 2. Problem statement

Applications integrating multiple model providers often duplicate
provider-specific request code, error handling, timeout logic, usage
tracking, and cost estimates. ModelSwitch centralizes these concerns
while preserving provider-specific capabilities instead of pretending
all providers are identical.

## 3. Objectives and success criteria

  -----------------------------------------------------------------------
  Objective                           Evidence
  ----------------------------------- -----------------------------------
  Consistent API                      Contract tests validate request and
                                      response schemas

  Multi-provider support              Two adapters pass end-to-end tests

  Bounded failure handling            Tests prove deadlines and retry
                                      limits are enforced

  Usage accounting                    Records distinguish reported usage
                                      from estimates

  Secure access                       Invalid keys rejected;
                                      secrets/content not logged by
                                      default

  Explainable routing                 Decisions include policy version
                                      and reason code

  Maintainability                     Modules have clear responsibilities
                                      and tests
  -----------------------------------------------------------------------

## 4. Scope

### Initial release

-   Client API-key authentication
-   Non-streaming text generation
-   Two provider adapters
-   Explicit routing policy and eligible fallback
-   Timeouts and bounded retries
-   PostgreSQL request and attempt records
-   Structured logs, basic metrics, health checks
-   Unit, integration, and provider contract tests
-   Docker-based local setup and CI

### Later

-   Streaming
-   Tool/function calling and structured output
-   Per-key quotas and budgets
-   React/TypeScript dashboard
-   Benchmark-informed routing
-   Additional provider-specific capabilities

### Out of scope initially

-   Training or hosting models
-   Guaranteeing identical output across providers
-   Unbounded retries
-   User-supplied arbitrary upstream URLs
-   Distributed microservices and multi-region coordination

## 5. Users and stakeholders

-   **Client developer:** wants one stable integration point.
-   **Operator:** configures providers, credentials, policies, and
    limits.
-   **Service owner:** investigates failures and monitors cost, latency,
    and availability.
-   **Security reviewer:** evaluates credential handling, authorization,
    privacy, and retention.

## 6. Architecture

1.  **HTTP/API layer:** validates requests and maps errors to HTTP
    responses.
2.  **Authentication/policy layer:** authenticates keys and applies
    client restrictions.
3.  **Routing layer:** selects an eligible provider and model.
4.  **Provider adapters:** translate between internal and
    provider-specific schemas.
5.  **Reliability layer:** enforces deadlines, bounded retries, and
    eligible fallback.
6.  **Accounting layer:** captures outcome, attempts, latency, usage,
    and estimated cost.
7.  **Persistence layer:** stores request history, attempts, policies,
    and pricing versions.
8.  **Observability layer:** emits structured logs and metrics.

## 7. Key design choices

### Modular monolith first

One deployable service is simpler to run and test. Enforce module
boundaries through types and dependency direction, not network calls.

### Internal model plus adapters

Normalize common behavior, but model capabilities explicitly. If a
provider cannot support a requested feature, reject the request or route
to a compatible provider; never silently discard the feature.

### Rule-based routing first

Start with deterministic, versioned policies. Add benchmark-based
routing only after a representative workload and measurable objective
exist.

### Bounded retries and fallback

Retry only eligible transient failures, within a deadline and configured
attempt limit. A timeout does not prove the provider did no work;
retries may duplicate cost. Record every attempt.

### Facts versus estimates

Store provider-reported usage separately from estimated cost. Cost
estimates must identify the pricing version used. Missing usage must
remain unknown, not fabricated.

## 8. Request lifecycle

1.  Assign or validate a request ID.
2.  Authenticate the client key.
3.  Validate request fields, size, capabilities, and limits.
4.  Resolve routing policy and eligible candidates.
5.  Record the selected provider and decision reason.
6.  Call the provider under a deadline.
7.  Retry or fall back only when policy allows.
8.  Normalize the response or return a stable sanitized error.
9.  Persist final outcome, attempts, latency, usage, and cost estimate.
10. Emit telemetry and return the response.

Persistence failure requires a deliberate policy. A failed history write
after successful generation must not accidentally cause a second
generation.

## 9. Privacy and data handling

-   Store hashes of client API keys, never plaintext keys.
-   Inject provider credentials through environment configuration or a
    secret manager.
-   Do not log prompts or completions by default.
-   Define retention/deletion policies before collecting production
    traffic.
-   Avoid storing request content unless explicitly enabled and
    access-controlled.

## 10. Failure model

  -----------------------------------------------------------------------
  Failure                             Expected behavior
  ----------------------------------- -----------------------------------
  Invalid client key                  Reject before provider call

  Invalid request                     Stable 4xx error; no retry

  Provider timeout                    Stop at deadline; fallback only if
                                      eligible

  Provider 429/5xx                    Bounded policy-based handling

  Malformed provider response         Treat as upstream failure; do not
                                      leak raw body

  No eligible provider                Return clear routing error

  Limit exceeded                      Reject before provider call where
                                      possible

  Database unavailable                Follow documented persistence
                                      policy and emit telemetry
  -----------------------------------------------------------------------

## 11. Deployment

For development, run the Rust service and PostgreSQL locally, preferably
with Docker Compose. CI should run formatting, linting, tests, and build
checks. Production requires secure secret injection, TLS termination,
database backups, and safe health/readiness endpoints.

## 12. Risks and mitigations

  -----------------------------------------------------------------------
  Risk                                Mitigation
  ----------------------------------- -----------------------------------
  Provider APIs diverge               Versioned adapters and contract
                                      tests

  Fallback adds cost                  Explicit eligibility, attempt
                                      records, per-request limits

  Pricing becomes stale               Versioned price data with source
                                      and retrieval timestamp

  Logs expose sensitive data          Metadata-first logging and
                                      redaction tests

  Scope expands too quickly           Complete one vertical slice at a
                                      time

  Router optimizes the wrong metric   Define workload and objective;
                                      publish trade-offs
  -----------------------------------------------------------------------

## 13. Open questions

-   Which two providers should be implemented first?
-   How will client keys be created and revoked?
-   Should accounting persistence fail closed or allow a response with
    an accounting warning?
-   What retention period is appropriate?
-   Which authoritative pricing sources and update process will be used?

## 14. First milestone acceptance gate

A client can submit a valid request, either configured provider can
process it, the documented response is returned, a request outcome is
stored, and tests cover success, invalid input, upstream failure, and
timeout.
