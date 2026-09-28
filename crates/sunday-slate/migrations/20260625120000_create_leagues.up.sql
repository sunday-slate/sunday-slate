CREATE TABLE leagues (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at TEXT    NOT NULL DEFAULT (datetime('now', 'subsec'))
);

CREATE TRIGGER tg_leagues_updated_at
    AFTER UPDATE ON leagues
    FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE leagues SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
