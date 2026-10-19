# ModelSwitch --- Software Requirements Specification (SRS)

-   Version: 0.2
-   Status: Proposed (design decisions recorded in `DECISIONS.md`)
-   **MUST** = required for this release; **SHOULD** = strongly
    recommended; **MAY** = optional.

## 1. Purpose

Define functional and non-functional requirements for ModelSwitch, a
multi-provider LLM inference gateway. This document guides
implementation, testing, and review.

## 2. Product perspective

ModelSwitch sits between client applications and external model
providers. It authenticates requests, validates requirements, selects a
compatible provider, executes under reliability policies, records
metadata, and returns a normalized result.

## 3. User classes

-   Client application
-   Operator
-   Maintainer
-   Security/reliability reviewer

## 4. Assumptions and constraints

-   Initial release is one Rust service backed by PostgreSQL.
-   Provider credentials come from secure deployment configuration.
-   Provider APIs, model names, rate limits, and pricing can change and
    must be verified before release.
-   Initial release supports non-streaming text generation.
-   Initial providers use free tiers or run locally (Groq, Gemini,
    Ollama); their limits and terms can change.
-   External provider availability and output quality are outside the
    gateway's direct control.

## 5. Functional requirements

### FR-01 --- Request validation

The service MUST validate required fields, supported capabilities,
request size, and configured limits before contacting a provider.
**Verification:** tests for missing fields, invalid values, oversized
requests, and unsupported capabilities.

### FR-02 --- Client authentication

Protected inference requests MUST require a valid gateway API key.
Invalid, revoked, or expired keys MUST be rejected before provider
invocation. **Verification:** integration tests confirm unauthorized
requests cause no mock-provider call.

### FR-03 --- Provider abstraction

Provider-specific transport details MUST be isolated behind an internal
adapter contract. **Verification:** adapter contract tests run against
each adapter.

### FR-04 --- Multi-provider support

The first release MUST support at least two provider adapters: an
OpenAI-compatible adapter (used for Groq and Ollama) and a Gemini
adapter. **Verification:** end-to-end tests exercise each adapter.

### FR-05 --- Capability compatibility

Adapters MUST declare supported capabilities. Routing MUST NOT select a
provider that lacks a required capability. **Verification:** tests cover
compatible selection and no-compatible-provider behavior.

### FR-06 --- Routing

Provider selection MUST follow an explicit, versioned policy. The
decision MUST record a reason code and policy version. **Verification:**
deterministic tests cover eligibility and candidate ordering.

### FR-07 --- Timeouts

Every upstream attempt MUST have a configurable deadline.
**Verification:** delayed mock-provider tests verify bounded completion.

### FR-08 --- Retry and fallback

Retries and fallback MUST be finite, policy-controlled, and
deadline-aware. Validation and authentication errors MUST NOT be
retried. **Verification:** failure-injection tests assert attempt counts
and error classification.

### FR-09 --- Stable errors

Gateway errors MUST use a documented JSON shape and stable error codes.
Responses MUST NOT expose secrets, stack traces, or unfiltered upstream
bodies. **Verification:** contract and redaction tests.

### FR-10 --- Request history

The system MUST persist request outcome, provider/model, timing, and
timestamps. If the database is unavailable before the provider call, the
request MUST be rejected without a provider call. If the record cannot
be written after a successful generation, the response MUST still be
returned with an accounting-failure header, log, and metric, and MUST
NOT trigger another generation. **Verification:** integration tests
inspect records after success and failure; failure-injection tests
cover both database-outage cases.

### FR-11 --- Usage and cost

Provider-reported usage MUST be stored when available. Estimated cost
MUST be distinguishable from reported billing data and identify the
pricing version. **Verification:** tests cover known usage, missing
usage, and pricing-version changes.

### FR-12 --- Limits

Before multi-user/public deployment, the service MUST support per-key
request limits. Budget enforcement MUST document its consistency and
failure behavior.

### FR-13 --- Health

The service MUST expose liveness and readiness endpoints without
disclosing secrets.

### FR-14 --- Auditability

Operator changes to keys, providers, and routing policies SHOULD be
auditable with actor and timestamp.

### FR-15 --- Model selection

The request `model` MUST accept an operator-defined alias or a
`provider/model` pin. Aliases MAY fall back down their candidate list;
pins MUST NOT fall back to another target. Responses MUST report the
provider and resolved model that ran. **Verification:** routing tests
cover alias fallback, `routing.fallback = false`, pins, and unknown
aliases or pins.

### FR-16 --- Request identification

The gateway MUST generate a time-sortable request ID for every request
and return it in `X-Request-Id`. An optional `X-Client-Request-Id` MUST
be validated, stored, and echoed. **Verification:** tests cover ID
presence on success and error, and rejection of invalid correlation IDs.

## 6. Non-functional requirements

-   **NFR-01 Security:** secrets MUST not be committed or logged; stored
    client keys MUST be hashed; outbound endpoints MUST be restricted to
    configured providers.
-   **NFR-02 Reliability:** upstream calls MUST have deadlines; retries
    MUST be finite; failure paths MUST be tested.
-   **NFR-03 Maintainability:** API, routing, providers, reliability,
    accounting, and storage MUST have clear module boundaries.
-   **NFR-04 Observability:** structured logs MUST include request ID,
    latency, outcome, provider, and error category; prompt/completion
    text MUST be excluded by default.
-   **NFR-05 Performance:** publish targets only after reproducible
    baseline tests. Measure gateway overhead separately from provider
    latency.
-   **NFR-06 Portability:** local setup and prerequisites SHOULD be
    documented and repeatable.
-   **NFR-07 Data integrity:** records MUST distinguish success,
    failure, timeout, rejection, and accounting failure.
-   **NFR-08 Dashboard usability:** if added, the dashboard SHOULD
    provide keyboard navigation, readable contrast, clear units, and
    useful empty/error states.

## 7. Initial API

-   `POST /v1/generate` --- authenticated non-streaming inference.
-   `GET /health/live` --- process liveness.
-   `GET /health/ready` --- readiness of required dependencies.
-   Responses MUST include a request ID. Errors MUST contain a stable
    `error.code`.
-   See `API.md` for the full contract.

## 8. Release 0.1 acceptance criteria

Release `v0.1.0` covers roadmap Stages 0--2 (ADR-013).

1.  A valid request can reach either configured provider.
2.  Invalid keys are rejected before provider invocation.
3.  Unsupported capabilities are not silently ignored.
4.  Upstream calls respect deadlines.
5.  Retry/fallback attempts are bounded and recorded.
6.  Usage/cost records do not fabricate missing token counts.
7.  Logs contain no API keys or prompt content by default.
8.  CI runs formatting, lint, unit/integration tests, and build.
9.  Setup and test instructions work from a clean clone.

## 9. Traceability

| Requirement | Main area | Evidence |
|---|---|---|
| FR-01 | API validation | Validation tests |
| FR-02 | Auth | Auth integration tests |
| FR-03--05 | Provider adapters | Adapter contract tests |
| FR-06, FR-15 | Router | Routing tests |
| FR-07--08 | Reliability | Timeout/failure tests |
| FR-09, FR-16 | Error mapping, request IDs | Response contract tests |
| FR-10--11 | Accounting/storage | Database and failure-injection tests |
| FR-12 | Limits | Rate-limit tests |
| FR-13 | Health | Health endpoint tests |
| NFR-01--04 | Cross-cutting | Security and observability checks |
