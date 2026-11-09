-- Entries and saved drafts identify their slate directly by nfl-data SlateId.
--
-- Only contests referenced by an entry or draft need a target slate. Every
-- referenced contest must resolve to one of the 2026 regular-season main
-- slate names (`Week 1`..`Week 18`); the mapping table is NOT NULL so an
-- unknown name or dangling reference aborts this migration instead of
-- discarding user data. Rollback is restoration of the predeployment
-- database backup; a down migration cannot recover discarded contest
-- identities or invent nonlegacy slates.

CREATE TABLE _contest_slate_map (
    contest_id INTEGER PRIMARY KEY,
    slate_id   INTEGER NOT NULL
);

INSERT INTO _contest_slate_map (contest_id, slate_id)
WITH RECURSIVE weeks(week) AS (
    SELECT 1
    UNION ALL
    SELECT week + 1 FROM weeks WHERE week < 18
)
SELECT referenced.contest_id,
       (SELECT (2026 * 1000 + weeks.week) * 10 + 1
          FROM weeks
         WHERE contest.name = 'Week ' || weeks.week)
FROM (
    SELECT contest_id FROM entries
    UNION
    SELECT contest_id FROM draft_entries
) AS referenced
LEFT JOIN contests AS contest ON contest.id = referenced.contest_id;

CREATE TABLE _entries_backup (
    id              INTEGER NOT NULL PRIMARY KEY,
    slate_id        INTEGER NOT NULL,
    fantasy_team_id INTEGER NOT NULL,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL,
    UNIQUE (slate_id, fantasy_team_id)
);

INSERT INTO _entries_backup (id, slate_id, fantasy_team_id, created_at, updated_at)
SELECT e.id, m.slate_id, e.fantasy_team_id, e.created_at, e.updated_at
FROM entries AS e
JOIN _contest_slate_map AS m ON m.contest_id = e.contest_id;

CREATE TABLE _draft_entries_backup (
    id              INTEGER NOT NULL PRIMARY KEY,
    slate_id        INTEGER NOT NULL,
    fantasy_team_id INTEGER NOT NULL,
    qb_gsis_id      TEXT,
    rb1_gsis_id     TEXT,
    rb2_gsis_id     TEXT,
    wr1_gsis_id     TEXT,
    wr2_gsis_id     TEXT,
    wr3_gsis_id     TEXT,
    te_gsis_id      TEXT,
    flex_gsis_id    TEXT,
    def_team_abbr   TEXT,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL,
    UNIQUE (slate_id, fantasy_team_id)
);

INSERT INTO _draft_entries_backup
    (id, slate_id, fantasy_team_id, qb_gsis_id, rb1_gsis_id, rb2_gsis_id,
     wr1_gsis_id, wr2_gsis_id, wr3_gsis_id, te_gsis_id, flex_gsis_id,
     def_team_abbr, created_at, updated_at)
SELECT d.id, m.slate_id, d.fantasy_team_id, d.qb_gsis_id, d.rb1_gsis_id,
       d.rb2_gsis_id, d.wr1_gsis_id, d.wr2_gsis_id, d.wr3_gsis_id,
       d.te_gsis_id, d.flex_gsis_id, d.def_team_abbr, d.created_at, d.updated_at
FROM draft_entries AS d
JOIN _contest_slate_map AS m ON m.contest_id = d.contest_id;

CREATE TABLE _entry_slots_backup (
    id             INTEGER NOT NULL PRIMARY KEY,
    entry_id       INTEGER NOT NULL,
    roster_slot    TEXT    NOT NULL,
    gsis_player_id TEXT,
    team_abbr      TEXT,
    created_at     TEXT    NOT NULL,
    updated_at     TEXT    NOT NULL
);

INSERT INTO _entry_slots_backup
    (id, entry_id, roster_slot, gsis_player_id, team_abbr, created_at, updated_at)
SELECT id, entry_id, roster_slot, gsis_player_id, team_abbr, created_at, updated_at
FROM entry_slots;

-- Child-first: entry_slots references entries; entries and drafts reference
-- contests. Dropping each table removes its triggers and indexes.
DROP TABLE entry_slots;
DROP TABLE draft_entries;
DROP TABLE entries;
DROP TABLE contests;

