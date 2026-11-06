CREATE TABLE service_capture_terminal_screen (
    capture_id TEXT PRIMARY KEY REFERENCES service_capture_leases(id) ON DELETE CASCADE,
    evidence_json TEXT NOT NULL CHECK(
        length(CAST(evidence_json AS BLOB)) <= 1024
        AND json_valid(evidence_json)
        AND json_type(evidence_json) = 'object'
    )
);
