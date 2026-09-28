ALTER TABLE fantasy_teams DROP COLUMN logo;
ALTER TABLE fantasy_teams ADD COLUMN logo_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL;
