-- Accounts, per-device credentials and pairing codes (0.29.0). Expand only: four new tables
-- and one nullable column; nothing existing is renamed, removed or retyped, so the previous
-- binary keeps running against this schema until the new one is deployed.
--
-- Secrets are never stored. A credential keeps SHA-256(secret): the secret has 256 bits of
-- entropy, so a slow KDF would only make every request dearer. A pairing code keeps
-- code_key, the SHA-256 of the code under a fixed domain; the host needs that key to check
-- the client's MAC, and a code lives 24 hours and dies on first use.
--
-- created_at and expires_at have no DEFAULT on purpose: the caller passes the clock, so a
-- test can inject it and a hidden now() cannot decide who may pair.
-- Every statement is guarded (IF NOT EXISTS / ON CONFLICT) because sqlx may replay it.

-- One row for the whole host: the id behind the fingerprint every client pins. Random v4
-- (gen_random_uuid), never time-ordered, and minted here so it exists before anyone asks.
CREATE TABLE IF NOT EXISTS brain_host_identity (
    singleton  boolean     PRIMARY KEY DEFAULT true CHECK (singleton),
    host_id    uuid        NOT NULL DEFAULT gen_random_uuid(),
    created_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO brain_host_identity DEFAULT VALUES ON CONFLICT DO NOTHING;

CREATE TABLE IF NOT EXISTS brain_accounts (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text        NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9._-]{0,31}$'),
    created_at timestamptz NOT NULL,
    revoked_at timestamptz,
    CONSTRAINT brain_accounts_name_key UNIQUE (name)
);

-- The credential id travels in the bearer (mi1.<id>.<secret>), so it is random v4 and not
-- guessable by time. ON DELETE RESTRICT: an account with devices is revoked, never deleted.
CREATE TABLE IF NOT EXISTS brain_account_credentials (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id   uuid        NOT NULL REFERENCES brain_accounts(id) ON DELETE RESTRICT,
    device_label text        NOT NULL CHECK (device_label ~ '^[A-Za-z0-9][A-Za-z0-9._ -]{0,39}$'),
    secret_hash  bytea       NOT NULL CHECK (octet_length(secret_hash) = 32),
    created_at   timestamptz NOT NULL,
    last_seen_at timestamptz,
    revoked_at   timestamptz
);
CREATE INDEX IF NOT EXISTS idx_brain_account_credentials_account
    ON brain_account_credentials (account_id);

-- 24 hours is a CHECK and not only application code: no writer, however buggy, can mint a
-- code that lives longer. used_at and credential_id are set together or not at all.
CREATE TABLE IF NOT EXISTS brain_pairing_codes (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id    uuid        NOT NULL REFERENCES brain_accounts(id) ON DELETE CASCADE,
    code_key      bytea       NOT NULL CHECK (octet_length(code_key) = 32),
    device_label  text        CHECK (device_label ~ '^[A-Za-z0-9][A-Za-z0-9._ -]{0,39}$'),
    created_at    timestamptz NOT NULL,
    expires_at    timestamptz NOT NULL,
    used_at       timestamptz,
    credential_id uuid        REFERENCES brain_account_credentials(id) ON DELETE RESTRICT,
    CHECK (expires_at > created_at AND expires_at - created_at <= interval '24 hours'),
    CHECK ((used_at IS NULL) = (credential_id IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_brain_pairing_codes_account
    ON brain_pairing_codes (account_id);
CREATE INDEX IF NOT EXISTS idx_brain_pairing_codes_credential
    ON brain_pairing_codes (credential_id);

-- Which account made a failed call; null for the shared token.
ALTER TABLE brain_handler_failures ADD COLUMN IF NOT EXISTS account text;

-- Revoking is a column, not a row removal: deleting an account or a credential would delete
-- the record of who did what. Pairing codes stay deletable (revoke and purge remove them).
REVOKE DELETE ON brain_accounts, brain_account_credentials, brain_host_identity FROM cuba_app;

COMMENT ON TABLE brain_host_identity IS
    'The one identity of this host (its host_id is behind the fingerprint clients pin). Never a secret.';
COMMENT ON TABLE brain_accounts IS
    'Named accounts that authenticate and attribute; they do not partition the graph. A revoked name is never reused. Never a secret.';
COMMENT ON TABLE brain_account_credentials IS
    'One credential per paired device. Holds SHA-256(secret) only, never the secret or the bearer.';
COMMENT ON TABLE brain_pairing_codes IS
    'Single-use pairing codes, valid 24 hours. Holds code_key only, never the code itself.';
