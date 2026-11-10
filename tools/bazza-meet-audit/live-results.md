# Live Meet audio results

All tests used the pinned Attendee revision `eaacec16f2939b595f196b3957a717bbb0ed23e9`, the isolated `BazzaMeetAudioAuditQa` WSL distribution and the standalone Bazza ingress snapshot. The signed-in organizer joined the closed test room with camera and microphone off. Only approved synthetic voices played.

## WSS startup change for integration

The [pure provider patcher](attendee-wss-startup/patch.py) stages three files under `bots/bot_controller`. The generated files are under `E:\WSL\BazzaMeetAudioAuditQa\startup-candidate`.

- `bot_controller.py` starts the per-participant WSS client in Google Meet's `JOINING` action, before starting the browser adapter.
- `bot_websocket_client_manager.py` adds idempotent startup for that configured client.
- `bot_websocket_client.py` prevents a connection that finishes after cleanup from putting the stopped client back into service.

The [managed provisioner proposal](attendee-wss-startup/managed-provision-wss.patch) embeds the tested pure patcher and mounts its three files only in the Windows managed worker. The [delivery checks and scope](attendee-wss-startup/README.md#proposed-managed-windows-delivery) cover cold setup, warm reuse and file repair. The [A/B stager](attendee-wss-startup/stage_worker.py) adds the browser and worker diagnostics separately. The deployment boundary retains Bazza's adoption gate and the provider's drop behavior when the socket is unavailable. There is no startup audio buffer.

## Candidate in a real call

| Control | Run under `E:\WSL\BazzaMeetAudioAuditQa` | Named WAVs | Worker PCM sent / Bazza PCM consumed | Authenticated WSS before adoption |
| --- | --- | ---: | ---: | ---: |
| Two voices | `run-bed9e9b2e9` | 2 | 2,344 / 2,344 | 79.553 s |
| Eight voices | `run-e694dc74e5` | 8 | 4,930 / 4,930 | 104.639 s |

Both runs have zero provider drops while disconnected and zero audit drops. The packet counts refer to the path from the worker to the WAV writer. On two voices the manifest reports 375,040 PCM samples retained out of 375,040 received. On eight voices it reports 775,360 retained out of 788,800 received. The packet comparison does not account for per-track sample placement and overlap trimming.

In the two-voice run, both early cue starts appear on the intended WAVs. In the eight-voice run, the first two speakers' early cues are identifiable on the intended WAVs. The first speaker's first 400 ms matches independently of the later cue sections, whose timing does not fit a single duration ratio. The first retained PCM is at 28.056 s from adoption, about 385 ms after the first dispatch. Late speaker admission postponed dispatch in this run.

Speaker 6 in the eight-voice run produced a distinct match in the intended WAV at about 68.920 s, roughly 11.250 s after its dispatch. The first named PCM was observed at about 69.076 s before placement in the WAV. Two of six 400 ms source sections match. The track has only about 1.2 s of PCM there, and recording continued for over 7 s after that interval. Attendee's own media request took 9.741 s from creation to completion. The packet counts show no drops from the worker to the writer and do not identify the cause of the partial cue. The browser counted 18 nonzero frames with no contributing source across the call, 11 within the early snapshot bracket. The aggregate counters do not identify the affected speaker.

The direct transport test `run-affb1b424e` is a separate failure. Attendee received and dropped 2,846 participant packets when Windows blocked inbound traffic to the standalone receiver. The isolated loopback relay passed two trusted WSS round trips before the candidate runs. The managed application's Windows Firewall configuration was left intact.

The same-room baselines with eight voices (`run-77cfe82492`) and two voices (`run-ec57c006f7`) both stopped before playback when one speaker missed admission. The deterministic A/B fixture compares both variants on the identical source revision and handshake schedule. The live candidate proves preconnection, correct first-cue routing and end-to-end packet conservation after the worker sees PCM.

## Duration evidence

The source cue is 57,600 samples at 24 kHz, exactly 2.400 s. The probe's submitted MP3 byte hashes match the resources persisted by Attendee. They decode to mono 44.1 kHz and 2.429388 s, including MP3 framing. The source and the recorded WAV therefore differ by more than the encoder's approximately 29 ms on some cues.

In the two-voice run, early speaker 1 spans 266 consecutive 320-byte WSS packets. Each packet is 160 PCM16 samples at 16 kHz, and their total duration is 2.660 s. Provider arrival PTS run from 988 to 3,634 ms. Both output WAVs declare 16 kHz. Across the run the track manifest accounts for all 375,040 received samples and adds no internal silence within the six matched cue windows. The WAV writer did not resample or stretch those packets. The same second-speaker MP3 matches at an estimated 1.11 duration ratio early and approximately 1.00 later. A fixed sample rate mismatch in the MP3 or WAV header cannot explain that variation.

The independent [tempo check](analyze_tempo.py) reproduces 1.100–1.110 duration ratios for five cues and 1.000 for the sixth. The median recorded-to-source fundamental frequency ratio remains 1.000–1.006. In particular, a fixed rate conversion sufficient to lengthen the waveform by 11% would also lower pitch by about 10%, which the measurements do not show.

The additional instrumented two-voice call `run-c3e205e638` captured all six sender cues in both browser clocks. Each had 2,429.388 ms scheduled at 44.1 kHz, playback rate 1.0 and no gap between its three buffers. Mapping the audio context endpoints through Chrome's `getOutputTimestamp()` yielded 2,429.018–2,429.224 ms on the output clock. The first two received WAV cues were still 1.115× and 1.080× their source duration with no internal writer gap, and the first cue's measured pitch ratio was 1.002.

On the receiving browser, 75 stable adjacent RTP reports counted 63,506 samples inserted for deceleration, zero samples removed for acceleration and 783,360 audio samples emitted by the WebRTC jitter buffer (Chromium NetEq). The inserted samples are 8.11% of the reported emitted samples. The sender clocks, receive-side sample insertion and continuous early WAV data show a pitch-preserving adjustment in the receiving WebRTC path. These RTP counters are aggregate across three receivers and do not quantify how much adjustment belongs to each speaker or how much of each cue's duration change it explains. The WSS startup fix and the receive-side timing behavior are separate.

Reproduce the clock and RTP totals with `python -B tools/bazza-meet-audit/analyze_browser_timing.py <run-dir>/timing-audit.jsonl`, and the received cue and pitch measurements with `python -B tools/bazza-meet-audit/analyze_tempo.py <scenario.json> <run-dir>`.
