CREATE TABLE ignored (
    raw_title TEXT PRIMARY KEY,
    at INTEGER NOT NULL,
    stopped_at INTEGER
);
ALTER TABLE history ADD COLUMN kind TEXT;
