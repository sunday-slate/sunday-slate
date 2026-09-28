-- A team's own offensive scoring, so the read query can derive each team's
-- FanDuel points-allowed as the OPPONENT's offensive points (6 per rush/rec TD,
-- 2 per rush/rec 2pt, plus PATs and FGs). This replaces the old
-- scoreboard-minus-known-non-offensive derivation, which silently kept
-- non-offensive scores nflverse's def_* columns don't carry (fumble-return TDs,
-- special-teams safeties). See 2026-07-31-nflverse-vs-fanduel-stat-reconciliation.
ALTER TABLE team_week_stats ADD COLUMN rushing_tds   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE team_week_stats ADD COLUMN receiving_tds INTEGER NOT NULL DEFAULT 0;
ALTER TABLE team_week_stats ADD COLUMN rushing_2pt   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE team_week_stats ADD COLUMN receiving_2pt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE team_week_stats ADD COLUMN pat_made      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE team_week_stats ADD COLUMN fg_made       INTEGER NOT NULL DEFAULT 0;
