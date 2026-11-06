-- v13: prepare reserves identity; begin is the durable permission to start raw
-- writers. Existing capturing rows are conservatively treated as already begun.
CREATE TABLE service_capture_leases_v13(
  id TEXT PRIMARY KEY,
  meeting_id TEXT NOT NULL UNIQUE,
  worker_session_id TEXT NOT NULL REFERENCES service_worker_sessions(id),
  library_epoch TEXT NOT NULL,
  generation TEXT NOT NULL,
  fence_hash TEXT NOT NULL,
  start_hash TEXT NOT NULL,
  provider_json TEXT NOT NULL CHECK(json_valid(provider_json)),
  state TEXT NOT NULL CHECK(state IN ('reserved','capturing','finalized','aborted')),
  last_confirmed_sequence INTEGER NOT NULL DEFAULT 0,
  finalize_hash TEXT,
  outcome_json TEXT,
  job_id TEXT REFERENCES service_jobs(id),
  created_at TEXT NOT NULL
);
INSERT INTO service_capture_leases_v13 SELECT * FROM service_capture_leases;
CREATE TABLE service_capture_captions_v13(
  capture_id TEXT NOT NULL REFERENCES service_capture_leases_v13(id) ON DELETE CASCADE,
  sequence INTEGER NOT NULL CHECK(sequence>0),
  segment_id INTEGER NOT NULL,
  caption_hash TEXT NOT NULL,
  PRIMARY KEY(capture_id,sequence)
);
INSERT INTO service_capture_captions_v13 SELECT * FROM service_capture_captions;
-- Drop the old child first: foreign_keys remains enabled throughout migration.
DROP TABLE service_capture_captions;
DROP TABLE service_capture_leases;
ALTER TABLE service_capture_leases_v13 RENAME TO service_capture_leases;
ALTER TABLE service_capture_captions_v13 RENAME TO service_capture_captions;
-- A constant key forbids one reserved AND one capturing row at the same time.
CREATE UNIQUE INDEX service_one_active_capture ON service_capture_leases((1))
 WHERE state IN ('reserved','capturing');
