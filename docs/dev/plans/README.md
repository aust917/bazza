# Task plans and handoffs

Create a plan only for a long-running task, a substantial cross-component change or
work that another session needs to continue. Ordinary fixes use the task conversation
and Git diff. There is no automatic requirement to read every plan.

Use `YYYY-MM-DD-short-task.md` here. Keep one file per task, link to evidence instead
of pasting logs, and keep the next action current. Completed plans move to
`docs/archive/` when useful history remains; otherwise Git history is sufficient.
Never commit meeting content, secrets or machine-local paths as handoff context.

Copy only the sections the task needs:

```markdown
# Task title

Status: active | blocked | complete
Owner / branch:

## Outcome and boundaries
Expected behavior, scope and relevant entry points. Link to existing instructions.

## Acceptance criteria
- Observable result that establishes completion.

## Steps
- [ ] Small, testable step.

## Decisions and findings
Only facts needed to avoid repeated discovery; distinguish proposals from decisions.

## Verification
Commands actually run, results, environment and any missing coverage.

## Handoff
Current state, unresolved issue and exact next action.
```

A durable architectural decision belongs in `docs/dev/decisions/NNNN-topic.md` with
context, decision and consequences. Task progress belongs in its plan, not in
`AGENTS.md`. Do not invent a roadmap while documenting existing code.
