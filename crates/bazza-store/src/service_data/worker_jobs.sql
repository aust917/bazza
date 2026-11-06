-- v15 aggregate: registered-worker post-process ownership. No document bodies,
-- caller secrets, paths or externally usable internal claim tokens are stored.
CREATE TABLE service_worker_job_claims(
  claim_id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  worker_session_id TEXT NOT NULL REFERENCES service_worker_sessions(id),
  library_epoch TEXT NOT NULL,
  fence_hash TEXT NOT NULL,
  token_hash TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','retired','failed')),
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX service_worker_job_active ON service_worker_job_claims(job_id) WHERE state='active';
CREATE TABLE service_worker_job_receipts(
  worker_session_id TEXT NOT NULL REFERENCES service_worker_sessions(id),
  operation_id TEXT NOT NULL,
  library_epoch TEXT NOT NULL,
  operation TEXT NOT NULL CHECK(operation IN ('claim','renew','fail')),
  arguments_hash TEXT NOT NULL,
  outcome_json TEXT NOT NULL CHECK(json_valid(outcome_json)),
  expires_at TEXT NOT NULL,
  PRIMARY KEY(worker_session_id,operation_id)
);
