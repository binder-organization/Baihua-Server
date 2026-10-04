CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_active_users_username_trigram
    ON users USING gin (username gin_trgm_ops)
    WHERE is_active = true;