CREATE TABLE entries (
    id              INTEGER PRIMARY KEY,
    slate_id        INTEGER NOT NULL,
    fantasy_team_id INTEGER NOT NULL REFERENCES fantasy_teams(id),
    created_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (slate_id, fantasy_team_id)
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

CREATE TABLE draft_entries (
    id              INTEGER PRIMARY KEY,
    slate_id        INTEGER NOT NULL,
    fantasy_team_id INTEGER NOT NULL REFERENCES fantasy_teams(id) ON DELETE CASCADE,
    qb_gsis_id      TEXT,
    rb1_gsis_id     TEXT,
    rb2_gsis_id     TEXT,
    wr1_gsis_id     TEXT,
    wr2_gsis_id     TEXT,
    wr3_gsis_id     TEXT,
    te_gsis_id      TEXT,
    flex_gsis_id    TEXT,
    def_team_abbr   TEXT,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now', 'subsec')),
    UNIQUE (slate_id, fantasy_team_id),
    CHECK (
        (qb_gsis_id IS NULL OR qb_gsis_id NOT IN
            (COALESCE(rb1_gsis_id,''), COALESCE(rb2_gsis_id,''),
             COALESCE(wr1_gsis_id,''), COALESCE(wr2_gsis_id,''),
             COALESCE(wr3_gsis_id,''), COALESCE(te_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (rb1_gsis_id IS NULL OR rb1_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb2_gsis_id,''),
             COALESCE(wr1_gsis_id,''), COALESCE(wr2_gsis_id,''),
             COALESCE(wr3_gsis_id,''), COALESCE(te_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (rb2_gsis_id IS NULL OR rb2_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb1_gsis_id,''),
             COALESCE(wr1_gsis_id,''), COALESCE(wr2_gsis_id,''),
             COALESCE(wr3_gsis_id,''), COALESCE(te_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (wr1_gsis_id IS NULL OR wr1_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb1_gsis_id,''),
             COALESCE(rb2_gsis_id,''), COALESCE(wr2_gsis_id,''),
             COALESCE(wr3_gsis_id,''), COALESCE(te_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (wr2_gsis_id IS NULL OR wr2_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb1_gsis_id,''),
             COALESCE(rb2_gsis_id,''), COALESCE(wr1_gsis_id,''),
             COALESCE(wr3_gsis_id,''), COALESCE(te_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (wr3_gsis_id IS NULL OR wr3_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb1_gsis_id,''),
             COALESCE(rb2_gsis_id,''), COALESCE(wr1_gsis_id,''),
             COALESCE(wr2_gsis_id,''), COALESCE(te_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (te_gsis_id IS NULL OR te_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb1_gsis_id,''),
             COALESCE(rb2_gsis_id,''), COALESCE(wr1_gsis_id,''),
             COALESCE(wr2_gsis_id,''), COALESCE(wr3_gsis_id,''),
             COALESCE(flex_gsis_id,'')))
        AND (flex_gsis_id IS NULL OR flex_gsis_id NOT IN
            (COALESCE(qb_gsis_id,''), COALESCE(rb1_gsis_id,''),
             COALESCE(rb2_gsis_id,''), COALESCE(wr1_gsis_id,''),
             COALESCE(wr2_gsis_id,''), COALESCE(wr3_gsis_id,''),
             COALESCE(te_gsis_id,'')))
    )
);

CREATE TRIGGER tg_draft_entries_updated_at
    AFTER UPDATE ON draft_entries FOR EACH ROW
    WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE draft_entries SET updated_at = datetime('now', 'subsec') WHERE id = NEW.id;
END;

-- Entries first: entry_slots references them.
INSERT INTO entries (id, slate_id, fantasy_team_id, created_at, updated_at)
SELECT id, slate_id, fantasy_team_id, created_at, updated_at
FROM _entries_backup;

INSERT INTO entry_slots
    (id, entry_id, roster_slot, gsis_player_id, team_abbr, created_at, updated_at)
SELECT id, entry_id, roster_slot, gsis_player_id, team_abbr, created_at, updated_at
FROM _entry_slots_backup;

INSERT INTO draft_entries
    (id, slate_id, fantasy_team_id, qb_gsis_id, rb1_gsis_id, rb2_gsis_id,
     wr1_gsis_id, wr2_gsis_id, wr3_gsis_id, te_gsis_id, flex_gsis_id,
     def_team_abbr, created_at, updated_at)
SELECT id, slate_id, fantasy_team_id, qb_gsis_id, rb1_gsis_id, rb2_gsis_id,
       wr1_gsis_id, wr2_gsis_id, wr3_gsis_id, te_gsis_id, flex_gsis_id,
       def_team_abbr, created_at, updated_at
FROM _draft_entries_backup;

-- A failed assertion trips the CHECK and rolls the whole migration back.
CREATE TABLE _migration_guard (
    ok INTEGER NOT NULL CHECK (ok = 1)
);

INSERT INTO _migration_guard (ok)
SELECT CASE
    WHEN (SELECT COUNT(*) FROM entries) = (SELECT COUNT(*) FROM _entries_backup)
     AND (SELECT COUNT(*) FROM entry_slots) = (SELECT COUNT(*) FROM _entry_slots_backup)
     AND (SELECT COUNT(*) FROM draft_entries) = (SELECT COUNT(*) FROM _draft_entries_backup)
     AND NOT EXISTS (SELECT 1 FROM pragma_foreign_key_check)
    THEN 1
    ELSE 0
END;

DROP TABLE _migration_guard;
DROP TABLE _entry_slots_backup;
DROP TABLE _draft_entries_backup;
DROP TABLE _entries_backup;
DROP TABLE _contest_slate_map;
