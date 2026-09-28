-- Restore the immediately-prior schema: the original team_week_stats plus the
-- six offensive columns added by 20260731000001_add_team_offensive_scoring, so a
-- single revert lands exactly one migration back.
DROP TABLE IF EXISTS team_week_stats;
CREATE TABLE team_week_stats (
    id                  INTEGER PRIMARY KEY,
    season              INTEGER NOT NULL,
    week                INTEGER NOT NULL,
    season_type         TEXT    NOT NULL,
    team                TEXT    NOT NULL,
    opponent            TEXT    NOT NULL,
    gsis_game_id        TEXT    NOT NULL,
    sacks               REAL    NOT NULL,
    interceptions       INTEGER NOT NULL,
    fumbles_recovered   INTEGER NOT NULL,
    defensive_tds       INTEGER NOT NULL,
    fumble_recovery_tds INTEGER NOT NULL,
    special_teams_tds   INTEGER NOT NULL,
    safeties            INTEGER NOT NULL,
    rushing_tds         INTEGER NOT NULL DEFAULT 0,
    receiving_tds       INTEGER NOT NULL DEFAULT 0,
    rushing_2pt         INTEGER NOT NULL DEFAULT 0,
    receiving_2pt       INTEGER NOT NULL DEFAULT 0,
    pat_made            INTEGER NOT NULL DEFAULT 0,
    fg_made             INTEGER NOT NULL DEFAULT 0,
    UNIQUE (season, week, season_type, team)
);
CREATE INDEX idx_team_week_stats_season_week ON team_week_stats (season, week);
