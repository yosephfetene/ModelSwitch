-- Versioned prices and estimated cost (ADR-004, ADR-034).
--
-- A new price closes the old row instead of replacing it, so an estimate
-- can always be traced to the price it used. Prices and costs are exact
-- numerics in USD, never floating point. The upper bound on prices also
-- rejects NaN, which PostgreSQL sorts above every number.

CREATE TABLE pricing_versions (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    provider                text        NOT NULL CHECK (char_length(provider) >= 1),
    model                   text        NOT NULL CHECK (char_length(model) >= 1),
    input_usd_per_million   numeric     NOT NULL
                                        CHECK (input_usd_per_million >= 0
                                               AND input_usd_per_million < 1000000),
    output_usd_per_million  numeric     NOT NULL
                                        CHECK (output_usd_per_million >= 0
                                               AND output_usd_per_million < 1000000),
    -- In effect from `effective_from` up to, not including, `effective_to`.
    effective_from          timestamptz NOT NULL DEFAULT now(),
    effective_to            timestamptz,
    -- Where the price came from, and when it was entered.
    source_reference        text        NOT NULL
                                        CHECK (char_length(source_reference) BETWEEN 1 AND 2048),
    retrieved_at            timestamptz NOT NULL DEFAULT now(),
    CHECK (effective_to > effective_from)
);

-- At most one current price per model.
CREATE UNIQUE INDEX pricing_versions_current
    ON pricing_versions (provider, model) WHERE effective_to IS NULL;
-- Finding the price in effect at a moment, on every priced request.
CREATE INDEX pricing_versions_lookup
    ON pricing_versions (provider, model, effective_from);

-- NULL means unknown: no price, or usage the provider did not report
-- (ADR-010). A cost always names the price it used.
ALTER TABLE request_records
    ADD COLUMN estimated_cost   numeric CHECK (estimated_cost >= 0),
    ADD COLUMN pricing_version  uuid    REFERENCES pricing_versions (id),
    ADD CONSTRAINT request_records_cost_names_its_price
        CHECK ((estimated_cost IS NULL) = (pricing_version IS NULL));
