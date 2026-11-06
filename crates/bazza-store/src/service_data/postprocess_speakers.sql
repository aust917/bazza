-- v19: anonymous labels are an overlay on one immutable source. A present
-- row with a NULL label explicitly clears the source's original label.
CREATE TABLE service_transcript_speaker_overlays(
 source_id TEXT NOT NULL REFERENCES service_transcript_sources(id) ON DELETE CASCADE,
 segment_key TEXT NOT NULL, label TEXT,
 PRIMARY KEY(source_id,segment_key)
);
CREATE TABLE service_post_process_speaker_results(
 job_id TEXT NOT NULL PRIMARY KEY REFERENCES service_post_process_local_sources(job_id) ON DELETE CASCADE,
 outcome_json TEXT NOT NULL CHECK(length(CAST(outcome_json AS BLOB))<=128 AND json_valid(outcome_json))
);
CREATE TRIGGER service_post_process_speaker_result_immutable
 BEFORE UPDATE ON service_post_process_speaker_results BEGIN
 SELECT RAISE(ABORT,'post-process speaker result is immutable');
END;

CREATE TABLE service_post_process_checkpoints_v19(
 job_id TEXT PRIMARY KEY REFERENCES service_post_process_anchors(job_id),
 revision TEXT NOT NULL,lifecycle_revision TEXT NOT NULL,transcript_revision TEXT NOT NULL,
 source_id TEXT,document_state TEXT NOT NULL CHECK(document_state IN ('absent','present')),
 phase TEXT NOT NULL CHECK(phase IN ('captured','local_asr','automatic_cleanup','speakers','ready','ready_cleanup')),
 state TEXT NOT NULL CHECK(state IN ('active','invalidated'))
);
INSERT INTO service_post_process_checkpoints_v19 SELECT * FROM service_post_process_checkpoints;
DROP TABLE service_post_process_checkpoints;
ALTER TABLE service_post_process_checkpoints_v19 RENAME TO service_post_process_checkpoints;

-- Keep the shared quota and retain the exact previous upload/chunk rows while
-- extending the closed kind constraint. Copy the child before dropping either.
CREATE TABLE service_post_process_cleanup_uploads_v19(
 id TEXT PRIMARY KEY,worker_session_id TEXT NOT NULL,library_epoch TEXT NOT NULL,
 claim_id TEXT NOT NULL,job_id TEXT NOT NULL,meeting_id TEXT NOT NULL,
 checkpoint_revision TEXT NOT NULL,input_json TEXT NOT NULL CHECK(json_valid(input_json)),
 fence_hash TEXT NOT NULL,intent_hash TEXT NOT NULL,
 total_bytes INTEGER NOT NULL CHECK(total_bytes BETWEEN 1 AND 8388608),sha256 TEXT NOT NULL,
 received_bytes INTEGER NOT NULL DEFAULT 0,next_sequence INTEGER NOT NULL DEFAULT 1,
 state TEXT NOT NULL CHECK(state IN ('open','committed','aborted','expired','invalidated')),
 expires_at TEXT NOT NULL,receipt_json TEXT,
 upload_kind TEXT NOT NULL DEFAULT 'cleanup' CHECK(upload_kind IN ('cleanup','local_source','speakers'))
);
CREATE TABLE service_post_process_cleanup_chunks_v19(
 upload_id TEXT NOT NULL REFERENCES service_post_process_cleanup_uploads_v19(id),
 sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 4096),
 byte_offset INTEGER NOT NULL,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,text TEXT,
 PRIMARY KEY(upload_id,sequence)
);
INSERT INTO service_post_process_cleanup_uploads_v19 SELECT * FROM service_post_process_cleanup_uploads;
INSERT INTO service_post_process_cleanup_chunks_v19 SELECT * FROM service_post_process_cleanup_chunks;
DROP TABLE service_post_process_cleanup_chunks;
DROP TABLE service_post_process_cleanup_uploads;
ALTER TABLE service_post_process_cleanup_uploads_v19 RENAME TO service_post_process_cleanup_uploads;
ALTER TABLE service_post_process_cleanup_chunks_v19 RENAME TO service_post_process_cleanup_chunks;
CREATE INDEX service_post_process_cleanup_expiry ON service_post_process_cleanup_uploads(expires_at,id) WHERE state='open';
CREATE INDEX service_post_process_cleanup_input ON service_post_process_cleanup_uploads(meeting_id,state);
