# Testing Strategy

## Unit tests

Test logic without network/database dependencies: request validation and
limits, capability matching, deterministic routing, retry classification
and bounds, cost calculation and pricing-version selection, HTTP error
mapping, and redaction helpers.

## Provider adapter contract tests

Use a mock HTTP server to test request serialization, auth headers,
success parsing, missing/malformed usage, rate limits, 5xx responses,
malformed JSON, and timeouts. CI should not depend on paid live provider
calls; keep optional live smoke tests separate and credential-gated.

## Integration tests

Use disposable PostgreSQL and mock providers to verify authenticated
end-to-end generation, request/attempt persistence, invalid keys causing
no upstream call, request ID and client correlation ID propagation,
alias fallback versus pinned targets, provider failure/fallback, and the
documented accounting-failure policy.

Integration tests use `#[sqlx::test]`, which creates a fresh database
per test and runs migrations, against a local PostgreSQL 18 server or
the CI service container (ADR-014). Tests never share database state.

## Failure-injection tests

Simulate provider delay, 429/5xx, connection reset, malformed response,
no eligible candidates, database outage before a provider call (expect
`503` and zero provider calls), and database outage after successful
generation (expect the response, the accounting-failure header, and
exactly one provider call). Assert bounded elapsed time, maximum attempt
count, correct final outcome, and no accidental duplicate generation.

## Load tests

Use a fixed workload. Record concurrency, throughput, error/timeout
rate, gateway overhead, database pool behavior, CPU, and memory. Publish
environment, workload, sample size, and results. Do not advertise
unmeasured performance claims.

## CI gate

1.  `cargo fmt --check`
2.  `cargo clippy` with an agreed warning policy
3.  Unit tests
4.  Integration tests with PostgreSQL 18/mock providers
5.  Build
6.  Docker image build
7.  Secret scanning and dependency review

Add frontend checks once the dashboard exists.

## Definition of done

Behavior is specified as test cases before implementation (ADR-016).
A feature is done when behavior and edge cases are documented, tests
cover success and relevant failures, errors are typed/sanitized,
telemetry is useful without leaking secrets, documentation is updated,
and the diff is understandable by its author.
