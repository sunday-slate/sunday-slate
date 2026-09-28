CREATE TABLE fantasy_teams (
    id              INTEGER PRIMARY KEY,
    league_id       INTEGER NOT NULL REFERENCES leagues(id),
    user_id         INTEGER NOT NULL REFERENCES users(id),
    name            TEXT    NOT NULL,
    owner_name      TEXT    NOT NULL,
    logo            TEXT,
    is_commissioner INTEGER NOT NULL DEFAULT FALSE,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (league_id, user_id)
);

CREATE TRIGGER tg_fantasy_teams_updated_at
    AFTER UPDATE ON fantasy_teams
    FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE fantasy_teams SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
