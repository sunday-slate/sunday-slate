-- team_week_stats becomes fully pbp-derived: FanDuel D/ST line items plus the
-- FanDuel points-allowed (the opponent's offensive points), all computed at
-- ingest from play-by-play. See
-- 2026-07-31-pbp-derived-dst-stats-design
DROP TABLE IF EXISTS team_week_stats;
CREATE TABLE team_week_stats (
    id                  INTEGER PRIMARY KEY,
    season              INTEGER NOT NULL,
    week                INTEGER NOT NULL,
    season_type         TEXT    NOT NULL,
    team                TEXT    NOT NULL,
    opponent            TEXT    NOT NULL,
    gsis_game_id        TEXT    NOT NULL,
    sacks               INTEGER NOT NULL,
    interceptions       INTEGER NOT NULL,
    fumble_recoveries   INTEGER NOT NULL,
    safeties            INTEGER NOT NULL,
    touchdowns          INTEGER NOT NULL,  -- non-offensive TDs (+6 each)
    blocked_kicks       INTEGER NOT NULL,
    conversion_returns  INTEGER NOT NULL,  -- defensive XP/2pt returns (+2 each)
    points_allowed      INTEGER NOT NULL,  -- opponent's offensive points (FanDuel PA)
    UNIQUE (season, week, season_type, team)
);
CREATE INDEX idx_team_week_stats_season_week ON team_week_stats (season, week);
