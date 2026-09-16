CREATE TABLE aliases (
    needle TEXT PRIMARY KEY,
    parsed_title TEXT NOT NULL,
    entry_id INTEGER NOT NULL REFERENCES entries(id),
    at INTEGER NOT NULL
);
