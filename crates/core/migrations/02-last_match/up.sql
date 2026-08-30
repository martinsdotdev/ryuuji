CREATE TABLE last_match (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    raw_title TEXT NOT NULL,
    parsed_title TEXT NOT NULL,
    episode INTEGER,
    season INTEGER,
    release_group TEXT,
    entry_id INTEGER REFERENCES entries(id) ON DELETE SET NULL,
    confidence TEXT NOT NULL,
    outcome TEXT NOT NULL,
    player TEXT NOT NULL,
    at INTEGER NOT NULL
)