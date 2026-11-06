CREATE TABLE service_external_read_grants(
 connection_id TEXT NOT NULL PRIMARY KEY CHECK(length(connection_id)=36),
 library_id TEXT NOT NULL CHECK(length(library_id)=36),
 client_label TEXT NOT NULL CHECK(length(CAST(client_label AS BLOB)) BETWEEN 1 AND 128),
 permission TEXT NOT NULL CHECK(permission='read'),
 credential_digest BLOB NOT NULL CHECK(typeof(credential_digest)='blob' AND length(credential_digest)=32),
 created_at TEXT NOT NULL,
 revoked_at TEXT
);
CREATE INDEX service_external_read_grants_library
 ON service_external_read_grants(library_id,connection_id);
CREATE TRIGGER service_external_read_grants_only_revoke
 BEFORE UPDATE ON service_external_read_grants
 WHEN NEW.connection_id<>OLD.connection_id OR NEW.library_id<>OLD.library_id
   OR NEW.client_label<>OLD.client_label OR NEW.permission<>OLD.permission
   OR NEW.credential_digest<>OLD.credential_digest OR NEW.created_at<>OLD.created_at
   OR OLD.revoked_at IS NOT NULL OR NEW.revoked_at IS NULL
 BEGIN SELECT RAISE(ABORT,'external read grants can only be revoked'); END;
