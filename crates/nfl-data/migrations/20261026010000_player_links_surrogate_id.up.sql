-- Replace the FanDuel-id primary key with a surrogate auto-increment id so
-- other identity providers can attach rows in the future. fd_player_id keeps
-- its unique constraint; existing links and timestamps are preserved.
CREATE TABLE player_links_new (
    id             INTEGER PRIMARY KEY,
    fd_player_id   INTEGER NOT NULL UNIQUE,
    gsis_player_id TEXT    NOT NULL UNIQUE,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now','subsec')),
    updated_at     TEXT    NOT NULL DEFAULT (datetime('now','subsec'))
);

INSERT INTO player_links_new (fd_player_id, gsis_player_id, created_at, updated_at)
SELECT fd_player_id, gsis_player_id, created_at, updated_at FROM player_links
ORDER BY fd_player_id;

DROP TABLE player_links;
ALTER TABLE player_links_new RENAME TO player_links;

CREATE TRIGGER tg_player_links_updated_at
    AFTER UPDATE ON player_links FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE player_links SET updated_at = datetime('now','subsec') WHERE id = NEW.id;
END;
