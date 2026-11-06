-- v24: a synced output intent survives interruption between install and visibility.
CREATE TABLE service_post_process_video (
    job_id TEXT PRIMARY KEY REFERENCES service_jobs(id) ON DELETE CASCADE,
    capture_id TEXT NOT NULL UNIQUE REFERENCES service_capture_leases(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('prepared','published','unavailable')),
    bytes INTEGER CHECK(bytes BETWEEN 1 AND 9007199254740991),
    digest TEXT CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
    staging_claim TEXT CHECK(length(staging_claim)=36),
    staging_owner TEXT CHECK(length(staging_owner)=36),
    reason TEXT CHECK(reason IN ('missing_raw','ffmpeg_unavailable','mux_failed')),
    change_sequence INTEGER CHECK(change_sequence BETWEEN 1 AND 9007199254740991),
    CHECK (
        (state='prepared' AND bytes IS NOT NULL AND digest IS NOT NULL AND staging_claim IS NOT NULL AND staging_owner IS NOT NULL AND reason IS NULL AND change_sequence IS NULL)
        OR (state='published' AND bytes IS NOT NULL AND digest IS NOT NULL AND staging_claim IS NULL AND staging_owner IS NULL AND reason IS NULL AND change_sequence IS NOT NULL)
        OR (state='unavailable' AND bytes IS NULL AND digest IS NULL AND staging_claim IS NULL AND staging_owner IS NULL AND reason IS NOT NULL AND change_sequence IS NOT NULL)
    )
);
