# Initial API Contract

This is a proposed v0.1 contract; finalize it in code and contract tests
before calling it stable.

## Authentication

Protected routes use a gateway API key:

``` http
Authorization: Bearer ms_live_REDACTED
```

This is a placeholder, not a real key. Generate real keys securely, show
them once, store only hashes, and support revocation.

## `POST /v1/generate`

### Example request

``` json
{
  "model": "configured-model-alias",
  "input": "Summarize the main trade-offs in this design.",
  "routing": { "policy": "balanced" },
  "max_output_tokens": 500,
  "metadata": { "feature": "design-review" }
}
```

-   `model`: configured model alias or identifier; define exact
    semantics in implementation.
-   `input`: text input for the first release.
-   `routing.policy`: optional policy name; clients cannot override
    privileged operator rules.
-   `max_output_tokens`: optional validated output limit.
-   `metadata`: optional bounded, non-sensitive metadata.

### Example success

``` json
{
  "id": "req_01JEXAMPLE",
  "model": "configured-model-alias",
  "provider": "provider-a",
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

If usage is unavailable, represent it as null or omit it according to
the finalized schema. Do not invent exact token counts.

### Headers

-   `X-Request-Id`: gateway request identifier.
-   `Content-Type: application/json`.

### Example error

``` json
{
  "error": {
    "code": "upstream_timeout",
    "message": "The selected provider did not respond before the deadline."
  },
  "request_id": "req_01JEXAMPLE"
}
```

### Suggested status mapping

  Condition                                         HTTP status
  ----------------------------------------------- -------------
  Invalid JSON or fields                                    400
  Missing/invalid/revoked key                               401
  Authenticated but not permitted                           403
  Request limit or budget exceeded                          429
  No eligible provider / unsupported capability             422
  Upstream provider failure                                 502
  Upstream timeout                                          504
  Unexpected gateway failure                                500

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
