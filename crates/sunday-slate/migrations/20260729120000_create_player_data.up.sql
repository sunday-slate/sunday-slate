CREATE TABLE contests (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at TEXT    NOT NULL DEFAULT (datetime('now', 'subsec'))
);
CREATE TRIGGER tg_contests_updated_at
    AFTER UPDATE ON contests FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE contests SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

CREATE TABLE contest_games (
    id           INTEGER PRIMARY KEY,
    contest_id   INTEGER NOT NULL REFERENCES contests(id),
    gsis_game_id TEXT    NOT NULL,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at   TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (contest_id, gsis_game_id)
);
CREATE TRIGGER tg_contest_games_updated_at
    AFTER UPDATE ON contest_games FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE contest_games SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

CREATE TABLE nfl_player (
    id             INTEGER PRIMARY KEY,
    gsis_player_id TEXT NOT NULL UNIQUE,
    fd_player_id   TEXT          UNIQUE,
    created_at     TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at     TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
);
CREATE TRIGGER tg_nfl_player_updated_at
    AFTER UPDATE ON nfl_player FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_player SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
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
