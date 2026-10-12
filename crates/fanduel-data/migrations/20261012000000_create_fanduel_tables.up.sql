CREATE TABLE players (
    id INTEGER PRIMARY KEY,
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULL
);

CREATE TABLE games (
    id INTEGER PRIMARY KEY,
    season_week INTEGER NOT NULL,
    away_team TEXT NOT NULL,
    home_team TEXT NOT NULL,
    UNIQUE (season_week, away_team, home_team)
);

CREATE INDEX idx_games_season_week ON games (season_week);

CREATE TABLE salaries (
    id INTEGER PRIMARY KEY,
    player_id INTEGER REFERENCES players(id),
    game_id INTEGER NOT NULL REFERENCES games(id),
    salary INTEGER NOT NULL CHECK (salary > 0),
    position TEXT NOT NULL CHECK (position IN ('QB', 'RB', 'WR', 'TE', 'DST')),
    team TEXT NOT NULL,
    CHECK (player_id IS NOT NULL OR position = 'DST')
);

CREATE INDEX idx_salaries_player_id ON salaries (player_id);
CREATE INDEX idx_salaries_game_id ON salaries (game_id);

CREATE UNIQUE INDEX idx_salaries_game_player
    ON salaries (game_id, player_id)
    WHERE position <> 'DST';

CREATE UNIQUE INDEX idx_salaries_game_dst_team
    ON salaries (game_id, team)
    WHERE position = 'DST';
