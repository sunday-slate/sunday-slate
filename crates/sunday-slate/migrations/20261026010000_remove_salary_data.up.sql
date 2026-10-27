DROP TABLE IF EXISTS nfl_player_salaries;

CREATE TABLE nfl_players_new (
    id               INTEGER PRIMARY KEY,
    gsis_player_id   TEXT    NOT NULL UNIQUE,
    created_at       TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at       TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    headshot_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL
);

INSERT INTO nfl_players_new (
    id,
    gsis_player_id,
    created_at,
    updated_at,
    headshot_media_id
)
SELECT
    id,
    gsis_player_id,
    created_at,
    updated_at,
    headshot_media_id
FROM nfl_players;

DROP TABLE nfl_players;
ALTER TABLE nfl_players_new RENAME TO nfl_players;

CREATE INDEX idx_nfl_players_headshot_media_id ON nfl_players(headshot_media_id);

CREATE TRIGGER tg_nfl_players_updated_at
    AFTER UPDATE ON nfl_players FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_players SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
