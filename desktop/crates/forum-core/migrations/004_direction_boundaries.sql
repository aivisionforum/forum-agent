ALTER TABLE sessions ADD COLUMN retroactive_epoch INTEGER NOT NULL DEFAULT 1;
ALTER TABLE sessions ADD COLUMN retroactive_targets_json TEXT;
UPDATE sessions SET retroactive_epoch=direction_epoch,retroactive_targets_json=targets_json WHERE direction_epoch>1;
ALTER TABLE capture_segments ADD COLUMN direction_epoch INTEGER NOT NULL DEFAULT 1;
ALTER TABLE capture_segments ADD COLUMN configured_source_language TEXT;
ALTER TABLE capture_segments ADD COLUMN targets_json TEXT;
UPDATE capture_segments SET direction_epoch=(SELECT direction_epoch FROM sessions JOIN segments ON segments.session_id=sessions.id WHERE segments.id=capture_segments.segment_id);
CREATE TABLE direction_boundaries (
 session_id TEXT NOT NULL REFERENCES sessions(id), direction_epoch INTEGER NOT NULL,
 track_id TEXT NOT NULL REFERENCES tracks(id), start_sample INTEGER NOT NULL,
 configured_source_language TEXT NOT NULL, targets_json TEXT NOT NULL,
 created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
 PRIMARY KEY(session_id,direction_epoch)
);
CREATE INDEX direction_track_boundary ON direction_boundaries(track_id,start_sample,direction_epoch);
PRAGMA user_version=4;
