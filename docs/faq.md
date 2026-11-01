# FAQ

**Does it work offline?** Yes, with local Whisper and the local language/speaker models downloaded. Optional Soniox and subscription-agent summaries use their providers' services only after you choose those integrations.

**Is it really free?** Hark is MIT licensed with no mandatory account. Local processing has no service fee. Optional Soniox or agent integrations require your own provider access and its plan/usage limits.

**How accurate is the transcription?** Quality depends on the model, language and recording. With local Whisper, live captions are provisional and the final text comes from the post-call pass. With Soniox selected, confirmed cloud text is retained instead. Cleanup is separate and the original text is kept. Real speech/model/service quality in this local fork still needs control examples and acceptance.

**How long does processing take after a call?** With the CUDA build, an 88-minute call takes about 10 minutes end to end (transcript, speakers, clean-up, summary, search index). On a CPU-only machine Hark measures how fast each speech model runs and picks the best one that finishes within the recording's own length; speaker detection runs across all cores in parallel and alongside transcription.

**Can it record only the meeting and not my music or notifications?** On Windows, yes: pick the meeting window and leave Audio on "This app only". Hark uses Windows' per-application audio capture, so other apps never reach the recording. Your mic is always included.

**What happens if my headset disconnects or the app crashes mid-call?** Hark switches to the current audio device and keeps recording. If the app dies, the recording is on disk already and is finished automatically the next time Hark starts.

**Why didn't it detect my meeting app?** Add a pattern under Settings > Detection, or open an issue with the window title. The audio-activity fallback catches most calls anyway.

**Can I use it for in-person meetings?** Yes: start a recording manually and pick "Everything playing" (or any window; only the mic matters). Speaker identification works from the mic alone. You can also record on your phone and import the file later.

**Can I import an old recording?** Yes. Library > Import takes any audio or video file (mp3, m4a, wav, mp4, mov, ...) and treats it like a recording: transcript, speakers, summary, search. The file's date becomes the meeting date.

**Can I keep a transcript I already have?** Use Library > Media + transcript for a UTF-8 SRT/WebVTT file. This imports ready text without running models or external services. See [formats and limits](import.md).

**Can I manage voice samples?** The Voices page accepts several single-speaker samples from a local file or recording interval. Recognition uses local models and offers names for confirmation. Samples and voiceprints are excluded from recording bundles.

**Can a subscription agent write my summary?** Choose Codex or Claude Code on the meeting's Summary tab, select a template and Generate. Hark uses an installed, signed-in CLI. The page names the recipient before sending the transcript/template; other AI features retain their configured model. Actual subscription access still needs acceptance for this local fork.

**How do I get a follow-up email or a subtitle file?** On the meeting page: Summary > Follow-up email drafts a short recap with decisions and owned action items (editable, Copy). Export copies or saves the transcript as .txt, .srt or .md.

**How do I move my library to a new computer?** Settings > Data > Export everything, copy the `.hark` file, then Import on the other machine. Or move the whole data folder.

**Does it work with Zoom's or Teams' own recording?** Independently. Hark records what your computer plays and your mic; it doesn't care what the meeting app does.

**Why is the Windows installer unsigned?** Code signing certificates cost money. Verify the SHA-256 on the release page if that matters to you, or build from source.

## Where do attendee names come from?

On Windows, Hark reads the names shown in the meeting app's own window (Zoom roster, Teams participant list, Meet tiles) through the accessibility tree, the same way a screen reader would. If you added a calendar source, the event's attendees are used too. Names are only suggestions in the speaker rename box; Hark never assigns them by itself. macOS support for window scraping is planned.

## How do updates work?

On startup Hark fetches one small file (`latest.json`) from GitHub releases. If a newer signed build exists you get a banner; nothing installs until you click. Turn the check off in Settings > Updates.

## Does the webhook send my data somewhere?

Only if you turn it on and enter a URL. Then each finished meeting (title, attendees, summary, transcript) is POSTed to that address once. There is a "Send test" button so you can see the shape of the payload first.

## Hark split one person into two speakers. How do I fix it?

On the meeting page, the **Speakers** list shows every voice it found. Give the extra row the same name as the real person - type it, or pick the person under "Same person as". Transcript labels, talk time and search are updated. An existing subscription-agent summary is preserved; regenerate it explicitly to include corrected names.

The reverse (one row that is really two people) needs a re-run: **Re-run > Speaker detection only**, which re-clusters the voices from the audio.
