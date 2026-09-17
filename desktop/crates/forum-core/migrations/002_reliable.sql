ALTER TABLE sessions ADD COLUMN state TEXT NOT NULL DEFAULT 'created';
ALTER TABLE sessions ADD COLUMN state_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN direction_epoch INTEGER NOT NULL DEFAULT 1;
ALTER TABLE sessions ADD COLUMN targets_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE sessions ADD COLUMN capture_stopped_seq INTEGER;
ALTER TABLE sessions ADD COLUMN sealed_seq INTEGER;
ALTER TABLE sessions ADD COLUMN incomplete INTEGER NOT NULL DEFAULT 0;
CREATE TABLE session_history (
 session_id TEXT NOT NULL REFERENCES sessions(id), store_seq INTEGER NOT NULL,
 status_json TEXT NOT NULL, PRIMARY KEY(session_id,store_seq)
);
CREATE TABLE capture_seals (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id), manifest_sha256 TEXT NOT NULL,
 payload_json TEXT NOT NULL, created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq)
);
CREATE TABLE producer_seals (
 producer_run_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
 final_seq INTEGER NOT NULL, payload_json TEXT NOT NULL,
 created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq)
);
CREATE TABLE audio_gaps (
 gap_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
 track_id TEXT NOT NULL, payload_json TEXT NOT NULL,
 created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
 FOREIGN KEY(track_id,session_id) REFERENCES tracks(id,session_id)
);
ALTER TABLE translation_coverage RENAME TO old_translation_coverage;
CREATE TABLE translations (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
 current_revision INTEGER NOT NULL, current_attempt INTEGER NOT NULL
);
CREATE TABLE translation_attempts (
 translation_id TEXT NOT NULL REFERENCES translations(id), revision INTEGER NOT NULL,
 attempt INTEGER NOT NULL, request_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('requested','final','passthrough','failed','stale')),
 text TEXT, error TEXT, result_json TEXT, created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
 updated_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
 PRIMARY KEY(translation_id,revision,attempt)
);
CREATE TABLE translation_sources (
 translation_id TEXT NOT NULL REFERENCES translations(id), revision INTEGER NOT NULL,
 source_index INTEGER NOT NULL, segment_id TEXT NOT NULL, segment_revision INTEGER NOT NULL,
 start_utf8 INTEGER NOT NULL, end_utf8 INTEGER NOT NULL, quote TEXT NOT NULL,
 PRIMARY KEY(translation_id,revision,source_index),
 FOREIGN KEY(segment_id,segment_revision) REFERENCES segment_revisions(segment_id,revision)
);
CREATE TABLE translation_coverage (
 segment_id TEXT NOT NULL, segment_revision INTEGER NOT NULL, target_language TEXT NOT NULL,
 direction_epoch INTEGER NOT NULL CHECK(direction_epoch>=1), start_utf8 INTEGER NOT NULL CHECK(start_utf8>=0),
 end_utf8 INTEGER NOT NULL CHECK(end_utf8>start_utf8),
 state TEXT NOT NULL CHECK(state IN ('pending','requested','final','failed','stale')),
 translation_id TEXT, translation_revision INTEGER, attempt INTEGER,
 PRIMARY KEY(segment_id,segment_revision,target_language,direction_epoch,start_utf8,end_utf8),
 FOREIGN KEY(segment_id,segment_revision) REFERENCES segment_revisions(segment_id,revision),
 FOREIGN KEY(translation_id,translation_revision,attempt) REFERENCES translation_attempts(translation_id,revision,attempt)
);
INSERT INTO translation_coverage(segment_id,segment_revision,target_language,direction_epoch,start_utf8,end_utf8,state)
 SELECT segment_id,segment_revision,target_language,direction_epoch,start_utf8,end_utf8,state FROM old_translation_coverage;
DROP TABLE old_translation_coverage;
CREATE INDEX translation_session ON translations(session_id);
CREATE INDEX translation_source_revision ON translation_sources(segment_id,segment_revision);
CREATE INDEX coverage_pending ON translation_coverage(state,segment_id);
UPDATE sessions SET state='recording' WHERE EXISTS(SELECT 1 FROM segments WHERE segments.session_id=sessions.id);
PRAGMA user_version=2;
