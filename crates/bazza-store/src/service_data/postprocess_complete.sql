-- v20: a terminal worker checkpoint stays distinct from source invalidation.
CREATE TABLE service_post_process_checkpoints_v20(
 job_id TEXT PRIMARY KEY REFERENCES service_post_process_anchors(job_id),
 revision TEXT NOT NULL,lifecycle_revision TEXT NOT NULL,transcript_revision TEXT NOT NULL,
 source_id TEXT,document_state TEXT NOT NULL CHECK(document_state IN ('absent','present')),
 phase TEXT NOT NULL CHECK(phase IN ('captured','local_asr','automatic_cleanup','speakers','ready','ready_cleanup')),
 state TEXT NOT NULL CHECK(state IN ('active','invalidated','completed')),
 CHECK(state<>'completed' OR phase IN ('ready','ready_cleanup'))
);
INSERT INTO service_post_process_checkpoints_v20 SELECT * FROM service_post_process_checkpoints;
DROP TABLE service_post_process_checkpoints;
ALTER TABLE service_post_process_checkpoints_v20 RENAME TO service_post_process_checkpoints;

-- Durable facts about finished fixture work live outside the capture payload
-- and the time-limited request receipt. No transcript or summary body is stored.
CREATE TABLE service_post_process_completions(
 job_id TEXT PRIMARY KEY NOT NULL REFERENCES service_jobs(id),
 outcome_json TEXT NOT NULL CHECK(length(CAST(outcome_json AS BLOB))<=1024 AND json_valid(outcome_json)),
 completed_at TEXT NOT NULL
);
CREATE TRIGGER service_post_process_completion_immutable
 BEFORE UPDATE ON service_post_process_completions BEGIN
 SELECT RAISE(ABORT,'post-process completion is immutable');
END;
