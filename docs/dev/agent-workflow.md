# Working with coding agents

## Keep context small

`AGENTS.md` owns repository-wide constraints and routes. Scoped files own local
invariants. This directory owns explanations and procedures. Source code, manifests
and tests own implementation facts. Keep one canonical owner for each fact and link
to it instead of copying it into several instruction files.

The root instruction budget is 4 KiB; each scoped file has 3 KiB. These are project
maintenance limits, not model token limits. `pnpm docs:check` enforces them and checks
local Markdown file links outside the archive. It does not validate heading anchors,
external websites or whether prose agrees with code.

Put a rule in always-loaded instructions only when missing it could change a real
implementation decision. Keep tutorials, transcripts, raw tool output and completed
task logs out. Add a reusable skill only after a recurring workflow justifies it.

## One task, one bounded context

1. Identify the expected behavior and the smallest affected area.
2. Read its instructions and relevant code/tests. Consult the architecture page for
   a cross-layer change, the testing page for checks, and the fork guide for releases.
3. Implement and verify the requested behavior. Inspect the diff for unrelated work.
4. Update documentation only when its owned facts change. Report actual verification
   and outstanding limitations, distinguishing a browser mock from a native app run.

Small fixes need no planning ceremony. For work that spans sessions or several
components, create one [task plan](plans/README.md) with scope, acceptance criteria,
decisions, verification evidence and the next action. Update that file rather than
adding a session diary. Git history records completed work.

## Parallel work

Give each worker a bounded outcome, relevant entry points, file ownership and an
expected validation result. Delegate independent areas or read-only reviews. Keep
shared contracts with one owner; coordinate before two workers edit the same file.
Use separate branches/worktrees for independent implementations when useful.

Return a compact handoff: changed paths, findings, checks and blockers. The integrating
agent reviews the combined diff and validates affected boundaries. Do not forward full
transcripts or have every worker rediscover the entire repository.

## Client compatibility

- **Codex:** uses root-to-startup-directory instruction discovery. When launched at
  the root, explicitly read the scoped files for areas you edit.
- **Claude Code:** current versions support `AGENTS.md` directly. This repository
  deliberately has no `CLAUDE.md` adapter: a parent `CLAUDE.md` can suppress default
  AGENTS discovery. Older clients can be explicitly told to read root and scoped files.
- **GitHub Copilot:** `.github/copilot-instructions.md` is a small routing adapter for
  clients that support it. `AGENTS.md` support varies by client; inspect the current
  support matrix rather than assuming every surface loads the same files.
- **Other agents:** supply the root file as the entry point and request the relevant
  scoped file. Instructions guide behavior; deterministic checks belong in CI.

## Research basis

Reviewed 2026-10-30; the choices above are this project's adaptation of primary docs:

- [OpenAI: AGENTS.md discovery](https://learn.chatgpt.com/docs/agent-configuration/agents-md):
  directory scope and instruction precedence.
- [OpenAI: keeping instructions focused](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra):
  minimal routers and task-dependent reading.
- [Claude Code: memory and AGENTS.md](https://code.claude.com/docs/en/memory#agents-md)
  and [best practices](https://code.claude.com/docs/en/best-practices): context hygiene
  and verifiable outcomes.
- [GitHub: repository instructions](https://docs.github.com/en/copilot/how-tos/copilot-on-github/customize-copilot/add-custom-instructions/add-repository-instructions)
  and [support matrix](https://docs.github.com/en/copilot/reference/custom-instructions-support):
  client-specific instruction support.
