# Security Requirements

## Assets

-   Provider credentials and client API keys
-   Client prompts, completions, and metadata
-   Usage records, cost estimates, and routing policy
-   Operational logs and deployment configuration

## Threats and controls

| Threat | Minimum control |
|---|---|
| Stolen client key | High-entropy key, hashed storage, revocation, redaction |
| Provider secret leak | Secret manager/environment injection; never commit or log |
| Cost abuse | Per-key limits, request caps, budgets, bounded retries |
| Resource exhaustion | Body-size limits, concurrency limits, deadlines, rate limits |
| SSRF/endpoint abuse | Allowlist configured provider endpoints; never accept arbitrary client URLs |
| Prompt leakage | No content logging by default; explicit retention/access policy |
| Log injection | Validate client-supplied values such as `X-Client-Request-Id` before logging |
| Error leakage | Stable sanitized errors; no raw upstream body or stack trace |
| Unauthorized policy change | Authenticated operator actions and audit records |
| Vulnerable dependency | CI dependency checks and documented updates |

## Client API keys

-   Format: `ms_dev_` or `ms_live_` followed by 32 random bytes from a
    cryptographically secure generator, encoded in base62. The prefix
    lets secret scanners recognize leaked keys.
-   Storage: SHA-256 hash plus a short display prefix; never the full
    key. A fast hash is appropriate because keys are high-entropy;
    slow password hashes (Argon2, bcrypt) defend low-entropy human
    secrets and would only add per-request latency (ADR-012).
-   Lifecycle: created and revoked with `modelswitch keys` CLI
    subcommands; shown once; revocation is checked on every request.
-   Verification: one uniform 401 for every bad key (the reason is only
    logged), keys of the wrong shape rejected before any database
    query, and fail closed with 503 when the key cannot be checked
    (ADR-023). Neither the key nor the `Authorization` header is logged.
-   Third-party provider data: free-tier providers may use submitted
    prompts; send only test data to them.
-   Outbound calls: provider URLs must use https (plain http only to
    localhost), redirects are never followed, provider responses are
    capped at 4 MiB, and provider error messages are discarded in favor
    of their short error code (ADR-025).
-   Provider keys are sent only in headers, never in URLs, and transport
    errors are logged without the URL. Model names that go into a URL
    path (Gemini) are restricted to plain identifiers and percent-encoded
    (ADR-026).

## Secret handling

-   Keep `.env` out of source control.
-   Commit `.env.example` with placeholders only.
-   Use separate development and production credentials.
-   Rotate/revoke exposed credentials.
-   Test that logs, tracing spans, panic messages, and error paths do
    not reveal secrets.
-   Never accept provider credentials from normal client request fields.

## Privacy

Default to metadata, outcomes, timing, and usage rather than
prompt/completion content. Any later content capture should be opt-in,
access-controlled, time-limited, and documented.

## Cost and abuse controls

-   Cap request size and output limits.
-   Bound concurrency and upstream deadlines.
-   Bound and record every retry/fallback attempt.
-   Apply per-key quotas and define budget consistency guarantees.
-   Prevent clients from overriding privileged routing policy.

## Release checklist

-   [ ] No committed secrets; secret scanning enabled
-   [ ] Client keys hashed and redacted
-   [ ] Unauthorized calls rejected before upstream invocation
-   [ ] Request-size and timeout limits configured
-   [ ] Retries and fallback bounded
-   [ ] Error responses sanitized
-   [ ] Logs tested for secret/content leakage
-   [ ] Dependencies/container image reviewed
-   [ ] Retention and deletion policy documented
