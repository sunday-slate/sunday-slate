CREATE TABLE uploads (
    id INTEGER PRIMARY KEY,
    filename TEXT,
    received_at TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    bytes BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('received', 'parsed', 'failed')),
    diagnostic TEXT
);

CREATE TABLE source_rows (
    id INTEGER PRIMARY KEY,
    upload_id INTEGER NOT NULL REFERENCES uploads(id),
    row_number INTEGER NOT NULL,
    interpreted TEXT NOT NULL,
    UNIQUE (upload_id, row_number)
);

CREATE INDEX idx_source_rows_upload_order ON source_rows (upload_id, row_number);
