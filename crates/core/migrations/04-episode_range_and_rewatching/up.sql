ALTER TABLE last_match ADD COLUMN episode_end INTEGER;
UPDATE last_match SET episode_end = episode;
ALTER TABLE entries ADD COLUMN rewatching INTEGER NOT NULL DEFAULT 0;
