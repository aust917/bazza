-- v16 component: immutable capture provenance and service-owned current inputs.
-- No raw source/body is duplicated and no read operation initializes a row.
CREATE TABLE service_post_process_anchors(
 job_id TEXT PRIMARY KEY REFERENCES service_jobs(id),meeting_id TEXT NOT NULL,
 capture_fingerprint TEXT NOT NULL,
 root_lifecycle_revision TEXT NOT NULL,root_transcript_revision TEXT NOT NULL,
 root_source_id TEXT,root_document_state TEXT NOT NULL CHECK(root_document_state IN ('absent','present'))
);
CREATE INDEX service_post_process_anchor_meeting ON service_post_process_anchors(meeting_id);
CREATE TRIGGER service_post_process_anchor_immutable BEFORE UPDATE ON service_post_process_anchors BEGIN
 SELECT RAISE(ABORT,'post-process capture anchors are immutable');
END;
CREATE TABLE service_post_process_checkpoints(
 job_id TEXT PRIMARY KEY REFERENCES service_post_process_anchors(job_id),
 revision TEXT NOT NULL,lifecycle_revision TEXT NOT NULL,transcript_revision TEXT NOT NULL,
 source_id TEXT,document_state TEXT NOT NULL CHECK(document_state IN ('absent','present')),
 phase TEXT NOT NULL CHECK(phase IN ('captured','automatic_cleanup')),
 state TEXT NOT NULL CHECK(state IN ('active','invalidated'))
);
