CREATE TABLE contest_games (
    id         INTEGER PRIMARY KEY,
    contest_id INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
    game_id    TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (contest_id, game_id)
);
CREATE TRIGGER tg_contest_games_updated_at
    AFTER UPDATE ON contest_games FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE contest_games SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
