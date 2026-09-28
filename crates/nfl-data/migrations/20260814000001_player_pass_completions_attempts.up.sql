-- A passer's completions and attempts, for the QB box-score line ("26/38").
-- Present in the nflverse player weekly feed; display-only, not scored.
ALTER TABLE player_week_stats ADD COLUMN completions INTEGER NOT NULL DEFAULT 0;
ALTER TABLE player_week_stats ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
