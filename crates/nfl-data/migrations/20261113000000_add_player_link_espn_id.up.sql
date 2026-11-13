ALTER TABLE player_links ADD COLUMN espn_player_id TEXT;

CREATE UNIQUE INDEX idx_player_links_espn_player_id
    ON player_links(espn_player_id)
    WHERE espn_player_id IS NOT NULL;
