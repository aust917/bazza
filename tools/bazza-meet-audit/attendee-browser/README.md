# Attendee browser diagnostics

`patch.mjs` adds aggregate diagnostics to Attendee’s pinned Google Meet browser payload. It checks the unmodified upstream file against the pinned SHA-256 and creates a new output file. It can run entirely offline once the source is available. [Pinned source](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/bots/google_meet_bot_adapter/google_meet_chromedriver_payload.js)

```text
node tools/bazza-meet-audit/attendee-browser/patch.mjs <original.js> <new-output.js>
node tools/bazza-meet-audit/attendee-browser/test.mjs <original.js>
```

When Attendee requests participant audio, the patched payload sends one `BazzaMeetAuditBrowser` JSON event per second over its existing internal WebSocket. A final snapshot is attempted when media sending stops. Snapshots contain elapsed browser monotonic milliseconds and cumulative numeric counters. The diagnostic event has no audio, names, participant IDs, or URLs. The Python audit receiver writes the counters to the worker log.

`frames_empty_sources` means a nonzero frame had no source in Attendee’s receiver cache. `frames_no_mapped_source` means it had source candidates but none could be assigned the ID used by Attendee. `frames_multiple_sources` counts candidates, which can reflect older packets in the cache. It does not prove that voices overlap in that frame. `send_accepted` counts successful calls to the browser’s internal WebSocket send method. Receipt and forwarding by the Python worker are measured separately.
