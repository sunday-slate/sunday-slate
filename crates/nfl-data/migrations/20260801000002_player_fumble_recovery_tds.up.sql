-- A player's offensive fumble-recovery TDs (recovering a fumble and scoring),
-- FanDuel's FU/TD (+6). Present in the nflverse player weekly feed; scored for
-- offensive skill players.
ALTER TABLE player_week_stats ADD COLUMN fumble_recovery_tds INTEGER NOT NULL DEFAULT 0;
