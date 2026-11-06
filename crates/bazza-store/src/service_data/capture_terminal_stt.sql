-- Bodyless Soniox finalization evidence. Media success and the mutable
-- transcript projection cannot substitute for this captured outcome.
CREATE TABLE service_capture_terminal_stt(
    capture_id TEXT PRIMARY KEY NOT NULL REFERENCES service_capture_leases(id),
    outcome_json TEXT NOT NULL CHECK(
        length(CAST(outcome_json AS BLOB)) BETWEEN 1 AND 512 AND json_valid(outcome_json)
    )
);
CREATE TRIGGER service_capture_terminal_stt_immutable
BEFORE UPDATE ON service_capture_terminal_stt
BEGIN
    SELECT RAISE(ABORT, 'capture terminal STT outcome is immutable');
END;
