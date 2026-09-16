ALTER TABLE history ADD COLUMN moved_to INTEGER REFERENCES entries(id);
ALTER TABLE history ADD COLUMN moved_from INTEGER REFERENCES entries(id);
