CREATE TABLE current_salaries (
    id INTEGER PRIMARY KEY,
    gsis_game_id TEXT NOT NULL,
    gsis_player_id TEXT,
    team_abbr TEXT NOT NULL,
    dfs_position TEXT NOT NULL,
    salary INTEGER NOT NULL,
    provenance TEXT NOT NULL CHECK (provenance IN ('legacy', 'import')),
    import_id INTEGER REFERENCES salary_imports(id),
    source_row_id INTEGER REFERENCES salary_import_rows(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (dfs_position IN ('QB', 'RB', 'WR', 'TE', 'DST')),
    CHECK ((dfs_position = 'DST' AND gsis_player_id IS NULL) OR (dfs_position <> 'DST' AND gsis_player_id IS NOT NULL)),
    CHECK ((provenance = 'legacy' AND import_id IS NULL AND source_row_id IS NULL) OR (provenance = 'import' AND import_id IS NOT NULL AND source_row_id IS NOT NULL))
);
CREATE UNIQUE INDEX ux_current_salary_player ON current_salaries(gsis_game_id, gsis_player_id) WHERE dfs_position <> 'DST';
CREATE UNIQUE INDEX ux_current_salary_defense ON current_salaries(gsis_game_id, team_abbr) WHERE dfs_position = 'DST';

CREATE TABLE confirmed_player_mappings (
    fd_player_id TEXT PRIMARY KEY,
    gsis_player_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE salary_imports (
    id INTEGER PRIMARY KEY,
    season INTEGER NOT NULL,
    week INTEGER NOT NULL,
    season_type TEXT NOT NULL,
    games INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'committed')),
    source_upload_id INTEGER NOT NULL,
    received_at TEXT NOT NULL,
    committed_at TEXT
);

CREATE TABLE salary_import_rows (
    id INTEGER PRIMARY KEY,
    import_id INTEGER NOT NULL REFERENCES salary_imports(id) ON DELETE CASCADE,
    source_row_id INTEGER NOT NULL,
    fd_player_id TEXT NOT NULL,
    fd_name TEXT NOT NULL,
    fd_team TEXT NOT NULL,
    original_position TEXT NOT NULL,
    dfs_position TEXT NOT NULL,
    salary INTEGER NOT NULL,
    gsis_game_id TEXT NOT NULL,
    gsis_player_id TEXT,
    suggested_gsis_player_id TEXT,
    state TEXT NOT NULL,
    UNIQUE (import_id, source_row_id)
);
CREATE INDEX ix_managed_salary_rows_import ON salary_import_rows(import_id);

CREATE TABLE migration_state (
    key TEXT PRIMARY KEY,
    completed_at TEXT NOT NULL
);
