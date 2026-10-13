CREATE TABLE salary_imports (
    id INTEGER PRIMARY KEY,
    season INTEGER NOT NULL,
    week INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    games INTEGER NOT NULL DEFAULT 0,
    created_by INTEGER REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now','subsec')),
    committed_at TEXT
);
CREATE TABLE salary_import_rows (
    id INTEGER PRIMARY KEY,
    import_id INTEGER NOT NULL REFERENCES salary_imports(id) ON DELETE CASCADE,
    fd_player_id TEXT NOT NULL,
    fd_name TEXT NOT NULL,
    fd_team TEXT NOT NULL,
    dfs_position TEXT NOT NULL,
    salary INTEGER NOT NULL,
    gsis_game_id TEXT NOT NULL,
    gsis_player_id TEXT,
    suggested_gsis_player_id TEXT,
    state TEXT NOT NULL
);
CREATE INDEX ix_salary_import_rows_import ON salary_import_rows(import_id);
