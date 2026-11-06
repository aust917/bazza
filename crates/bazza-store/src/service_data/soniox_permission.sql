CREATE TABLE service_soniox_authority(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    permission_epoch TEXT NOT NULL CHECK(length(permission_epoch)=32)
);
INSERT INTO service_soniox_authority VALUES(1,lower(hex(randomblob(16))));

-- No credential or transcript text is stored in this authority ledger.
CREATE TABLE service_soniox_permissions(
    capture_id TEXT PRIMARY KEY NOT NULL REFERENCES service_capture_leases(id),
    library_epoch TEXT NOT NULL CHECK(length(library_epoch)=36),
    permission_epoch TEXT NOT NULL CHECK(length(permission_epoch)=36),
    expires_at TEXT NOT NULL CHECK(length(expires_at)<=64),
    drained_at TEXT CHECK(length(drained_at)<=64)
);

CREATE TRIGGER service_soniox_setting_insert AFTER INSERT ON settings
WHEN new.key IN ('settings','private:soniox_api_key')
BEGIN
    UPDATE service_soniox_authority SET permission_epoch=lower(hex(randomblob(16))) WHERE singleton=1;
END;

CREATE TRIGGER service_soniox_setting_delete AFTER DELETE ON settings
WHEN old.key IN ('settings','private:soniox_api_key')
BEGIN
    UPDATE service_soniox_authority SET permission_epoch=lower(hex(randomblob(16))) WHERE singleton=1;
END;

CREATE TRIGGER service_soniox_setting_update AFTER UPDATE ON settings
WHEN
    (old.key IS NOT new.key AND (old.key IN ('settings','private:soniox_api_key')
                                OR new.key IN ('settings','private:soniox_api_key')))
    OR (old.key='private:soniox_api_key' AND old.value IS NOT new.value)
    OR (old.key='settings' AND CASE
        WHEN json_valid(old.value) AND json_valid(new.value) THEN
            json_extract(old.value,'$.stt_provider') IS NOT json_extract(new.value,'$.stt_provider')
            OR json_extract(old.value,'$.soniox_consent') IS NOT json_extract(new.value,'$.soniox_consent')
            OR json_extract(old.value,'$.soniox_model') IS NOT json_extract(new.value,'$.soniox_model')
            OR json_extract(old.value,'$.language') IS NOT json_extract(new.value,'$.language')
        ELSE old.value IS NOT new.value
    END)
BEGIN
    UPDATE service_soniox_authority SET permission_epoch=lower(hex(randomblob(16))) WHERE singleton=1;
END;
