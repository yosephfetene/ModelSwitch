-- Optional key expiry (ADR-033). NULL means the key never expires, so
-- existing keys keep working. Status stays derived: a key is revoked when
-- `revoked_at` is set, expired once `expires_at` has passed, otherwise
-- active.

ALTER TABLE client_api_keys
    ADD COLUMN expires_at timestamptz,
    ADD CONSTRAINT client_api_keys_expires_after_created
        CHECK (expires_at > created_at);
