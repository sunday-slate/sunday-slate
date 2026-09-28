DROP TRIGGER tg_nfl_player_updated_at;

ALTER TABLE nfl_player RENAME TO nfl_players;

ALTER TABLE nfl_players
    ADD COLUMN headshot_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL;

-- ON DELETE SET NULL scans the referencing column on every media delete.
CREATE INDEX idx_nfl_players_headshot_media_id ON nfl_players(headshot_media_id);

CREATE TRIGGER tg_nfl_players_updated_at
    AFTER UPDATE ON nfl_players FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_players SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

CREATE TABLE nfl_teams (
    abbr          TEXT PRIMARY KEY,
    logo_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL
);

CREATE INDEX idx_nfl_teams_logo_media_id ON nfl_teams(logo_media_id);

INSERT INTO nfl_teams (abbr) VALUES
    ('ARI'), ('ATL'), ('BAL'), ('BUF'), ('CAR'), ('CHI'), ('CIN'), ('CLE'),
    ('DAL'), ('DEN'), ('DET'), ('GB'),  ('HOU'), ('IND'), ('JAX'), ('KC'),
    ('LA'),  ('LAC'), ('LV'),  ('MIA'), ('MIN'), ('NE'),  ('NO'),  ('NYG'),
    ('NYJ'), ('PHI'), ('PIT'), ('SEA'), ('SF'),  ('TB'),  ('TEN'), ('WAS');
