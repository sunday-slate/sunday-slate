CREATE TABLE media (
    id           INTEGER PRIMARY KEY,
    path         TEXT,
    content_type TEXT    NOT NULL,
    byte_size    INTEGER NOT NULL,
    width        INTEGER NOT NULL,
    height       INTEGER NOT NULL,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at   TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (path)
);

CREATE TRIGGER tg_media_updated_at
    AFTER UPDATE ON media
    FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE media SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
