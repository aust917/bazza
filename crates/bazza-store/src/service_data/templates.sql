-- v15 component: explicit private templates and typed database upload staging.
-- No builtin seeding or legacy production adoption occurs in this migration.
CREATE TABLE service_template_catalog(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision TEXT NOT NULL);
INSERT INTO service_template_catalog VALUES(1,lower(hex(randomblob(16))));
-- Retained independently of templates and ordinary idempotency retention.
CREATE TABLE service_template_state(
 id TEXT PRIMARY KEY,revision TEXT NOT NULL,generation TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('present','deleted')),builtin INTEGER NOT NULL CHECK(builtin IN (0,1))
);
INSERT INTO service_template_state SELECT id,lower(hex(randomblob(16))),lower(hex(randomblob(16))),'present',builtin!=0 FROM templates;
CREATE TABLE service_template_uploads(
 id TEXT PRIMARY KEY,principal TEXT NOT NULL,begin_key TEXT NOT NULL,intent_hash TEXT NOT NULL,
 template_id TEXT NOT NULL,target_revision TEXT NOT NULL,target_generation TEXT NOT NULL,
 name TEXT,description TEXT,total_bytes INTEGER NOT NULL CHECK(total_bytes BETWEEN 0 AND 8388608),
 sha256 TEXT NOT NULL,received_bytes INTEGER NOT NULL DEFAULT 0,next_sequence INTEGER NOT NULL DEFAULT 1,
 state TEXT NOT NULL CHECK(state IN ('open','committed','aborted','expired')),
 expires_at TEXT NOT NULL,receipt_json TEXT,
 UNIQUE(principal,begin_key)
);
CREATE TABLE service_template_chunks(
 upload_id TEXT NOT NULL REFERENCES service_template_uploads(id),sequence INTEGER NOT NULL,
 byte_offset INTEGER NOT NULL,text TEXT NOT NULL,sha256 TEXT NOT NULL,append_key TEXT NOT NULL,
 PRIMARY KEY(upload_id,sequence),UNIQUE(upload_id,append_key)
);
CREATE INDEX service_template_expiry ON service_template_uploads(expires_at,id) WHERE state='open';
