# Explicit subscription-agent summaries

The meeting's Summary tab offers the configured language model, Codex, and Claude
Code. An agent is invoked only when the user selects it and presses Generate or
Regenerate. The UI names the recipient of the transcript and template. Automatic
summaries, cleanup, titles, chat, and live notes keep their existing LLM path.

## Local runner boundary

`hark-summary-runner` locates an installed native executable on `PATH`, checks its
existing subscription login, and passes the rendered template and transcript over
stdin. It never installs a CLI, signs in, copies credentials, or launches a shell.
API-key environment variables are removed for the child. Login diagnostics are
not returned to the UI or logs. A missing CLI, unsupported flags, or failed login
leaves the saved summary intact.

Each job has a temporary working directory and bounded input/output (2 MiB each).
Oversized input is rejected without truncation; smaller long transcripts remain
subject to the selected agent's context and subscription limits. A job times out
after ten minutes and supports cancellation. A private Windows Job Object or Unix
process group owns descendants, so inherited pipes cannot indefinitely block a
timeout. Temporary files are removed after the process and its readers finish.

Codex runs ephemeral, without user configuration or execpolicy rules, with its
command tool and web search disabled, no configured MCP servers, no project
document discovery, and a read-only sandbox. Claude Code uses safe mode, no tools,
strict empty MCP configuration, no slash commands, and no session persistence.
Claude's bare mode is deliberately avoided because it disables subscription
authentication. Administrator-managed policies remain the CLI's responsibility.

The result records agent/template provenance in `Summary.structured.generation`
and a model label when returned by the CLI. Failures/cancellation do not replace
the previous result. Automatic transcript/speaker refreshes preserve an existing
agent summary; updating it requires another explicit Generate action. The
configured local/endpoint model remains available for an explicit replacement.

## Evidence and remaining acceptance

On 2026-11-01 both installed CLIs' help confirmed the command flags. Official
references were checked: [Codex non-interactive mode](https://developers.openai.com/codex/noninteractive/),
[Codex configuration](https://developers.openai.com/codex/config-reference/),
[Claude CLI](https://code.claude.com/docs/en/cli-reference), and
[Claude programmatic mode](https://code.claude.com/docs/en/headless).

`cargo test --release -p hark-summary-runner --offline` verifies fake subprocess
stdin, failure, timeout, output bounds, cancellation, and descendant cleanup.
The SummaryControls tests verify explicit invocation, recipient disclosure,
launch failure, and cancellation. These checks make no inference requests.

Actual subscription access, server limits, summary quality, and both agents'
behavior on a long artificial transcript still require explicitly selecting and
running each integration. Installed executables and passing fake tests do not
establish R8 acceptance. No real meeting was sent to either agent in this task.
