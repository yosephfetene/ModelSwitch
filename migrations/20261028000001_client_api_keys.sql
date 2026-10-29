-- Client API keys (ADR-012). Only a SHA-256 hash of each key is stored;
-- the plaintext is shown once at creation and never persisted.
--
-- Status is derived, not stored: a key is revoked when `revoked_at` is
-- set, otherwise active. One source of truth means the two can never
-- disagree. Expiry (`expires_at`) arrives with its enforcement in
-- Stage 3.

CREATE TABLE client_api_keys (
    id          uuid        PRIMARY KEY DEFAULT uuidv7(),
    name        text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    -- First characters of the key (e.g. `ms_dev_7Hq2`), for display.
    key_prefix  text        NOT NULL,
    -- SHA-256 of the full key. Unique, and looked up on every request.
    key_hash    bytea       NOT NULL UNIQUE CHECK (octet_length(key_hash) = 32),
    created_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz
);
