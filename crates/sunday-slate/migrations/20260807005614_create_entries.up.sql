CREATE TABLE entries (
    id              INTEGER PRIMARY KEY,
    contest_id      INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
    fantasy_team_id INTEGER NOT NULL REFERENCES fantasy_teams(id),
    created_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (contest_id, fantasy_team_id)
);
CREATE TRIGGER tg_entries_updated_at
    AFTER UPDATE ON entries FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE entries SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

CREATE TABLE entry_slots (
    id             INTEGER PRIMARY KEY,
    entry_id       INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    roster_slot    TEXT    NOT NULL,
    gsis_player_id TEXT,
    team_abbr      TEXT,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at     TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (entry_id, roster_slot),
    -- Exactly nine slots make an entry, so only these nine names are valid.
    -- DEF is a team (mirrors nfl_player_salaries' DST shape); every other slot is a player.
    CHECK (roster_slot IN ('QB', 'RB1', 'RB2', 'WR1', 'WR2', 'WR3', 'TE', 'FLEX', 'DEF')
       AND (roster_slot = 'DEF') = (team_abbr IS NOT NULL)
       AND (roster_slot = 'DEF') = (gsis_player_id IS NULL))
);
CREATE TRIGGER tg_entry_slots_updated_at
    AFTER UPDATE ON entry_slots FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE entry_slots SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

CREATE UNIQUE INDEX ux_entry_slots_player
    ON entry_slots(entry_id, gsis_player_id) WHERE gsis_player_id IS NOT NULL;
