-- v18: a registered local ASR result can advance the current source without
-- rewriting the original capture anchor or introducing a generic rebind.
CREATE TABLE service_post_process_local_sources(
 source_id TEXT PRIMARY KEY REFERENCES service_transcript_sources(id) ON DELETE CASCADE,
 job_id TEXT NOT NULL UNIQUE REFERENCES service_jobs(id),
 model TEXT NOT NULL CHECK(length(CAST(model AS BLOB)) BETWEEN 1 AND 256),
 outcome TEXT NOT NULL CHECK(outcome IN ('complete','no_model')),
 caption_count INTEGER NOT NULL CHECK(caption_count BETWEEN 0 AND 10000),
 CHECK(outcome='complete' OR caption_count=0)
);
CREATE TRIGGER service_post_process_local_source_immutable BEFORE UPDATE ON service_post_process_local_sources BEGIN
 SELECT RAISE(ABORT,'post-process local source lineage is immutable');
END;

CREATE TABLE service_post_process_checkpoints_v18(
 job_id TEXT PRIMARY KEY REFERENCES service_post_process_anchors(job_id),
 revision TEXT NOT NULL,lifecycle_revision TEXT NOT NULL,transcript_revision TEXT NOT NULL,
 source_id TEXT,document_state TEXT NOT NULL CHECK(document_state IN ('absent','present')),
 phase TEXT NOT NULL CHECK(phase IN ('captured','local_asr','automatic_cleanup')),
 state TEXT NOT NULL CHECK(state IN ('active','invalidated'))
);
INSERT INTO service_post_process_checkpoints_v18 SELECT * FROM service_post_process_checkpoints;
DROP TABLE service_post_process_checkpoints;
ALTER TABLE service_post_process_checkpoints_v18 RENAME TO service_post_process_checkpoints;

-- Cleanup and local ASR share byte limits and the durable upload identity
-- namespace. Existing cleanup identities and receipts keep their meaning.
ALTER TABLE service_post_process_cleanup_uploads ADD COLUMN upload_kind TEXT NOT NULL DEFAULT 'cleanup'
 CHECK(upload_kind IN ('cleanup','local_source'));
