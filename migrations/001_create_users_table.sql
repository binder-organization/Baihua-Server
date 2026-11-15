CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL,
    password TEXT NOT NULL,
    nickname TEXT,
    phone_number TEXT,
    bio TEXT,
    avatar TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    is_active BOOLEAN NOT NULL DEFAULT true,
    email_verified_at TIMESTAMPTZ,
    token_version BIGINT NOT NULL DEFAULT 0
);

-- User list queries filter on is_active = true and sort by created_at DESC
-- (src/user/list.rs). A partial index matches that exact access pattern.
CREATE INDEX IF NOT EXISTS idx_users_created_at
    ON users (created_at DESC) WHERE is_active = true;

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_active_users_username_trigram
    ON users USING gin (username gin_trgm_ops)
    WHERE is_active = true;

CREATE INDEX users_normalized_email_index ON users (lower(email));

CREATE TABLE email_verification_tokens (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE email_verification_sends (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    sent_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX email_verification_sends_recent
    ON email_verification_sends (user_id, sent_at DESC);
