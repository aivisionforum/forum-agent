CREATE TABLE events (id TEXT PRIMARY KEY);
CREATE TABLE rooms (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES events(id),
  UNIQUE(id, event_id)
);
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES events(id),
  room_id TEXT NOT NULL,
  owner_device_id TEXT NOT NULL,
  title TEXT NOT NULL,
  spec_json TEXT NOT NULL,
  FOREIGN KEY(room_id, event_id) REFERENCES rooms(id, event_id)
);
CREATE TABLE tracks (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  sample_rate INTEGER NOT NULL CHECK(sample_rate > 0),
  spec_json TEXT NOT NULL,
  UNIQUE(id, session_id)
);
CREATE TABLE ingested_events (
  store_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT NOT NULL UNIQUE,
  event_id TEXT NOT NULL REFERENCES events(id),
  session_id TEXT REFERENCES sessions(id),
  producer_run_id TEXT NOT NULL,
  producer_seq INTEGER NOT NULL CHECK(producer_seq >= 0),
  event_type TEXT NOT NULL,
  body_sha256 TEXT NOT NULL,
  body_json TEXT NOT NULL,
  committed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  UNIQUE(producer_run_id, producer_seq)
);
CREATE TABLE segments (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  track_id TEXT NOT NULL,
  current_revision INTEGER CHECK(current_revision >= 1),
  FOREIGN KEY(track_id, session_id) REFERENCES tracks(id, session_id)
);
CREATE TABLE capture_segments (
  segment_id TEXT PRIMARY KEY REFERENCES segments(id),
  audio_json TEXT NOT NULL,
  recording_ref TEXT,
  created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
  result_seq INTEGER REFERENCES ingested_events(store_seq)
);
CREATE TABLE segment_revisions (
  segment_id TEXT NOT NULL REFERENCES segments(id),
  revision INTEGER NOT NULL CHECK(revision >= 1),
  text TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('success','empty','failed')),
  record_json TEXT NOT NULL,
  created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq),
  PRIMARY KEY(segment_id, revision)
);
CREATE TABLE translation_coverage (
  segment_id TEXT NOT NULL,
  segment_revision INTEGER NOT NULL,
  target_language TEXT NOT NULL,
  direction_epoch INTEGER NOT NULL CHECK(direction_epoch >= 1),
  start_utf8 INTEGER NOT NULL CHECK(start_utf8 >= 0),
  end_utf8 INTEGER NOT NULL CHECK(end_utf8 > start_utf8),
  state TEXT NOT NULL CHECK(state IN ('pending','stale')),
  PRIMARY KEY(segment_id, segment_revision, target_language, direction_epoch, start_utf8, end_utf8),
  FOREIGN KEY(segment_id, segment_revision) REFERENCES segment_revisions(segment_id, revision)
);
CREATE TABLE outbox (
  store_seq INTEGER PRIMARY KEY REFERENCES ingested_events(store_seq),
  message_id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  event_type TEXT NOT NULL,
  body_json TEXT NOT NULL
);
CREATE INDEX segments_session ON segments(session_id);
CREATE INDEX events_session_cursor ON ingested_events(session_id, store_seq);
PRAGMA user_version = 1;
