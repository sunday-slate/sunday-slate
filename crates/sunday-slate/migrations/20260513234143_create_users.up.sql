CREATE TABLE users (
   id              INTEGER PRIMARY KEY,
   email           TEXT    NOT NULL UNIQUE COLLATE NOCASE,
   display_name    TEXT    NOT NULL,
   created_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
   updated_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec'))
);

CREATE TRIGGER tg_users_updated_at
    AFTER UPDATE ON users
    FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE users SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;
