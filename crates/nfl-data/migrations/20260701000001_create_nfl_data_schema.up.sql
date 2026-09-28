-- Cache tables deliberately skip the created_at/updated_at trigger convention:
-- rows are wholesale-replaced snapshots of upstream release assets, and their
-- provenance timestamp lives in sync_state.synced_at.

CREATE TABLE sync_state (
    id                  INTEGER PRIMARY KEY,
    release_tag         TEXT    NOT NULL,
    asset_name          TEXT    NOT NULL,
    upstream_updated_at TEXT    NOT NULL,
    synced_at           TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (release_tag, asset_name)
);

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
CREATE INDEX idx_games_season_week ON games (season, week);

CREATE TABLE players (
    id           INTEGER PRIMARY KEY,
    gsis_id      TEXT NOT NULL UNIQUE,
    full_name    TEXT NOT NULL,
    first_name   TEXT,
    last_name    TEXT,
    position     TEXT,
    latest_team  TEXT,
    status       TEXT,
    birth_date   TEXT,
    headshot_url TEXT
);

CREATE TABLE roster_entries (
    id            INTEGER PRIMARY KEY,
    season        INTEGER NOT NULL,
    team          TEXT    NOT NULL,
    gsis_id       TEXT,
    full_name     TEXT    NOT NULL,
    position      TEXT,
    jersey_number INTEGER,
    status        TEXT    NOT NULL,
    UNIQUE (season, team, gsis_id)
);
CREATE INDEX idx_roster_entries_season_team ON roster_entries (season, team);

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
    fg_made_0_19          INTEGER NOT NULL,
    fg_made_20_29         INTEGER NOT NULL,
    fg_made_30_39         INTEGER NOT NULL,
    fg_made_40_49         INTEGER NOT NULL,
    fg_made_50_59         INTEGER NOT NULL,
    fg_made_60_plus       INTEGER NOT NULL,
    fg_missed             INTEGER NOT NULL,
    pat_made              INTEGER NOT NULL,
    pat_missed            INTEGER NOT NULL,
    fantasy_points        REAL    NOT NULL,
    fantasy_points_ppr    REAL    NOT NULL,
    UNIQUE (season, week, season_type, gsis_id)
);
CREATE INDEX idx_player_week_stats_season_week ON player_week_stats (season, week);

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
    UNIQUE (season, week, season_type, team)
);
CREATE INDEX idx_team_week_stats_season_week ON team_week_stats (season, week);
