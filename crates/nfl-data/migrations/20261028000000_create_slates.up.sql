CREATE TABLE slates (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    season_week INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    kickoff     TEXT    NULL,
    UNIQUE (season_week, name)
);

CREATE TABLE slate_games (
    slate_id     INTEGER NOT NULL REFERENCES slates (id) ON DELETE CASCADE,
    gsis_game_id TEXT    NOT NULL,
    PRIMARY KEY (slate_id, gsis_game_id)
);

CREATE TABLE slate_week_snapshots (
    season_week INTEGER PRIMARY KEY
);
