-- Benchmark runs and their samples (Stage 5, ADR-038). Kept apart from
-- request records, so benchmark calls never count toward live latency,
-- request history, or any key's budget. Verdicts, numbers, and times
-- only: never a prompt or an output (ADR-005).

CREATE TABLE bench_runs (
    id               uuid        PRIMARY KEY DEFAULT uuidv7(),
    -- The alias whose candidates were benchmarked.
    alias            text        NOT NULL CHECK (char_length(alias) BETWEEN 1 AND 128),
    -- The workload file's name and the SHA-256 of its bytes, so results
    -- can be traced to the exact prompts behind them.
    workload_name    text        NOT NULL CHECK (char_length(workload_name) BETWEEN 1 AND 255),
    workload_sha256  text        NOT NULL CHECK (workload_sha256 ~ '^[0-9a-f]{64}$'),
    repeats          integer     NOT NULL CHECK (repeats >= 1),
    -- When the results were stored; samples are priced as of this moment.
    created_at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX bench_runs_latest ON bench_runs (alias, created_at DESC);

CREATE TABLE bench_samples (
    run_id                  uuid    NOT NULL REFERENCES bench_runs (id) ON DELETE CASCADE,
    -- Order taken within the run, from 1.
    sample_number           integer NOT NULL CHECK (sample_number >= 1),
    item_id                 text    NOT NULL CHECK (item_id ~ '^[a-z0-9-]{1,64}$'),
    repeat                  integer NOT NULL CHECK (repeat >= 1),
    provider                text    NOT NULL CHECK (char_length(provider) >= 1),
    model                   text    NOT NULL CHECK (char_length(model) >= 1),
    status                  text    NOT NULL
                                    CHECK (status IN ('succeeded', 'failed', 'timed_out')),
    -- Answered, and the answer passed its check.
    passed                  boolean NOT NULL,
    error_kind              text    CHECK (error_kind IN ('request', 'status', 'invalid_response', 'blocked')),
    error_code              text    CHECK (char_length(error_code) BETWEEN 1 AND 64),
    provider_status         integer CHECK (provider_status BETWEEN 100 AND 599),
    latency_ms              bigint  NOT NULL CHECK (latency_ms >= 0),
    -- As the provider reported them; NULL when not reported (ADR-010).
    reported_input_tokens   bigint  CHECK (reported_input_tokens >= 0),
    reported_output_tokens  bigint  CHECK (reported_output_tokens >= 0),
    -- From reported usage and the price in effect at `bench_runs.created_at`
    -- (ADR-034). NULL when unpriced or usage is missing.
    estimated_cost          numeric CHECK (estimated_cost >= 0),
    pricing_version         uuid    REFERENCES pricing_versions (id),
    PRIMARY KEY (run_id, sample_number),
    CHECK (status = 'succeeded' OR NOT passed),
    CHECK (CASE status
               WHEN 'failed' THEN error_kind IS NOT NULL
               ELSE error_kind IS NULL AND error_code IS NULL AND provider_status IS NULL
           END),
    CHECK ((estimated_cost IS NULL) = (pricing_version IS NULL))
);
