# 0001: Small entry points and context on demand

Date: 2026-10-30
Status: accepted

## Context

The inherited repository already separates UI, Tauri orchestration and Rust crates.
It has no agent entry point, contains large historical plans with old tool-specific
instructions, and tracks some generated/session files. Agents would otherwise repeat
discovery or load irrelevant history.

## Decision

- Preserve the source layout so upstream merges remain straightforward.
- Use a short root `AGENTS.md` as a router, with local instructions for distinct areas.
- Keep setup, architecture and workflow details in linked development pages.
- Keep historical specifications and plans under `docs/archive/`, outside normal routing.
- Maintain one optional plan per substantial active task; add no mandatory session log.
- Use dependency-free link and instruction-size checks in a separate lightweight CI job.
- Keep a tiny Copilot adapter; use native AGENTS support for current Claude Code.

## Consequences

An agent can start with a few KiB of guidance and expand only into its task area.
Contributors must update the owning page when a boundary changes. Size/link checks
catch structural drift, while code review still checks factual accuracy. Agents
whose client does not discover scoped files must follow the explicit root routes.

Sources and client caveats are recorded in [agent-workflow.md](../agent-workflow.md).
