-- Fresh nflverse cache baseline. No legacy-cache conversion is supported.
-- Snapshots are replaced wholesale; provenance lives in sync_state.synced_at.

CREATE TABLE games (
    id           INTEGER PRIMARY KEY,
    gsis_game_id TEXT    NOT NULL UNIQUE,
    season       INTEGER NOT NULL,
    week         INTEGER NOT NULL,
    season_type  TEXT    NOT NULL,
    kickoff      TEXT,
    home_team    TEXT    NOT NULL,
    away_team    TEXT    NOT NULL,
    home_score   INTEGER,
    away_score   INTEGER
);

CREATE TABLE player_week_stats (
    id                    INTEGER PRIMARY KEY,
    season                INTEGER NOT NULL,
    week                  INTEGER NOT NULL,
    season_type           TEXT    NOT NULL,
    gsis_id               TEXT    NOT NULL,
    team                  TEXT    NOT NULL,
    opponent              TEXT,
    passing_yards         INTEGER NOT NULL,
    passing_tds           INTEGER NOT NULL,
    passing_interceptions INTEGER NOT NULL,
    rushing_attempts      INTEGER NOT NULL,
    rushing_yards         INTEGER NOT NULL,
    rushing_tds           INTEGER NOT NULL,
    targets               INTEGER NOT NULL,
    receptions            INTEGER NOT NULL,
    receiving_yards       INTEGER NOT NULL,
    receiving_tds         INTEGER NOT NULL,
    fumbles_lost          INTEGER NOT NULL,
    two_point_conversions INTEGER NOT NULL,
    special_teams_tds     INTEGER NOT NULL,
    fumble_recovery_tds   INTEGER NOT NULL DEFAULT 0,
    completions           INTEGER NOT NULL DEFAULT 0,
    attempts              INTEGER NOT NULL DEFAULT 0,
    UNIQUE (season, week, season_type, gsis_id)
);

CREATE TABLE players (
    id           INTEGER PRIMARY KEY,
    gsis_id      TEXT NOT NULL UNIQUE,
    full_name    TEXT NOT NULL,
    first_name   TEXT,
    last_name    TEXT,
    position     TEXT,
    latest_team  TEXT,
    headshot_url TEXT,
    espn_id      TEXT
);

CREATE TABLE sync_state (
    id                  INTEGER PRIMARY KEY,
    release_tag         TEXT    NOT NULL,
    asset_name          TEXT    NOT NULL,
    upstream_updated_at TEXT    NOT NULL,
    synced_at           TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (release_tag, asset_name)
);

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

CREATE TABLE weekly_roster_entries (
    id        INTEGER PRIMARY KEY,
    season    INTEGER NOT NULL,
    week      INTEGER NOT NULL,
    team      TEXT    NOT NULL,
    gsis_id   TEXT,
    full_name TEXT    NOT NULL,
    last_name TEXT,
    position  TEXT,
    espn_id   TEXT,
    UNIQUE (season, week, team, gsis_id)
);

CREATE INDEX idx_games_season_week ON games (season, week);

CREATE INDEX idx_player_week_stats_season_week ON player_week_stats (season, week);

CREATE INDEX idx_team_week_stats_season_week ON team_week_stats (season, week);

CREATE INDEX idx_weekly_roster_entries_gsis
    ON weekly_roster_entries (gsis_id, season DESC, week DESC);

CREATE INDEX idx_weekly_roster_entries_season_week_team
    ON weekly_roster_entries (season, week, team);
