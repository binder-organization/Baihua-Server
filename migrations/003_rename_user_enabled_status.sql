ALTER TABLE users RENAME COLUMN is_active TO is_enabled;

ALTER INDEX idx_users_created_at RENAME TO idx_enabled_users_created_at;
ALTER INDEX idx_active_users_username_trigram RENAME TO idx_enabled_users_username_trigram;
