DROP TRIGGER tg_player_links_updated_at;

CREATE TABLE player_links_old (
    fd_player_id   INTEGER PRIMARY KEY,
    gsis_player_id TEXT    NOT NULL UNIQUE,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now','subsec')),
    updated_at     TEXT    NOT NULL DEFAULT (datetime('now','subsec'))
);

INSERT INTO player_links_old (fd_player_id, gsis_player_id, created_at, updated_at)
SELECT fd_player_id, gsis_player_id, created_at, updated_at FROM player_links
ORDER BY fd_player_id;

DROP TABLE player_links;
ALTER TABLE player_links_old RENAME TO player_links;

CREATE TRIGGER tg_player_links_updated_at
    AFTER UPDATE ON player_links FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE player_links SET updated_at = datetime('now','subsec') WHERE fd_player_id = NEW.fd_player_id;
END;
