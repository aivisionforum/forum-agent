-- Approval is explicitly selected per local session; old sessions remain gated.
CREATE TABLE insight_settings(session_id TEXT PRIMARY KEY REFERENCES sessions(id), mode TEXT NOT NULL CHECK(mode IN ('gated','automatic')));
CREATE TABLE insight_settings_audit(seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id), changed_at_ms INTEGER NOT NULL, command_json TEXT NOT NULL);
CREATE TABLE insight_schedule(session_id TEXT PRIMARY KEY REFERENCES sessions(id), next_update_at_ms INTEGER NOT NULL, blocked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE public_speaker_labels(session_id TEXT NOT NULL REFERENCES sessions(id), speaker_id TEXT NOT NULL, ordinal INTEGER NOT NULL, PRIMARY KEY(session_id,speaker_id), UNIQUE(session_id,ordinal));
PRAGMA user_version = 7;
