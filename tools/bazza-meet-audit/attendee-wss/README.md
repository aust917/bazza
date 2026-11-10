# Attendee provider counters for the Meet audit

The overlay covers Attendee `eaacec16f2939b595f196b3957a717bbb0ed23e9`. Prepare it from a copy of that source and an output directory whose parent already exists.

```powershell
python -B tools/bazza-meet-audit/attendee-wss/patch.py <pinned-source-dir> <new-output-dir>
```

The patcher checks the upstream files by SHA-256 after normalizing line endings, verifies each insertion point and parses the resulting Python. Mount the four files individually, read-only, at `/attendee/<relative path>` in the test worker. The browser overlay provides the separate Google Meet JavaScript patch. Set `BAZZA_MEET_AUDIT=1` on the test worker and leave its log level at `WARNING` or more verbose.

`BAZZA_MEET_AUDIT_BROWSER` receives the browser's numeric counters. `BAZZA_MEET_AUDIT_WSS` counts type-5 messages entering the Python adapter, per-participant audio entering the controller, and external WSS lifecycle and output. The first event in each counter has UTC and monotonic timestamps. Follow `pid` and sort the lines by monotonic timestamp within that process. Client IDs are assigned locally in creation order and `per_participant_*` counters distinguish audio on shared clients. `sent` means the WebSocket `send()` returned successfully. Compare it with the receiver's accepted-frame counts. The queue depth at cleanup is a snapshot.

Both prefixes carry fixed event/counter names and numbers. They omit URLs, IDs from the meeting, participant data, audio and captions. The overlay also removes URL and exception text from the touched WebSocket client logs. Inspect or export the tagged lines only since the unchanged upstream components can produce other logs.

## Isolating one worker from the running QA stack

Use the already built image. Run an auxiliary API at its own loopback port and a disposable recorder worker with a unique queue on the existing Docker network. Keep their container names, mounts and settings separate from the QA services. This approach shares PostgreSQL and Redis so the recorder contributes ordinary test rows and one slot to the same project limit. The original API and workers keep their existing queue and configuration.

Load an auxiliary Django settings module via `DJANGO_SETTINGS_MODULE`. Mount the QA configuration and its callback CA bundle read-only, and add the auxiliary module from a separate directory.

```python
import os
from bazza_settings import *

CELERY_TASK_ROUTES = {
    **CELERY_TASK_ROUTES,
    "bots.tasks.run_bot_task.run_bot": {"queue": os.environ["BAZZA_MEET_AUDIT_QUEUE"]},
}
```

Use the same settings module and queue name in the auxiliary API and worker. Start the worker with `celery -A attendee worker -l WARNING -Q <unique-queue> --concurrency=1 --prefetch-multiplier=1 --max-tasks-per-child=1`. Give only that worker the two code overlays. Preserve the QA worker's callback hostname mapping, TLS trust and shared-memory size. Verify that only the audit worker consumes the new queue before creating the recorder through the auxiliary API. The original API can still launch the speaking bots. Keep credentials in the existing protected environment files.

If no test rows or queues may be added to the shared services, run a separate Compose project with its own database, Redis, API port and project credentials. Reuse the built image for its recorder. The two Attendee instances can join the same dedicated test meeting.
