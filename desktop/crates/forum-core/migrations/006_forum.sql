-- Private speaker labels are separate from immutable transcript revisions.
CREATE TABLE speaker_assignments(segment_id TEXT PRIMARY KEY REFERENCES segments(id),session_id TEXT NOT NULL REFERENCES sessions(id),body_json TEXT NOT NULL);
CREATE TABLE speaker_assignment_revisions(segment_id TEXT NOT NULL REFERENCES segments(id),revision INTEGER NOT NULL,body_json TEXT NOT NULL,PRIMARY KEY(segment_id,revision));
CREATE TABLE speaker_assignment_requests(request_id TEXT PRIMARY KEY,command_sha256 TEXT NOT NULL,body_json TEXT NOT NULL);
CREATE TABLE speaker_clusters(speaker_id TEXT PRIMARY KEY,session_id TEXT NOT NULL REFERENCES sessions(id),model_manifest_id TEXT NOT NULL,embedding_json TEXT NOT NULL);
CREATE INDEX speaker_clusters_session ON speaker_clusters(session_id);
-- Peers may write only reviewed public copies, never local session source truth.
CREATE TABLE peer_sessions(session_id TEXT PRIMARY KEY,owner_device_id TEXT NOT NULL,event_id TEXT NOT NULL,body_json TEXT NOT NULL,cursor INTEGER NOT NULL DEFAULT 0,snapshot_sha256 TEXT,last_sync_at_ms INTEGER,stale INTEGER NOT NULL DEFAULT 1,last_batch_sha256 TEXT);
CREATE TABLE peer_publications(alias_id TEXT PRIMARY KEY,session_id TEXT NOT NULL REFERENCES peer_sessions(session_id),public_id TEXT NOT NULL,body_json TEXT NOT NULL,UNIQUE(session_id,public_id));
CREATE TABLE peer_publication_history(alias_id TEXT NOT NULL,revision INTEGER NOT NULL,body_json TEXT NOT NULL,PRIMARY KEY(alias_id,revision));
CREATE TABLE peer_publication_watermarks(alias_id TEXT PRIMARY KEY,publication_seq INTEGER NOT NULL,withdrawn INTEGER NOT NULL);
CREATE TABLE analysis_peer_provenance(snapshot_id TEXT NOT NULL REFERENCES analysis_snapshots(id),alias_id TEXT NOT NULL,body_json TEXT NOT NULL,PRIMARY KEY(snapshot_id,alias_id));
PRAGMA user_version = 6;
