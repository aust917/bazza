# Privacy

Hark is built so that you never have to trust anyone with your meetings.

- **Local by default.** Recordings, transcripts, summaries, chats and voice samples stay in your local library unless you explicitly choose an external integration or share action.
- **No account, no telemetry, no crash reporting.** Every network connection Hark makes is one you can see and turn off:
  - downloading AI models from Hugging Face when you ask;
  - fetching `latest.json` from GitHub releases on startup to check for updates (Settings > Updates; off = no request);
  - fetching the calendar .ics addresses you pasted (Settings > Calendar), on the schedule you set;
  - POSTing finished meetings to the webhook URL you entered (Settings > Webhook, off by default);
  - talking to a model server you configure yourself (off by default).
  - streaming recorded microphone and selected system audio to Soniox, only after you save a key, permit processing and select that provider (off by default);
  - sending a transcript and chosen template through your signed-in Codex or Claude Code CLI, only when you select that agent and generate a summary. The CLI uses its provider's service and subscription limits.
- **Attendee names** come from the meeting window on your own screen (Windows accessibility tree) or your calendar event. They are stored with the meeting and never sent anywhere unless you enable the webhook.
- **No bot.** Hark records what your computer already hears and shows. Nobody joins your call.
- **Only what you pick.** Every recording starts from a picker: the window or display for video, and - on Windows - just that application's audio or everything playing. Nothing else on your screen or speakers is captured.
- **Manual.** Detection only shows a popup. Recording starts when you click Record or press the hotkey. Speaker names are suggestions until you confirm. Share links exist only when you create them, on your network, while Hark is open.
- **Explicit transcription choice.** Local Whisper remains the default. With Soniox selected, confirmed captions and provider provenance are saved locally; a connection failure leaves media and confirmed text intact. Partial text is not promoted to final text. Re-transcribe explicitly selects local Whisper.
- **Revocable cloud capture.** Switching to Local, turning off Soniox consent, or clearing its key stops the current cloud worker while local recording continues. Confirmed text remains; a local re-transcription is a separate action.
- **Voice memory stays separate.** Managed speech samples and voiceprints remain local and are excluded from `.hark` recording exports. Soniox credentials are stored separately in the library database, never returned by ordinary settings or included in recording bundles.
- **Agent summaries are explicit.** Transcript and speaker edits preserve an existing agent summary until you choose to regenerate it. This does not enable agent calls for cleanup, titles, chat or live notes. Live notes continue using the configured local/endpoint language model.
- **Nothing lingers after a crash.** The screen recorder is tied to Hark's own process, so it cannot keep capturing if Hark dies; the interrupted recording is finished from disk on the next launch.

Recording other people may require their consent depending on where you are. Hark makes that easy to do properly, but it is your responsibility.
