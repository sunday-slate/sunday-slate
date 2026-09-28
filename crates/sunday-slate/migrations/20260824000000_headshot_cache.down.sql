DROP TABLE nfl_teams;
DROP TRIGGER tg_nfl_players_updated_at;

DROP INDEX idx_nfl_players_headshot_media_id;
ALTER TABLE nfl_players DROP COLUMN headshot_media_id;
ALTER TABLE nfl_players RENAME TO nfl_player;

CREATE TRIGGER tg_nfl_player_updated_at
    AFTER UPDATE ON nfl_player FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE nfl_player SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
