CREATE TABLE invites (
    id          INTEGER PRIMARY KEY,
    league_id   INTEGER NOT NULL REFERENCES leagues(id) ON DELETE CASCADE,
    email       TEXT    NOT NULL,
    token_hash  TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL,
    accepted_at TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at  TEXT    NOT NULL DEFAULT (datetime('now', 'subsec'))
);

-- At most one outstanding (unaccepted) invite per (league, email).
CREATE UNIQUE INDEX idx_invites_active
    ON invites (league_id, email) WHERE accepted_at IS NULL;

CREATE INDEX idx_invites_expires_at ON invites (expires_at);

CREATE TRIGGER tg_invites_updated_at
    AFTER UPDATE ON invites
    FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE invites SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
