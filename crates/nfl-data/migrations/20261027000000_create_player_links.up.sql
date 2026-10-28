CREATE TABLE player_links (
    id             INTEGER PRIMARY KEY,
    fd_player_id   INTEGER NOT NULL UNIQUE,
    gsis_player_id TEXT    NOT NULL UNIQUE,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now','subsec')),
    updated_at     TEXT    NOT NULL DEFAULT (datetime('now','subsec'))
);

CREATE TRIGGER tg_player_links_updated_at
    AFTER UPDATE ON player_links FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE player_links SET updated_at = datetime('now','subsec') WHERE id = NEW.id;
END;
