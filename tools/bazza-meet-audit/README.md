# Meet 8-speaker startup audit

This reads the existing native acceptance artifacts and recalculates the gap between each sender dispatch and the recorder's first retained PCM. Run it from the main checkout at `E:\Claude\Development\Hark`; the QA runtime and private fixtures remain isolated:

```powershell
$root = 'E:\Claude\Development\Hark\.git\local-tests\bazza-call-acceptance'
$id = '7110659b-e452-483f-9f5c-fb33cd3bf6a8'
python -B tools/bazza-meet-audit/audit.py "$root\scenario-8.json" "$root\meet-runs\scale-08-01\speakers.jsonl" "$root\native-live\library\recordings\$id" --diagnostics "$root\native-live\measurement\meet-$id\native-meet-scale-diagnostics.json"
```

The control uses the sender's monotonic dispatch timestamp plus source duration as a reference. This is an estimate of the playback window. The optional diagnostics file supplies the existing local spectral matches. The control does not rescore WAVs, infer playback from ACKs, or classify silence after a track has started.

Expected result on this run: 22 dispatches and ACKs, 9 WAVs for 8 speakers, 8 clips ending before their speaker's first observed and retained PCM, 6 of those 8 with independent spectral evidence in the local recording. The first retained PCM is 166.583 s from the common recorder origin, or 154.732 s from scenario start. For the eight clips the gap to first observation is 16.211–215.281 s. The recorder and mix origins are identical. The Attendee manifest reports zero queue overflow and clock reanchors.

The recorder was admitted 64.6 s before the local recording and had s1–s7 in its roster before capture. Roster membership does not prove the recorder associated their WebRTC source IDs with that roster. The native Meet captions seen by the recorder include early utterances from s1, s2, s3 and s4. Attendee's retained database shows all 22 output requests finished, with media-manager timing within a fraction of a second of the planned clip length. The timer alone cannot confirm microphone output. The captions and local spectral evidence independently confirm that speech reached Meet while the named PCM stream was missing. Historical receiver worker logs were lost when the containers were recreated. The black GDI video and its rejected mux are separate from the PCM audit.

## Cold-start control to locate the drop

1. After the ongoing acceptance finishes, use a dedicated test room and an isolated Attendee worker with the pinned revision. Run once with two speakers and once with eight. Have the recorder join before local start and reproduce the roughly 60-second preconnect interval. Log its admitted state, exact Bazza adoption time, and the driver start. Keep separate logs for each run. Use the same two short, distinctive WAV cues in both room sizes, once just after adoption and again at +5 s and +15 s. Follow with one overlap, one rejoin, and a final pair of cues. Keep the local system audio as the independent reference.
2. Collect counters at five-second intervals through the receiver path. In the recorder's browser, distinguish nonzero WebRTC frames, missing contributing sources, unresolved source IDs and mapped frames sent. In the worker, count browser messages received, external WSS handshake/reconnect, queued/sent messages and drops while disconnected. At Bazza ingress, distinguish socket acceptance, pre-adoption drops, parse/ID rejects and writes to the WAV. Correlate by the shared origin and keep only aggregate counters and times, with no audio bodies, room links, credentials or callback URLs in these logs.
3. Require the first known cue to appear in the correct WAV in both room sizes. Compare with the local waveform. The repeated cue shows whether the path begins working later; the overlap and a separate rejoin check test attribution. If nonzero frames lack a mapped source, inspect the CSRC cache and roster; do not guess the participant. If the worker receives mapped frames before the WSS handshake completes, compare eager connection startup with the baseline. Keep pre-adoption rejection in Bazza and do not introduce a provider PCM backlog. If only Bazza rejects frames after adoption, inspect the exact rejection reason before changing the gate. If WebRTC has no matching frames, inspect the receiver subscription and test layout first.

The existing recording should remain marked as partial. Leading WAV padding cannot recover speech that was never received. A successful warm run does not replace the cold-start check.

## Reproducing the isolated controls

