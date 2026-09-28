-- Per-week team rosters (nflverse `weekly_rosters` release). The season-level
-- `roster_entries` table carries only an end-of-season snapshot, so a player
-- traded mid-season looks like he was always on his final team. Salary imports
-- match a player against the team he actually played for in that week, which
-- needs this granularity.
--
-- `last_name` is stored rather than derived: upstream splits names properly
-- ("Dante Fowler Jr." has last_name "Fowler", "Marquez Valdes-Scantling" keeps
-- its hyphen), and deriving a surname from full_name gets both wrong. Candidate
-- lists sort on it.
CREATE TABLE weekly_roster_entries (
    id        INTEGER PRIMARY KEY,
    season    INTEGER NOT NULL,
    week      INTEGER NOT NULL,
    team      TEXT    NOT NULL,
    gsis_id   TEXT,
    full_name TEXT    NOT NULL,
    last_name TEXT,
    position  TEXT,
    status    TEXT    NOT NULL,
    UNIQUE (season, week, team, gsis_id)
);

CREATE INDEX idx_weekly_roster_entries_season_week_team
    ON weekly_roster_entries (season, week, team);
