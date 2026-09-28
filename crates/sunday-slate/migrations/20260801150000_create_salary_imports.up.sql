CREATE TABLE salary_imports (
    id           INTEGER PRIMARY KEY,
    season       INTEGER NOT NULL,
    -- The NFL week this slate covers. A slate is always one week, pinned at
    -- planning time by resolve_slate_games.
    week         INTEGER,
    status       TEXT    NOT NULL DEFAULT 'pending',
    games        INTEGER NOT NULL DEFAULT 0,
    created_by   INTEGER REFERENCES users(id),
    created_at   TEXT    NOT NULL DEFAULT (datetime('now','subsec')),
    committed_at TEXT
);

CREATE TABLE salary_import_rows (
    id             INTEGER PRIMARY KEY,
    import_id      INTEGER NOT NULL REFERENCES salary_imports(id) ON DELETE CASCADE,
    fd_player_id   TEXT    NOT NULL,
    fd_name        TEXT    NOT NULL,
    fd_team        TEXT    NOT NULL,
    dfs_position   TEXT    NOT NULL,
    salary         INTEGER NOT NULL,
    gsis_game_id   TEXT    NOT NULL,
    gsis_player_id TEXT,
    -- A likely-but-unproven match offered to the admin for one-click
    -- confirmation. Set only on unmatched rows; never imported without a human
    -- decision.
    suggested_gsis_player_id TEXT,
    state          TEXT    NOT NULL
);

CREATE INDEX ix_salary_import_rows_import ON salary_import_rows(import_id);