- Runtime: `BazzaMeetAudioAuditQa`, with a separate Attendee API on `127.0.0.1:23497`, an isolated database, and the Windows WSS listener. Stage `baseline` and `candidate` as shown below. Both mount the same browser and worker files read-only and enable the same counters. Run `start_snapshot_worker.sh` inside that distribution with `1` and the variant before each run. The runner checks the worker's overlay, then scales only this distribution after admission. Keep API credentials, room link, callback certificate and private key in their existing private fixtures. Stop only the audit distribution after the bots have left.
- Offline: [browser tests](attendee-browser/README.md) exercise the pinned payload, [provider tests](attendee-wss/README.md) exercise the pinned adapter and WSS client, and the [ingress fixture](bazza-ingress/README.md) sends valid and invalid messages through real loopback TLS/WSS. The latter verifies that pre-adoption PCM is excluded, two correctly named WAVs match their source PCM, and leave is confirmed.
- Live: `prepare_control.py` makes the 2- and 8-speaker controls from the existing synthetic WAV scenario. The first pair of cues starts at +0.3 s and +3.7 s in both rooms, followed by repeats and an overlap. In the 8-speaker case the six other voices are admitted before start and each plays once. Keep a separate local reference recording in the test room to validate clip identity. No rejoin is automated in this short control.

```powershell
$q = 'E:\WSL\BazzaMeetAudioAuditQa'
$fixture = 'E:\Claude\Development\Hark\.git\local-tests\bazza-call-acceptance\scenario-8.json'
$source = '\\wsl.localhost\BazzaMeetAudioAuditQa\opt\bazza-attendee\source'
$stage = 'tools/bazza-meet-audit/attendee-wss-startup/stage_worker.py'
python -B $stage baseline $source "$q\worker-baseline"
python -B $stage candidate $source "$q\worker-candidate"
python -B tools/bazza-meet-audit/prepare_control.py $fixture "$q\control-2" --speakers 2
python -B tools/bazza-meet-audit/prepare_control.py $fixture "$q\control-8" --speakers 8
$exe = 'tools/bazza-meet-audit/bazza-ingress/target/debug/bazza-ingress-audit.exe'
$helper = '/mnt/e/Claude/Development/Bazza/tools/bazza-meet-audit/start_snapshot_worker.sh'
wsl.exe -d BazzaMeetAudioAuditQa -u root --exec bash $helper 1 baseline relay
python -B tools/bazza-meet-audit/check_worker_relay.py (Join-Path (Split-Path $fixture) 'meet-service.json') $q
./tools/bazza-meet-audit/run_control.ps1 -IngressExe $exe -ConfirmDedicatedRoom -Speakers 2 -Variant baseline -Transport relay
```

The preparation commands require fresh directories and can be skipped for the existing controls. Run the same test with `-Speakers 8` after the smaller run. The host admits the recorder and then the named synthetic speakers. The runner waits about a minute after the recorder's admission before adoption. Speech starts after the receiver acknowledges adoption and all speakers have joined. If speaker admission takes longer, the initial WAV timeline contains extra silence and the report aligns against the actual dispatch time.

In relay mode the worker resolves the original callback hostname to its isolated Docker bridge. The bridge listener sends raw TLS bytes over the WSL process pipes, and the Windows relay connects to the receiver on loopback. No firewall rule or certificate trust change is needed. The smoke check uses the existing test certificate and verifies two WSS round trips. The original direct mode remains available for a test receiver already permitted to accept inbound connections.

Keep `receiver-config.json`, raw speaker logs and WAVs private. Run `python -B tools/bazza-meet-audit/analyze_control.py <scenario.json> <run-dir>` to align the sender, browser, worker, ingress and named WAVs. The waveform comparison checks which track contains each synthetic cue. A successful HTTP acknowledgment only means Attendee accepted a playback request.

For an isolated timing run, stage the playout payload, RTP payload and adapter after the candidate above. The runner gathers only validated timing lines in `timing-audit.jsonl`. Keep the room fixture in the isolated runtime root when replacing the default test link.

