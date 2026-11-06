CREATE TABLE nfl_players (
    id               INTEGER PRIMARY KEY,
    gsis_player_id   TEXT    NOT NULL UNIQUE,
    fd_player_id     INTEGER UNIQUE,
    created_at       TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at       TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    headshot_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL
);

INSERT INTO nfl_players (gsis_player_id, headshot_media_id)
SELECT nfl_player_id, headshot_media_id
FROM nfl_player_headshots;

CREATE INDEX idx_nfl_players_headshot_media_id ON nfl_players(headshot_media_id);

CREATE TRIGGER tg_nfl_players_updated_at
    AFTER UPDATE ON nfl_players FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_players SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

DROP TABLE nfl_player_headshots;
