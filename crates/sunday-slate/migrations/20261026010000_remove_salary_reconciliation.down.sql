DROP TRIGGER tg_nfl_players_updated_at;
DROP INDEX idx_nfl_players_headshot_media_id;

CREATE TABLE nfl_players_old (
    id               INTEGER PRIMARY KEY,
    gsis_player_id   TEXT    NOT NULL UNIQUE,
    fd_player_id     INTEGER UNIQUE,
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
    CAST(NULL AS INTEGER),
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

CREATE TABLE nfl_player_salaries (
    id             INTEGER PRIMARY KEY,
    gsis_game_id   TEXT    NOT NULL,
    gsis_player_id TEXT,
    team_abbr      TEXT    NOT NULL,
    dfs_position   TEXT    NOT NULL,
    salary         INTEGER NOT NULL,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at     TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    CHECK (dfs_position = 'DST' OR gsis_player_id IS NOT NULL)
);
CREATE UNIQUE INDEX ux_salary_player
    ON nfl_player_salaries(gsis_game_id, gsis_player_id) WHERE dfs_position <> 'DST';
CREATE UNIQUE INDEX ux_salary_dst
    ON nfl_player_salaries(gsis_game_id, team_abbr) WHERE dfs_position = 'DST';
CREATE TRIGGER tg_nfl_player_salaries_updated_at
    AFTER UPDATE ON nfl_player_salaries FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_player_salaries SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
