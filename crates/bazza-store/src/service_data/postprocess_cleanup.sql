-- v16 component: bounded whole-result automatic overlays, invisible until commit.
-- Retained identity and append hashes contain no transcript text after retirement.
CREATE TABLE service_post_process_cleanup_uploads(
 id TEXT PRIMARY KEY,worker_session_id TEXT NOT NULL,library_epoch TEXT NOT NULL,
 claim_id TEXT NOT NULL,job_id TEXT NOT NULL,meeting_id TEXT NOT NULL,
 checkpoint_revision TEXT NOT NULL,input_json TEXT NOT NULL CHECK(json_valid(input_json)),
 fence_hash TEXT NOT NULL,intent_hash TEXT NOT NULL,
 total_bytes INTEGER NOT NULL CHECK(total_bytes BETWEEN 1 AND 8388608),sha256 TEXT NOT NULL,
 received_bytes INTEGER NOT NULL DEFAULT 0,next_sequence INTEGER NOT NULL DEFAULT 1,
 state TEXT NOT NULL CHECK(state IN ('open','committed','aborted','expired','invalidated')),
 expires_at TEXT NOT NULL,receipt_json TEXT
);
CREATE INDEX service_post_process_cleanup_expiry ON service_post_process_cleanup_uploads(expires_at,id) WHERE state='open';
CREATE INDEX service_post_process_cleanup_input ON service_post_process_cleanup_uploads(meeting_id,state);
CREATE TABLE service_post_process_cleanup_chunks(
 upload_id TEXT NOT NULL REFERENCES service_post_process_cleanup_uploads(id),
 sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 4096),
 byte_offset INTEGER NOT NULL,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,text TEXT,
 PRIMARY KEY(upload_id,sequence)
);
