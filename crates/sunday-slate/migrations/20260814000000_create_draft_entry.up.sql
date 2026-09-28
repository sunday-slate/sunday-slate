CREATE TABLE draft_entries (
    id              INTEGER PRIMARY KEY,
    contest_id      INTEGER NOT NULL REFERENCES contests(id) ON DELETE CASCADE,
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
    UNIQUE (contest_id, fantasy_team_id),
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
