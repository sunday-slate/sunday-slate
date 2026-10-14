DROP TRIGGER tg_nfl_players_updated_at;
DROP INDEX idx_nfl_players_headshot_media_id;

CREATE TABLE nfl_players_old (
    id               INTEGER PRIMARY KEY,
    gsis_player_id   TEXT    NOT NULL UNIQUE,
    fd_player_id     TEXT    UNIQUE,
    created_at       TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at       TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    headshot_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL
);

INSERT INTO nfl_players_old (
    id,
    gsis_player_id,
    fd_player_id,
    created_at,
    updated_at,
    headshot_media_id
)
SELECT
    id,
    gsis_player_id,
    CAST(fd_player_id AS TEXT),
    created_at,
    updated_at,
    headshot_media_id
FROM nfl_players;

DROP TABLE nfl_players;
ALTER TABLE nfl_players_old RENAME TO nfl_players;

CREATE INDEX idx_nfl_players_headshot_media_id ON nfl_players(headshot_media_id);

CREATE TRIGGER tg_nfl_players_updated_at
    AFTER UPDATE ON nfl_players FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_players SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
