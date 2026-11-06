CREATE TABLE nfl_player_headshots (
    nfl_player_id     TEXT    NOT NULL PRIMARY KEY,
    headshot_media_id INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE
);

INSERT INTO nfl_player_headshots (nfl_player_id, headshot_media_id)
SELECT gsis_player_id, headshot_media_id
FROM nfl_players
WHERE headshot_media_id IS NOT NULL;

DROP TABLE nfl_players;
