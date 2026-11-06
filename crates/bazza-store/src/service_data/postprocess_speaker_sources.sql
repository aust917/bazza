-- v25: diarization results can belong to local ASR or a sealed Soniox source.
-- Preserve the source deletion cascade and all existing immutable outcomes.
CREATE TABLE service_post_process_speaker_results_v25(
 job_id TEXT NOT NULL PRIMARY KEY REFERENCES service_post_process_anchors(job_id) ON DELETE CASCADE,
 source_id TEXT NOT NULL REFERENCES service_transcript_sources(id) ON DELETE CASCADE,
 outcome_json TEXT NOT NULL CHECK(length(CAST(outcome_json AS BLOB))<=128 AND json_valid(outcome_json))
);
INSERT INTO service_post_process_speaker_results_v25(job_id,source_id,outcome_json)
 SELECT r.job_id,l.source_id,r.outcome_json
 FROM service_post_process_speaker_results r
 LEFT JOIN service_post_process_local_sources l ON l.job_id=r.job_id;
DROP TABLE service_post_process_speaker_results;
ALTER TABLE service_post_process_speaker_results_v25 RENAME TO service_post_process_speaker_results;
CREATE INDEX service_post_process_speaker_result_source ON service_post_process_speaker_results(source_id);
CREATE TRIGGER service_post_process_speaker_result_immutable
 BEFORE UPDATE ON service_post_process_speaker_results BEGIN
 SELECT RAISE(ABORT,'post-process speaker result is immutable');
END;