```powershell
node tools/bazza-meet-audit/attendee-playout/patch.mjs "$source\bots\web_bot_adapter\shared_chromedriver_payload.js" "$q\playout-audit.js"
node tools/bazza-meet-audit/attendee-rtp/patch.mjs "$q\worker-candidate\browser-audit.js" "$q\rtp-audit.js"
python -B tools/bazza-meet-audit/attendee-playout/patch_adapter.py "$q\worker-candidate\provider-overlay\bots\web_bot_adapter\web_bot_adapter.py" "$q\timing-web-bot-adapter.py"
wsl.exe -d BazzaMeetAudioAuditQa -u root --exec bash $helper 1 candidate relay timing
./tools/bazza-meet-audit/run_control.ps1 -IngressExe $exe -ConfirmDedicatedRoom -Speakers 2 -Variant candidate -Transport relay -Timing
```

See [the live results and exact managed Attendee patch paths](live-results.md).

First live attempt: `E:\WSL\BazzaMeetAudioAuditQa\run-25e529c784`. Within the initial 55-second window the API confirmed creation but reported no `joined_meeting` event. The browser had three receivers and 13,878 zero PCM frames, with no mapped or forwarded frame. Ingress never reached `Connected`; no speakers started. Leave and finalization were confirmed. This is an admission-limited control and says nothing about loss of voiced frames in the historical recording. The next-run admission window is 120 seconds, below the ingress admission limit.

The [WSS startup candidate](attendee-wss-startup/README.md) is staged separately. With the pinned unmodified client and a controlled slow handshake, 2 of 2 initial packets were dropped. With the candidate and the same handshake finishing before speech, all 3 packets were queued. A candidate handshake that still overlaps speech preserves the drop behavior and does not replay audio. The candidate also closes a socket returned after cleanup. The fixture proves the startup race and its timing limits. It does not establish the cause of the historical multi-minute gap.

Run the diagnostic baseline in Meet with 2 and 8 speakers. If it locates a loss at WSS startup, repeat with `candidate` in both the worker and runner commands. The stager composes the provider corrections and counters before mounting them. Compare provider and Bazza ingress counters and the output WAVs across runs. Integrate only the evidenced provider correction into the pinned managed Attendee patch set. Retain the existing external provider contract and local WAV format. Gate the change on first-cue identity and coverage, no pre-adoption audio, cleanup on early leave, and valid final tracks after a reconnect or rejoin. The timeline and transcript are outside this change. Keep the diagnostic overlays and synthetic fixtures in the test path.

## Windows window capture acceptance

Run the same checks for **Google Meet in Chrome** and **native Discord**, with the actual meeting or call window selected. The Chrome WGC control produced 109 frames over 7.93 seconds. The Discord 16 WGC control produced one visible frame at 1104×1030. Both are evidence of visibility. Neither supplies an integrated A/V result.

1. Select and validate the exact HWND and its owning process. Verify continuous, changing content over a complete call sample with normal hardware acceleration. Use an animated cue or actual changing call content. Encoder exit, frame count and a single nonblack frame are insufficient.
2. Cover the target with a distinct window, resize it, minimize and restore it, and close it. Check that only the selected window appears. On source loss, surface the video status and continue audio capture. Keep monitor capture available as its own explicit selection.
3. Preserve the first retained frame's QPC origin and subsequent frame timestamps for muxing. Compare A/V markers at start, after pause/resume and near the end of the same recording. Check the full audio tail and seek in the final file. Validate WAV collection separately, including both Meet and Discord per-participant audio. An early Meet PCM gap should be reported separately from any video failure.

- [Pinned CSRC sampling and receiver cache](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/bots/google_meet_bot_adapter/google_meet_chromedriver_payload.js#L1172-L1203)
- [Pinned forwarding gate in the browser](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/bots/google_meet_bot_adapter/google_meet_chromedriver_payload.js#L2214-L2244)
- [Pinned WebSocket client startup](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/bots/bot_controller/bot_websocket_client_manager.py#L46-L61) and [drops before connection](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/bots/bot_controller/bot_websocket_client.py#L65-L71)
