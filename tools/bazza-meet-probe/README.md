# Google Meet speaker probe

The probe plays prepared synthetic clips through separate Attendee bots in a dedicated Google Meet. Bazza or another recorder is started separately. The probe uses Python 3.10+ and FFmpeg with libmp3lame. It does not require a Bazza build.

Validate the shared scenario and exercise encoding without network access:

```powershell
python -B tools/bazza-meet-probe/probe.py .git/local-tests/bazza-call-acceptance/scenario-2.json --prepare
```

The same command accepts the 8-, 16- and optional 32-speaker scenarios. Input is schema v1 with `duration_ms`, `participants`, `events` and `utterances`. WAV paths are relative to the JSON. A fixture inside `.git/local-tests` may refer to another directory within that tree. Times are integer milliseconds from scenario start. Speech bounds are relative to each clip. Clips may overlap across speakers. The probe rejects overlaps on the same speaker because Attendee plays that bot's media requests in order. A separate `markers` list is analysis metadata. Include a cue as an utterance if the bot should play it.

For a live run, provide `BAZZA_ATTENDEE_API_URL`, `BAZZA_ATTENDEE_API_KEY` and `BAZZA_TEST_MEET_URL` through the environment. Use a disposable Attendee test project and a dedicated meeting attended by its organizer and test participants. The live flag confirms that it is a dedicated test meeting. A guest bot requires admission when Google Meet asks for it. If the meeting only admits invited signed-in accounts, provide an Attendee test login group with enough authorized accounts using `--login-group`. Verify that the accounts show up as separate participants.

```powershell
python -B tools/bazza-meet-probe/probe.py .git/local-tests/bazza-call-acceptance/scenario-2.json --confirm-test-meeting --log .git/local-tests/bazza-call-acceptance/meet-2.jsonl
```

The log must be a new file. Initial `join` events at zero are a warm-up. The schedule starts after those bots are admitted. Use `--start-file <new-path>` to wait for the recorder operator to create a local start signal after the log reports `ready_to_start`. Later join and leave requests follow the specified schedule. A clip more than 1 second late is reported as a failed send by default. A timed-out audio POST is not repeated because Attendee might already have queued it.

The probe converts the WAVs to mono MP3 before creating any bots. The pinned Attendee REST `output_audio` endpoint accepts MP3 and stores the supplied media in the project. Acknowledgement means queued, not played. Compare received WAVs against the source signals to verify output latency, overlapping speech and attribution. The probe requests no meeting recording or async transcription audio chunks. Attendee can still collect native Meet captions, as in the Bazza integration. Use only synthetic audio and test participants.

`scenario_start` records the monotonic origin, nearby Unix time and the clock sampling span. `audio_post_started` records dispatch to the HTTP client and `audio_post_ack` records the response. These are not timestamps of audible speech. `bot_created` is the Attendee bot ID. `participant_discovered` is the participant UUID seen by the named observer at poll time, plus the scenario participant ID. Pass `--observer-bot-id` to use an already running recorder in the same project. Otherwise up to two active speaker bots observe each other, since a bot's roster omits itself. UUIDs may differ across observers or rejoins. Only names of this run's synthetic bots are matched and logged. A signed-in account can show its Google account name instead of the requested bot name. `participant_unmatched` flags a missing mapping for manual verification. The token, meeting URL, clip data and text are not logged.

Provision at least **3 / 9 / 17 / 33** available bot slots for **2 / 8 / 16 / 32** speakers plus one recorder. Match the test project's concurrency limit to the available slots and confirm each bot actually joins. The default managed Bazza stack allows one bot. Increase capacity on a separate test instance and scale in stages. Each concurrent bot uses a browser and a long-lived worker slot. Some tasks can sit reserved even with Celery prefetch multiplier 1.

The probe asks every bot it created to leave on completion, failure or Ctrl+C. Inspect any `leave_unconfirmed` or `join_unconfirmed` and use the meeting controls to remove a remaining bot. Any pre-existing observer is left alone.

[Attendee audio API](https://docs.attendee.dev/api-reference/tag/bots/post/api/v1/bots/object_id/output_audio) · [Pinned playback implementation](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/bots/bots_api_views.py#L444-L538) · [Meet admission settings](https://support.google.com/a/users/answer/11989526?hl=en)
