-- Request records (ADR-009, ADR-027): one row per request that reached a
-- provider. Never holds prompts or outputs (ADR-005).
--
-- Token counts are exactly what the provider reported; unreported counts
-- stay NULL with usage_source = 'unavailable' (ADR-010). They are bigint
-- because a provider may report any unsigned 32-bit count.

CREATE TABLE request_records (
    -- Gateway request ID: `req_` + UUIDv7, so it sorts by time.
    id                      text        PRIMARY KEY
                                        CHECK (id ~ '^req_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    client_request_id       text        CHECK (char_length(client_request_id) BETWEEN 1 AND 128),
    client_api_key_id       uuid        NOT NULL REFERENCES client_api_keys (id),
    -- The alias or `provider/model` pin as sent, then what actually ran.
    requested_model         text        NOT NULL,
    selected_provider       text        NOT NULL,
    selected_model          text        NOT NULL,
    status                  text        NOT NULL
                                        CHECK (status IN ('succeeded', 'failed', 'timed_out')),
    -- Short identifiers only, never provider messages.
    error_kind              text        CHECK (error_kind IN ('request', 'status', 'invalid_response', 'blocked')),
    error_code              text        CHECK (char_length(error_code) BETWEEN 1 AND 64),
    provider_status         integer     CHECK (provider_status BETWEEN 100 AND 599),
    latency_ms              bigint      NOT NULL CHECK (latency_ms >= 0),
    reported_input_tokens   bigint      CHECK (reported_input_tokens >= 0),
    reported_output_tokens  bigint      CHECK (reported_output_tokens >= 0),
    reported_total_tokens   bigint      CHECK (reported_total_tokens >= 0),
    usage_source            text        NOT NULL
                                        CHECK (usage_source IN ('provider_reported', 'unavailable')),
    created_at              timestamptz NOT NULL,
    completed_at            timestamptz NOT NULL DEFAULT now(),
    -- A failure always says what kind; successes and timeouts carry no
    -- error details.
    CHECK (CASE status
               WHEN 'failed' THEN error_kind IS NOT NULL
               ELSE error_kind IS NULL AND error_code IS NULL AND provider_status IS NULL
           END)
);

-- Usage over time, overall and per client key, and failures over time.
CREATE INDEX request_records_created_at ON request_records (created_at);
CREATE INDEX request_records_key_created_at ON request_records (client_api_key_id, created_at);
CREATE INDEX request_records_status_created_at ON request_records (status, created_at);
