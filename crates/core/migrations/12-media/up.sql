CREATE TABLE media (
    service TEXT NOT NULL,
    media_id INTEGER NOT NULL,
    titles TEXT NOT NULL,
    format TEXT,
    season TEXT,
    season_year INTEGER,
    status TEXT,
    episodes INTEGER,
    genres TEXT NOT NULL,
    studio TEXT,
    description TEXT,
    cover_file TEXT,
    described_at INTEGER NOT NULL,
    PRIMARY KEY (service, media_id)
);
