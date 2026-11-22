ALTER TABLE games ADD COLUMN kickoff_override BOOLEAN NOT NULL DEFAULT FALSE;

-- Seed the corrections even when this migration runs before the first sync.
INSERT INTO games (game_id, season, week, season_type, kickoff, home_team, away_team, kickoff_override)
VALUES
    ('2026_11_JAX_NYG', 2026, 11, '0', '2026-11-22T21:25:00Z', 'NYG', 'JAX', TRUE),
    ('2026_11_PIT_PHI', 2026, 11, '0', '2026-11-22T18:00:00Z', 'PHI', 'PIT', TRUE)
ON CONFLICT (game_id) DO UPDATE SET
    kickoff = excluded.kickoff,
    kickoff_override = excluded.kickoff_override;
