-- Analysis state/outbox has event + explicit session scope; no arbitrary room.
CREATE TABLE analysis_snapshots(id TEXT PRIMARY KEY, sha256 TEXT UNIQUE NOT NULL, body_json TEXT NOT NULL);
CREATE TABLE analysis_jobs(id TEXT PRIMARY KEY, request_id TEXT UNIQUE NOT NULL, request_sha256 TEXT NOT NULL, state TEXT NOT NULL, created_at_ms INTEGER NOT NULL, body_json TEXT NOT NULL);
CREATE TABLE analysis_changes(seq INTEGER PRIMARY KEY AUTOINCREMENT, entity_kind TEXT NOT NULL, entity_id TEXT NOT NULL, created_at_ms INTEGER NOT NULL, body_json TEXT NOT NULL);
CREATE INDEX analysis_history ON analysis_changes(entity_kind, entity_id, seq);
CREATE INDEX analysis_job_queue ON analysis_jobs(state,created_at_ms,id);
CREATE TABLE analysis_attempts(job_id TEXT NOT NULL REFERENCES analysis_jobs(id), attempt INTEGER NOT NULL, body_json TEXT NOT NULL, PRIMARY KEY(job_id,attempt));
CREATE TABLE analysis_checkpoints(job_id TEXT NOT NULL REFERENCES analysis_jobs(id), step_index INTEGER NOT NULL, input_sha256 TEXT NOT NULL, effective_config_hash TEXT NOT NULL, body_json TEXT NOT NULL, PRIMARY KEY(job_id,step_index,input_sha256,effective_config_hash));
CREATE TABLE analysis_artifacts(id TEXT PRIMARY KEY, created_at_ms INTEGER NOT NULL, body_json TEXT NOT NULL);
CREATE TABLE artifact_revisions(artifact_id TEXT NOT NULL REFERENCES analysis_artifacts(id), revision INTEGER NOT NULL, body_json TEXT NOT NULL, PRIMARY KEY(artifact_id,revision));
CREATE TABLE artifact_dependencies(artifact_id TEXT NOT NULL REFERENCES analysis_artifacts(id), dependency_kind TEXT NOT NULL, dependency_id TEXT NOT NULL, dependency_revision INTEGER, PRIMARY KEY(artifact_id,dependency_kind,dependency_id));
CREATE INDEX artifact_dependencies_reverse ON artifact_dependencies(dependency_kind,dependency_id);
CREATE TABLE publication_projection(artifact_id TEXT PRIMARY KEY REFERENCES analysis_artifacts(id), public_id TEXT UNIQUE NOT NULL, active INTEGER NOT NULL, body_json TEXT NOT NULL);
CREATE TABLE publication_changes(publication_seq INTEGER PRIMARY KEY AUTOINCREMENT, artifact_id TEXT NOT NULL, public_id TEXT NOT NULL, body_json TEXT NOT NULL);
CREATE TABLE analysis_results(job_id TEXT NOT NULL REFERENCES analysis_jobs(id), attempt INTEGER NOT NULL, result_sha256 TEXT NOT NULL, artifact_id TEXT NOT NULL, revision INTEGER NOT NULL, PRIMARY KEY(job_id,attempt));
ALTER TABLE publication_projection ADD COLUMN command_sha256 TEXT;
PRAGMA user_version = 5;
-- Imported transcript timing is not a recording/capture assertion.
CREATE TABLE legacy_imports(event_id TEXT NOT NULL REFERENCES events(id), file_sha256 TEXT NOT NULL, session_id TEXT UNIQUE NOT NULL REFERENCES sessions(id), line_count INTEGER NOT NULL, PRIMARY KEY(event_id,file_sha256));
CREATE TABLE legacy_import_rows(session_id TEXT NOT NULL REFERENCES sessions(id), line INTEGER NOT NULL, original_json TEXT NOT NULL, PRIMARY KEY(session_id,line));
CREATE TABLE analysis_request_keys(request_id TEXT PRIMARY KEY,request_sha256 TEXT NOT NULL,job_id TEXT NOT NULL REFERENCES analysis_jobs(id));
CREATE TABLE publication_reviews(publication_seq INTEGER PRIMARY KEY REFERENCES publication_changes(publication_seq), command_json TEXT NOT NULL);
-- A confirmed runtime Stop must survive before model/config/job availability.
-- AUTOINCREMENT prevents a consumed marker from being reused after deletion.
CREATE TABLE analysis_stop_intents (
 marker INTEGER PRIMARY KEY AUTOINCREMENT,
 session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id)
);
