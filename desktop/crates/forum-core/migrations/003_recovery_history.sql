CREATE TABLE producer_reconciliations (
 producer_run_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
 final_seq INTEGER NOT NULL, payload_json TEXT NOT NULL,
 created_seq INTEGER NOT NULL REFERENCES ingested_events(store_seq)
);
CREATE TABLE translation_history (
 translation_id TEXT NOT NULL REFERENCES translations(id), store_seq INTEGER NOT NULL,
 record_json TEXT NOT NULL, PRIMARY KEY(translation_id,store_seq)
);
CREATE TABLE snapshot_floors (projection TEXT PRIMARY KEY, min_cursor INTEGER NOT NULL);
INSERT INTO snapshot_floors VALUES('translation_history',(SELECT COALESCE(MAX(store_seq),0) FROM ingested_events));
INSERT INTO translation_history(translation_id,store_seq,record_json)
 SELECT t.id,(SELECT min_cursor FROM snapshot_floors WHERE projection='translation_history'),
 json_object('session_id',t.session_id,'request',json(a.request_json),'result',json(a.result_json),'state',a.state,'text',a.text,'error',a.error,'created_seq',(SELECT MIN(v.created_seq) FROM translation_attempts v WHERE v.translation_id=t.id),'updated_seq',a.updated_seq)
 FROM translations t JOIN translation_attempts a ON a.translation_id=t.id AND a.revision=t.current_revision AND a.attempt=t.current_attempt;
PRAGMA user_version=3;
