-- Why an adaptive alias tried its candidates in the order it did
-- (Stage 5, ADR-039), such as "groq/x first: fastest of 2 eligible by
-- live p50 230 ms over 41 requests". NULL for pins and for aliases in
-- configured order, whose order the attempts already explain (ADR-036).
-- Identifiers and numbers only, never prompt or provider text.

ALTER TABLE request_records
    ADD COLUMN routing_reason text CHECK (char_length(routing_reason) BETWEEN 1 AND 256);
