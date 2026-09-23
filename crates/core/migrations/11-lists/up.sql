CREATE TABLE accounts (
    service TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    expires_at INTEGER,
    signed_in_at INTEGER NOT NULL,
    refused_at INTEGER,
    refusal TEXT,
    signed_out_at INTEGER
);
CREATE TABLE listings (
    entry_id INTEGER NOT NULL REFERENCES entries(id),
    service TEXT NOT NULL,
    tie TEXT NOT NULL,
    found INTEGER,
    media_id INTEGER,
    media_title TEXT,
    media_episodes INTEGER,
    mal_id INTEGER,
    linked_by TEXT,
    tied_at INTEGER NOT NULL,
    base_user INTEGER,
    base_listed INTEGER,
    base_status TEXT,
    base_progress INTEGER,
    base_repeat INTEGER,
    based_at INTEGER,
    repeat_offset INTEGER NOT NULL DEFAULT 0,
    parked_status TEXT,
    parked_progress INTEGER,
    parked_repeat INTEGER,
    parked TEXT,
    parked_at INTEGER,
    PRIMARY KEY (entry_id, service)
);
CREATE TABLE updates (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES entries(id),
    service TEXT NOT NULL,
    progress_before INTEGER NOT NULL,
    progress INTEGER NOT NULL,
    status_before TEXT NOT NULL,
    status TEXT NOT NULL,
    at INTEGER NOT NULL
);
CREATE INDEX updates_by_entry ON updates(entry_id, id);
