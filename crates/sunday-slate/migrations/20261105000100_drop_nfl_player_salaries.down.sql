CREATE TABLE nfl_player_salaries (
    id             INTEGER PRIMARY KEY,
    game_id        TEXT    NOT NULL,
    gsis_player_id TEXT,
    team_abbr      TEXT    NOT NULL,
    dfs_position   TEXT    NOT NULL,
    salary         INTEGER NOT NULL,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at     TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    CHECK (dfs_position = 'DST' OR gsis_player_id IS NOT NULL)
);
CREATE UNIQUE INDEX ux_salary_player
    ON nfl_player_salaries(game_id, gsis_player_id) WHERE dfs_position <> 'DST';
CREATE UNIQUE INDEX ux_salary_dst
    ON nfl_player_salaries(game_id, team_abbr) WHERE dfs_position = 'DST';
CREATE TRIGGER tg_nfl_player_salaries_updated_at
    AFTER UPDATE ON nfl_player_salaries FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_player_salaries SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
