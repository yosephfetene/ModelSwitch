# Proposed Data Model

Conceptual model only. Choose exact SQL types, constraints, migrations,
and indexes during implementation.

## `client_api_keys`

-   `id` --- primary key / UUID
-   `name` --- operator-assigned label
-   `key_hash` --- unique hash of secret
-   `status` --- active, revoked, expired
-   `created_at`, `expires_at`, `revoked_at`
-   `rate_limit_policy_id` --- optional reference

Generate keys securely, display a new key once, and never log its
plaintext value.

## `request_records`

-   `id` --- gateway request ID
-   `client_api_key_id`
-   `requested_model`, `selected_provider`, `selected_model`
-   `routing_policy_version`, `routing_reason`
-   `status` --- succeeded, failed, timed out, rejected
-   `error_code` --- nullable
-   `latency_ms`
-   `input_tokens`, `output_tokens`, `total_tokens` --- nullable
-   `usage_source` --- provider_reported, estimated, unavailable
-   `estimated_cost`, `pricing_version`
-   `created_at`, `completed_at`

Do not store prompts/completions by default. If added later, define
opt-in, access controls, retention, and deletion behavior.

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

Finalize indexes based on query patterns. Define retention and deletion
for related records.

## Consistency notes

-   Use transactions for related record changes where appropriate.
-   Never hold a database transaction open during a provider call.
-   Use atomic database operations for cross-instance budgets and rate
    limits.
-   A database failure after successful generation must not cause
    duplicate generation.
