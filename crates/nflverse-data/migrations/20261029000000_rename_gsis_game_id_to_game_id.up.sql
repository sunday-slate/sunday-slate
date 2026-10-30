-- nflverse's own `gsis` column is a different (numeric) id. What we store is
-- nflverse's `game_id` (YYYY_WW_AWAY_HOME), so name it that.
ALTER TABLE games RENAME COLUMN gsis_game_id TO game_id;
ALTER TABLE team_week_stats RENAME COLUMN gsis_game_id TO game_id;
