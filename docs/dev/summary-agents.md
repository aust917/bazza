# Explicit subscription-agent summaries

Settings → AI models shows the local model and a separate guide to manual
subscription summaries. Expand **Check CLI sign-in** to check each installed
CLI before opening a finished meeting.

In the classic library, the meeting's Summary tab defaults to the configured
language model and offers Codex and Claude Code as explicit alternatives. The
selector distinguishes a missing CLI, a required sign-in and a failed check.
Pressing Generate or Regenerate with an agent selected sends the transcript and
chosen template. Template variables can include meeting details and timestamps.
In the service library, select Codex or Claude Code and choose a time range from
the meeting. Bazza prepares a preview with the selected effective transcript
lines and shows the exact text and recipient. Generate sends that preview. A
stale preview is rejected before inference when the meeting, transcript or
saved summary changed. The service checks the same revisions atomically at
save, so a result cannot overwrite a newer summary or claim a newer transcript.
A saved result replaces the previous summary, which remains in revision history.

**Ask Codex or Claude Code** is a separate form. Each send uses only the current
question and the optional text in the context field. Sending a question with an
empty field requires an explicit choice. On the meeting page, an
explicit time range can copy lines from the displayed computer transcript to
that editable field for review. The app uses a local chat key to track the
request. The CLI receives neither that key nor library or chat history.

The app allows one in-flight request per meeting form, or per library form on
the classic Ask page. Closing the form or changing tabs keeps the request
available when it reopens. Cancel targets that request and stops the CLI.
The latest status and reply are held in memory for up to 30 minutes after
completion, subject to a cap on retained requests. Automatic
summaries, cleanup, titles, ordinary chat and live notes use the configured
language-model path.

## Local runner boundary

`bazza-summary-runner` locates an installed native executable from `PATH`. On
Windows it also recognizes the native executable inside the official Codex npm
package. It checks the existing subscription login and passes the rendered template and transcript over
stdin. The same runner passes the question and explicitly pasted text for a
one-off reply. It never installs a CLI, signs in, copies credentials, or launches a shell.
API-key environment variables are removed for the child. Login diagnostics are
not returned to the UI or logs. A missing CLI, unsupported flags, or failed login
leaves the saved summary intact.

Each job has a temporary working directory and bounded input/output (2 MiB each).
Oversized input is rejected without truncation; smaller long transcripts remain
subject to the selected agent's context and subscription limits. A job times out
after ten minutes. A private Windows Job Object or Unix
process group owns descendants, so inherited pipes cannot indefinitely block a
timeout. Temporary files are removed after the process and its readers finish.

Summary jobs and one-off replies expose cancellation. A successful login check is local CLI evidence. Plan quotas and
model access are only known when a request runs.

Codex runs ephemeral, without user configuration or execpolicy rules, with its
command tool and web search disabled, no configured MCP servers, no project
document discovery, and a read-only sandbox. Claude Code uses safe mode, no tools,
strict empty MCP configuration, no slash commands, and no session persistence.
Claude's bare mode is deliberately avoided because it disables subscription
authentication. Administrator-managed policies remain the CLI's responsibility.
This is a local evaluation with user-installed CLIs. Anthropic specifies separate
terms for third-party products that run Claude Code and for routing requests on
users' behalf. See [Claude Code legal and compliance](https://code.claude.com/docs/en/legal-and-compliance).

The result records agent/template provenance in `Summary.structured.generation`
and a model label when returned by the CLI. Failures/cancellation do not replace
the previous result. Automatic transcript/speaker refreshes preserve an existing
agent summary; updating it requires another explicit Generate action. The
configured local/endpoint model remains available for an explicit replacement
in the classic library.

## Evidence and observed limits

On 2026-11-01 both installed CLIs' help confirmed the command flags. Official
references were checked: [Codex non-interactive mode](https://developers.openai.com/codex/noninteractive/),
[Codex configuration](https://developers.openai.com/codex/config-reference/),
[Claude CLI](https://code.claude.com/docs/en/cli-reference), and
[Claude programmatic mode](https://code.claude.com/docs/en/headless).

`cargo test --release -p bazza-summary-runner --offline` covers fake subprocess
stdin, failure, timeout, output bounds, cancellation, descendant cleanup and
synthetic login status. Frontend tests cover explicit invocation, recipient
disclosure, payload isolation, launch failure, and summary cancellation. These
checks make no inference requests.

The [2026-11-02 native Windows follow-up](test-reports/2026-11-02-full-automatic-acceptance.md)
exercised both subscriptions on artificial transcripts: generation, long input,
job reopen, cancellation, oversize rejection and preservation of a previous
summary on failure. A separate model-backed pipeline checked that local
re-transcription, cleanup and a speaker rename preserve a generated Codex summary.
Early cancellation during Claude's login check was mislabeled as login failure;
the saved summary survived. The user chose to retain this finding without a fix.
Restart persistence is tracked in the report. These fixtures do not establish
general summary quality or every provider's context/subscription limits.
