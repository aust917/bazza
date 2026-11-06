-- v10: service data core. Legacy writers are deliberately not hooked here.
-- Production coherence requires the GUI/worker cutover, not just this schema.
CREATE TABLE service_meeting_state(
  meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
  content_kind TEXT NOT NULL CHECK(content_kind IN ('recorded','text')),
  metadata_revision TEXT NOT NULL,
  lifecycle_revision TEXT NOT NULL,
  tombstoned INTEGER NOT NULL DEFAULT 0 CHECK(tombstoned IN (0,1))
);
INSERT INTO service_meeting_state SELECT id,'recorded',lower(hex(randomblob(16))),lower(hex(randomblob(16))),0 FROM meetings;
CREATE TABLE service_changes(
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  entity_kind TEXT NOT NULL, entity_id TEXT NOT NULL, revision TEXT NOT NULL,
  actor TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE service_idempotency(
  principal TEXT NOT NULL, tool TEXT NOT NULL, key TEXT NOT NULL,
  arguments_hash TEXT NOT NULL, outcome_json TEXT NOT NULL, expires_at TEXT NOT NULL,
  PRIMARY KEY(principal,tool,key)
);
CREATE TABLE service_transcript_sources(
  id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  origin TEXT NOT NULL, source_label TEXT, language TEXT,
  raw_json TEXT NOT NULL CHECK(json_valid(raw_json)), created_at TEXT NOT NULL
);
CREATE INDEX service_transcript_sources_meeting ON service_transcript_sources(meeting_id);
CREATE TRIGGER service_sources_immutable BEFORE UPDATE ON service_transcript_sources BEGIN
  SELECT RAISE(ABORT,'transcript sources are immutable');
END;
CREATE TABLE service_transcript_state(
  meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
  revision TEXT NOT NULL, current_source_id TEXT,
  document_state TEXT NOT NULL CHECK(document_state IN ('absent','present','deleted')),
  deletion_epoch INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE service_transcript_overlays(
  source_id TEXT NOT NULL REFERENCES service_transcript_sources(id) ON DELETE CASCADE,
  segment_key TEXT NOT NULL, clean_text TEXT, manual INTEGER NOT NULL CHECK(manual IN (0,1)),
  PRIMARY KEY(source_id,segment_key), CHECK(manual=0 OR clean_text IS NOT NULL)
);
INSERT INTO service_transcript_sources(id,meeting_id,origin,raw_json,created_at)
 SELECT 'legacy-'||lower(hex(randomblob(16))),m.id,'legacy',
  (SELECT json_group_array(json_object('id',s.id,'start_ms',s.start_ms,'end_ms',s.end_ms,'speaker',s.speaker,'text',s.text))
   FROM (SELECT * FROM segments WHERE meeting_id=m.id ORDER BY start_ms,id) s),m.created_at
 FROM meetings m WHERE EXISTS(SELECT 1 FROM segments WHERE meeting_id=m.id);
INSERT INTO service_transcript_state
 SELECT id,lower(hex(randomblob(16))),(SELECT id FROM service_transcript_sources WHERE meeting_id=m.id),
 CASE WHEN EXISTS(SELECT 1 FROM segments WHERE meeting_id=m.id) THEN 'present' ELSE 'absent' END,0 FROM meetings m;
INSERT INTO service_transcript_overlays
 SELECT src.id,CAST(s.id AS TEXT),s.clean_text,s.clean_text_manual FROM segments s JOIN service_transcript_sources src ON src.meeting_id=s.meeting_id WHERE s.clean_text IS NOT NULL;
CREATE TABLE service_summary_state(
  meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
  revision TEXT NOT NULL, document_state TEXT NOT NULL CHECK(document_state IN ('absent','present','deleted')),
  deletion_epoch INTEGER NOT NULL DEFAULT 0,
  automatic_generation_suppressed INTEGER NOT NULL DEFAULT 0 CHECK(automatic_generation_suppressed IN (0,1)),
  origin TEXT NOT NULL, auto_replace INTEGER NOT NULL CHECK(auto_replace IN (0,1)), source_label TEXT,
  source_revision TEXT, source_missing INTEGER NOT NULL DEFAULT 0 CHECK(source_missing IN (0,1))
);
INSERT INTO service_summary_state
 SELECT id,lower(hex(randomblob(16))),CASE WHEN EXISTS(SELECT 1 FROM summaries WHERE meeting_id=m.id) THEN 'present' ELSE 'absent' END,
 0,0,'legacy',0,NULL,(SELECT revision FROM service_transcript_state t WHERE t.meeting_id=m.id AND t.document_state='present'),0 FROM meetings m;
CREATE TABLE service_summary_history(
  id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  revision TEXT NOT NULL, template_id TEXT NOT NULL, markdown TEXT NOT NULL, structured_json TEXT NOT NULL,
  model TEXT NOT NULL, created_at TEXT NOT NULL, origin TEXT NOT NULL, auto_replace INTEGER NOT NULL, source_label TEXT,
  source_revision TEXT
);
CREATE INDEX service_summary_history_meeting ON service_summary_history(meeting_id);
CREATE TRIGGER service_summary_history_immutable BEFORE UPDATE ON service_summary_history BEGIN
  SELECT RAISE(ABORT,'summary history is immutable');
END;
CREATE TABLE service_notes(
  id TEXT PRIMARY KEY, meeting_id TEXT REFERENCES meetings(id) ON DELETE CASCADE,
  title TEXT NOT NULL, markdown TEXT NOT NULL, revision TEXT NOT NULL,
  origin TEXT NOT NULL, actor TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE INDEX service_notes_meeting ON service_notes(meeting_id,id);
CREATE TABLE service_search_state(
  singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','building','ready','unavailable')),
  cursor_scope TEXT NOT NULL
);
INSERT INTO service_search_state VALUES(1,0,'pending',lower(hex(randomblob(16))));
CREATE TABLE service_search_documents(
  id INTEGER PRIMARY KEY, source_kind TEXT NOT NULL, entity_id TEXT NOT NULL,
  meeting_id TEXT, source_id TEXT, revision TEXT NOT NULL,
  representation TEXT NOT NULL CHECK(representation IN ('raw','effective')),
  segment_id INTEGER, start_ms INTEGER, end_ms INTEGER, text TEXT NOT NULL
);
CREATE INDEX service_search_entity ON service_search_documents(source_kind,entity_id);
CREATE INDEX service_search_segment ON service_search_documents(entity_id,segment_id,representation);
CREATE VIRTUAL TABLE service_search_fts USING fts5(text,content='service_search_documents',content_rowid='id');
CREATE TRIGGER service_search_ai AFTER INSERT ON service_search_documents BEGIN
 INSERT INTO service_search_fts(rowid,text) VALUES(new.id,new.text);
END;
CREATE TRIGGER service_search_ad AFTER DELETE ON service_search_documents BEGIN
 INSERT INTO service_search_fts(service_search_fts,rowid,text) VALUES('delete',old.id,old.text);
END;
CREATE TABLE service_jobs(
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, entity_id TEXT, executor TEXT NOT NULL,
  state TEXT NOT NULL, revision TEXT NOT NULL, actor TEXT NOT NULL,
  cancel_requested INTEGER NOT NULL DEFAULT 0, mandatory INTEGER NOT NULL DEFAULT 0,
  claim_token TEXT, lease_until TEXT, attempts INTEGER NOT NULL DEFAULT 0,
  payload_json TEXT NOT NULL DEFAULT '{}', error_code TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE service_deletions(
  id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, scope TEXT NOT NULL,
  generation TEXT NOT NULL, job_id TEXT NOT NULL REFERENCES service_jobs(id),
  state TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE service_staging(
  id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES service_jobs(id),
  meeting_id TEXT, target_kind TEXT NOT NULL, target_revision TEXT NOT NULL, relative_path TEXT NOT NULL,
  state TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE service_publication_dependencies(
  share_token TEXT NOT NULL REFERENCES shares(token) ON DELETE CASCADE,
  source_kind TEXT NOT NULL, entity_id TEXT NOT NULL, revision TEXT NOT NULL,
  PRIMARY KEY(share_token,source_kind,entity_id)
);
CREATE TABLE service_calendar_sources(
  id TEXT PRIMARY KEY, label TEXT NOT NULL, sync_revision TEXT NOT NULL,
  freshness TEXT NOT NULL, last_success_at TEXT, last_error_code TEXT
);
CREATE TABLE service_calendar_occurrences(
  source_id TEXT NOT NULL REFERENCES service_calendar_sources(id) ON DELETE CASCADE,
  uid TEXT NOT NULL, start_at TEXT NOT NULL, end_at TEXT NOT NULL, title TEXT NOT NULL,
  PRIMARY KEY(source_id,uid,start_at)
);
