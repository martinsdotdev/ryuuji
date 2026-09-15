CREATE TABLE history (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER REFERENCES entries(id),
    episode INTEGER,
    episode_end INTEGER,
    progress_before INTEGER,
    progress INTEGER,
    raw_title TEXT,
    player TEXT,
    at INTEGER NOT NULL,
    undone_at INTEGER,
    parsed_title TEXT,
    confidence TEXT NOT NULL,
    reason TEXT,
    recorded_at INTEGER,
    added_at INTEGER
);
INSERT INTO history (id, entry_id, episode, episode_end, progress_before, progress,
                     raw_title, player, at, undone_at, confidence, recorded_at)
SELECT id, entry_id, episode, episode_end, progress_before, progress,
       raw_title, player, at, undone_at, 'exact', at
FROM watch_events;
DROP TABLE watch_events;
CREATE INDEX history_by_entry ON history(entry_id, id);
