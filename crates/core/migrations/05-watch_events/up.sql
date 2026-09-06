CREATE TABLE watch_events (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES entries(id),
    episode INTEGER NOT NULL,
    episode_end INTEGER NOT NULL,
    progress_before INTEGER NOT NULL,
    progress INTEGER NOT NULL,
    raw_title TEXT NOT NULL,
    player TEXT NOT NULL,
    at INTEGER NOT NULL,
    undone_at INTEGER
);
CREATE INDEX watch_events_by_entry ON watch_events(entry_id, id);
