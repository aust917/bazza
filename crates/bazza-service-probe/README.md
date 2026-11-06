# Synthetic service lifecycle probe

This phase-0 experiment opens only an explicitly marked synthetic v8 fixture,
read-only. It has no production-library default, Tauri, capture, inference,
network listener, migrations, grant, or MCP protocol. It is not `bazza-service`.

`cargo test -p bazza-service-probe` generates fresh disposable fixtures, launches
a hidden detached process, and checks reading one synthetic meeting after its
launcher exits. A separate Windows experiment kills the launcher tree and
classifies heartbeat survival; breakaway alone does not promise survival of
an ancestry-based tree kill. Tests also cover canonical fixture ownership,
restart, and fail-closed paths.

For an already generated fixture (never a user library):

```powershell
cargo run -p bazza-service-probe -- launch --fixture <absolute-fixture-path> --lifetime-ms 10000
cargo run -p bazza-service-probe -- status --fixture <absolute-fixture-path>
cargo run -p bazza-service-probe -- stop --fixture <absolute-fixture-path>
```

The maximum lifetime is five minutes. Status is a bounded atomic JSON snapshot
inside the disposable fixture, not authenticated RPC or endpoint discovery.
On Windows, atomic replacement retries only access/sharing/lock failures for at
most 250 ms using the same completed temporary file. A persistent failure is
reported without truncating or removing the previous complete JSON snapshot.
The experiment uses `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB` on Windows
and `setsid()` on Unix. A host that refuses breakaway gets an explicit error;
a user-session broker remains a separate installation requirement for that host.
No claim about Codex/Claude Code/macOS support follows without a run in that host.

The explicit Windows broker experiment is
`experiments/windows-user-task.ps1`. It requires an existing synthetic fixture,
the absolute probe executable and a new evidence JSON path. It registers a
uniquely named temporary Task Scheduler task as the current interactive user
with limited privileges, starts it from a hidden launcher, kills that launcher's
tree, checks a later heartbeat, and stops/removes the task in `finally`.
It does not install a persistent service or change existing tasks. Run it
separately from automated tests; it deliberately exercises OS task registration.

Production follow-up requires private authenticated IPC, version negotiation,
secure grants, a Store lifetime owner guard, GUI cutover and legacy-version
interlock. The fixture lock cannot exclude an old production GUI.
