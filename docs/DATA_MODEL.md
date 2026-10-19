# Proposed Data Model

Conceptual model only. Choose exact SQL types, constraints, migrations,
and indexes during implementation.

## `client_api_keys`

-   `id` --- primary key / UUID
-   `name` --- operator-assigned label
-   `key_prefix` --- first characters of the key (for example
    `ms_dev_7Hq2`), for display only
-   `key_hash` --- unique SHA-256 hash of the full key
-   `status` --- active, revoked, expired
-   `created_at`, `expires_at`, `revoked_at`
-   `rate_limit_policy_id` --- optional reference (Stage 3)

Keys are `ms_dev_` or `ms_live_` followed by 32 random bytes in base62.
Generate keys securely, display a new key once, and never log its
plaintext value. A fast hash is sufficient because keys are
high-entropy (ADR-012).

## `request_records`

-   `id` --- gateway request ID (time-sortable, `req_` prefix)
-   `client_request_id` --- nullable, validated client correlation ID
-   `client_api_key_id`
-   `requested_model` --- alias or `provider/model` pin as sent
-   `selected_provider`, `selected_model` --- what actually ran
-   `routing_policy_version`, `routing_reason`
-   `status` --- succeeded, failed, timed_out, rejected
-   `error_code` --- nullable
-   `latency_ms`
-   `reported_input_tokens`, `reported_output_tokens`,
    `reported_total_tokens` --- nullable provider facts
-   `usage_source` --- provider_reported, unavailable
-   `estimated_cost`, `pricing_version` --- nullable until pricing
    exists (Stage 3)
-   `created_at`, `completed_at`

Do not store prompts/completions by default. If added later, define
opt-in, access controls, retention, and deletion behavior.

Missing usage stays null with `usage_source = unavailable`; it is never
fabricated. Stage 3 adds separate `estimated_input_tokens`,
`estimated_output_tokens`, and `estimate_method` columns used only for
budget enforcement (ADR-010).

### Accounting failures

When generation succeeds but this record cannot be written, the failure
cannot be recorded in the row itself. It is captured instead by a
structured `accounting_write_failed` log entry with the request ID, the
`accounting_failures_total` metric, and the
`X-ModelSwitch-Accounting: failed` response header (ADR-009). If an
optional background retry later writes the row, it is marked with
`recorded_late = true`.

## `request_attempts`

-   `id`, `request_id`, `attempt_number`
-   `provider`, `model`
-   `started_at`, `completed_at`, `latency_ms`
-   `outcome`, `provider_status_code`, `error_class`
-   `retry_decision` --- not_retried, retry, fallback, exhausted

This separates one logical request from individual provider attempts.

## `routing_policies`

-   `id`, `name`, `version`, `status`
-   validated rule configuration
-   `created_at`, `created_by`

Policy changes should be auditable and validated before activation.
Model aliases start in configuration files; whether they move into the
database is decided with routing policy storage.

## `pricing_versions`

-   `id`, `provider`, `model`
-   input/output price per documented unit
-   `effective_from`, `effective_to`
-   `source_reference`, `retrieved_at`

Pricing changes over time. Preserve historical versions so old estimates
remain explainable.

## Index candidates

-   `request_records(created_at)`
-   `request_records(client_api_key_id, created_at)`
-   `request_records(status, created_at)`
-   `request_attempts(request_id, attempt_number)`
-   `client_api_keys(key_hash)` --- unique, used on every request

Finalize indexes based on query patterns. Define retention and deletion
for related records.

## Consistency notes

-   Use transactions for related record changes where appropriate.
-   Never hold a database transaction open during a provider call.
-   Use atomic database operations for cross-instance budgets and rate
    limits.
-   A database failure after successful generation must not cause
    duplicate generation (ADR-009).
