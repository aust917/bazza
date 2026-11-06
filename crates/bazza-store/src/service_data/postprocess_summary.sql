-- v20 component: durable evidence of the generated-summary publication.
-- It contains only identifiers and revisions, never document bodies.
CREATE TABLE service_post_process_generated_summaries(
 job_id TEXT NOT NULL PRIMARY KEY REFERENCES service_post_process_anchors(job_id),
 source_id TEXT NOT NULL,transcript_revision TEXT NOT NULL,summary_revision TEXT NOT NULL,
 template_id TEXT NOT NULL,template_revision TEXT NOT NULL,template_generation TEXT NOT NULL
);
CREATE TRIGGER service_post_process_generated_summary_immutable
 BEFORE UPDATE ON service_post_process_generated_summaries BEGIN
 SELECT RAISE(ABORT,'post-process summary evidence is immutable');
END;

-- Preserve prior upload identities and staged bytes while extending kinds.
CREATE TABLE service_post_process_cleanup_uploads_v20(
 id TEXT PRIMARY KEY,worker_session_id TEXT NOT NULL,library_epoch TEXT NOT NULL,
 claim_id TEXT NOT NULL,job_id TEXT NOT NULL,meeting_id TEXT NOT NULL,
 checkpoint_revision TEXT NOT NULL,input_json TEXT NOT NULL CHECK(json_valid(input_json)),
 fence_hash TEXT NOT NULL,intent_hash TEXT NOT NULL,
 total_bytes INTEGER NOT NULL CHECK(total_bytes BETWEEN 1 AND 8388608),sha256 TEXT NOT NULL,
 received_bytes INTEGER NOT NULL DEFAULT 0,next_sequence INTEGER NOT NULL DEFAULT 1,
 state TEXT NOT NULL CHECK(state IN ('open','committed','aborted','expired','invalidated')),
 expires_at TEXT NOT NULL,receipt_json TEXT,
 upload_kind TEXT NOT NULL DEFAULT 'cleanup'
 CHECK(upload_kind IN ('cleanup','local_source','speakers','generated_summary')),
 CHECK(upload_kind<>'generated_summary' OR total_bytes<=2097152)
);
CREATE TABLE service_post_process_cleanup_chunks_v20(
 upload_id TEXT NOT NULL REFERENCES service_post_process_cleanup_uploads_v20(id),
 sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 4096),
 byte_offset INTEGER NOT NULL,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,text TEXT,
 PRIMARY KEY(upload_id,sequence)
);
INSERT INTO service_post_process_cleanup_uploads_v20 SELECT * FROM service_post_process_cleanup_uploads;
INSERT INTO service_post_process_cleanup_chunks_v20 SELECT * FROM service_post_process_cleanup_chunks;
DROP TABLE service_post_process_cleanup_chunks;
DROP TABLE service_post_process_cleanup_uploads;
ALTER TABLE service_post_process_cleanup_uploads_v20 RENAME TO service_post_process_cleanup_uploads;
ALTER TABLE service_post_process_cleanup_chunks_v20 RENAME TO service_post_process_cleanup_chunks;
CREATE INDEX service_post_process_cleanup_expiry ON service_post_process_cleanup_uploads(expires_at,id) WHERE state='open';
CREATE INDEX service_post_process_cleanup_input ON service_post_process_cleanup_uploads(meeting_id,state);
