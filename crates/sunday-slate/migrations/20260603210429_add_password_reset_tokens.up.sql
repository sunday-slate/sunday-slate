CREATE TABLE password_reset_tokens (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    token_hash  TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL,
    used_at     TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at  TEXT    NOT NULL DEFAULT (datetime('now', 'subsec'))
);

CREATE INDEX idx_prt_expires_at ON password_reset_tokens(expires_at);

CREATE TRIGGER tg_password_reset_tokens_updated_at
    AFTER UPDATE ON password_reset_tokens
    FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE password_reset_tokens SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
