-- Request attempts (ADR-003, ADR-029): one row per provider call of a
-- recorded request, written in the same transaction as the request.
-- Identifiers and numbers only, never provider text (ADR-005).

CREATE TABLE request_attempts (
    request_id       text    NOT NULL REFERENCES request_records (id),
    -- 1 for the first call of the request.
    attempt_number   integer NOT NULL CHECK (attempt_number >= 1),
    provider         text    NOT NULL,
    model            text    NOT NULL,
    status           text    NOT NULL
                             CHECK (status IN ('succeeded', 'failed', 'timed_out')),
    error_kind       text    CHECK (error_kind IN ('request', 'status', 'invalid_response', 'blocked')),
    error_code       text    CHECK (char_length(error_code) BETWEEN 1 AND 64),
    provider_status  integer CHECK (provider_status BETWEEN 100 AND 599),
    -- How the failure was classified (ADR-028), and what happened next.
    error_class      text    CHECK (error_class IN ('transient', 'target_problem', 'request_problem')),
    retry_decision   text    NOT NULL
                             CHECK (retry_decision IN ('not_retried', 'retry', 'fallback', 'exhausted')),
    latency_ms       bigint  NOT NULL CHECK (latency_ms >= 0),
    PRIMARY KEY (request_id, attempt_number),
    -- A failure always says what kind; a success has no class, and only
    -- a failure carries error details.
    CHECK (CASE status
               WHEN 'succeeded' THEN error_class IS NULL
               ELSE error_class IS NOT NULL
           END),
    CHECK (CASE status
               WHEN 'failed' THEN error_kind IS NOT NULL
               ELSE error_kind IS NULL AND error_code IS NULL AND provider_status IS NULL
           END)
);
