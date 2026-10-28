# Initial API Contract

This is a proposed v0.1 contract; finalize it in code and contract tests
before calling it stable.

## Authentication

Protected routes use a gateway API key:

``` http
Authorization: Bearer ms_live_REDACTED
```

This is a placeholder, not a real key. Keys use an environment prefix
(`ms_dev_` or `ms_live_`) followed by 32 random bytes in base62. They are
shown once on creation, stored only as SHA-256 hashes, and can be
revoked (see ADR-012).

## `POST /v1/generate`

### Example request

``` json
{
  "model": "fast-cheap",
  "input": "Summarize the main trade-offs in this design.",
  "routing": { "fallback": true },
  "max_output_tokens": 500,
  "metadata": { "feature": "design-review" }
}
```

-   `model`: either an operator-defined **alias** or a
    **`provider/model` pin** (see "Model selection" below).
-   `input`: text input for the first release.
-   `routing.fallback`: optional, default `true`. When `false`, an
    alias uses only its first eligible candidate. Ignored for pins,
    which never fall back.
-   `routing.policy`: reserved; semantics are decided in Stage 1.
    Clients cannot override privileged operator rules.
-   `max_output_tokens`: optional validated output limit.
-   `metadata`: optional bounded, non-sensitive metadata.

### Model selection

Operators define aliases in configuration as ordered candidate lists:

``` toml
[aliases.fast-cheap]
requires = []
candidates = [
  "groq/llama-3.3-70b",
  "gemini/gemini-flash",
  "ollama/llama3.2",
]
```

Model names above are illustrative; verify current provider model names
when configuring.

| `model` value | Meaning | Fallback |
|---|---|---|
| `fast-cheap` | Alias: any eligible candidate, in order | Down the candidate list, within policy limits |
| `groq/llama-3.3-70b` | Pin: exactly this provider and model | Never to another target; same-target retries per policy |

Candidates that lack a capability listed in `requires` are never
selected. Unknown aliases and unknown or unconfigured pins are rejected
with `422`.

### Example success

``` json
{
  "id": "req_01JEXAMPLE",
  "model": "fast-cheap",
  "provider": "gemini",
  "resolved_model": "gemini-flash",
  "output": "Generated response text.",
  "usage": {
    "input_tokens": 120,
    "output_tokens": 80,
    "total_tokens": 200,
    "source": "provider_reported"
  },
  "latency_ms": 842
}
```

-   `model` echoes what the client requested; `provider` and
    `resolved_model` report what actually ran.
-   If usage is unavailable, the token fields are `null` and `source`
    is `"unavailable"`. Exact token counts are never invented
    (ADR-010).

### Headers

Request:

-   `Authorization: Bearer <key>` --- required.
-   `X-Client-Request-Id` --- optional client correlation ID, accepted
    on every route. Must be 1--128 characters from `[A-Za-z0-9._:-]`
    and sent at most once; otherwise the request is rejected with `400`
    `invalid_request`, and the rejected value is neither logged nor
    echoed. When valid it is stored, logged, and echoed.
-   `X-Request-Id` --- ignored if sent; the gateway always generates
    its own (ADR-011).

Response:

-   `X-Request-Id` --- gateway-generated request ID: `req_` followed by
    a UUIDv7, so IDs sort by creation time. Present on every response
    from every route, including errors; on errors it equals the body's
    `request_id`.
-   `X-Client-Request-Id` --- echoed when the client sent a valid one.
-   `X-ModelSwitch-Accounting: failed` --- present when generation
    succeeded but the request record could not be written (ADR-009).
-   `Content-Type: application/json`.

### Example error

``` json
{
  "error": {
    "code": "upstream_timeout",
    "message": "The selected provider did not respond before the deadline."
  },
  "request_id": "req_019a2f4c-8e7b-7c3d-9f1a-2b3c4d5e6f70"
}
```

Every error, on every route, uses this shape. Clients branch on
`error.code`, which is stable; `error.message` is for humans and may
change. Codes implemented so far: `invalid_request` (400), `not_found`
(404), `method_not_allowed` (405), `internal_error` (500).

### Suggested status mapping

| Condition | HTTP status |
|---|---:|
| Invalid JSON, fields, or correlation ID | 400 |
| Missing/invalid/revoked key | 401 |
| Authenticated but not permitted | 403 |
| Unknown route | 404 |
| Route exists but not for this HTTP method | 405 |
| Unknown alias/pin, no eligible provider, or unsupported capability | 422 |
| Request limit or budget exceeded | 429 |
| Unexpected gateway failure | 500 |
| Upstream provider failure | 502 |
| Dependency needed before the provider call is unavailable (for example the database) | 503 |
| Upstream timeout | 504 |

## Health endpoints

-   `GET /health/live`: process is alive; no client key required.
-   `GET /health/ready`: required dependencies are ready according to
    deployment policy. Must not disclose credentials or connection
    strings.

## Contract rules

-   Enforce request-body size limits.
-   Never return provider credentials, stack traces, or raw upstream
    error bodies.
-   Keep public errors stable even if provider-specific errors change.
-   Streaming, tool calls, and structured output are excluded from the
    initial contract.
-   Idempotency keys are not supported in Release 0.1 (ADR-011).
