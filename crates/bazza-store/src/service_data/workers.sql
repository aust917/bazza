-- v11: experimental registered workers and capture handoff. No legacy hooks.
CREATE TABLE service_worker_sessions(
  id TEXT PRIMARY KEY,
  peer_pid INTEGER NOT NULL CHECK(peer_pid>0),
  secret_hash TEXT NOT NULL,
  library_epoch TEXT NOT NULL,
  capabilities_json TEXT NOT NULL CHECK(json_valid(capabilities_json)),
  expires_at TEXT NOT NULL,
  revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)),
  created_at TEXT NOT NULL
);
CREATE TABLE service_capture_leases(
  id TEXT PRIMARY KEY,
  -- Keep the minimal capture identity/receipt after meeting purge so an old
  -- prepare request can never reuse its capture ID to resurrect deleted data.
  meeting_id TEXT NOT NULL UNIQUE,
  worker_session_id TEXT NOT NULL REFERENCES service_worker_sessions(id),
  library_epoch TEXT NOT NULL,
  generation TEXT NOT NULL,
  fence_hash TEXT NOT NULL,
  start_hash TEXT NOT NULL,
  provider_json TEXT NOT NULL CHECK(json_valid(provider_json)),
  state TEXT NOT NULL CHECK(state IN ('capturing','finalized')),
  last_confirmed_sequence INTEGER NOT NULL DEFAULT 0,
  finalize_hash TEXT,
  outcome_json TEXT,
  job_id TEXT REFERENCES service_jobs(id),
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX service_one_active_capture ON service_capture_leases(state) WHERE state='capturing';
CREATE TABLE service_capture_captions(
  capture_id TEXT NOT NULL REFERENCES service_capture_leases(id) ON DELETE CASCADE,
  sequence INTEGER NOT NULL CHECK(sequence>0),
  segment_id INTEGER NOT NULL,
  caption_hash TEXT NOT NULL,
  PRIMARY KEY(capture_id,sequence)
);
