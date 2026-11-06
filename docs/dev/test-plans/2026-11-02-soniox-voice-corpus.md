# Russian synthetic voice corpus

Status: generated, saved and audio-verified on 2026-11-02. This page owns the local
corpus layout and reuse procedure. Generation and audio validation are separate
from Bazza recognition acceptance. The native sample probe below found false
known-name suggestions for the deliberately unknown synthetic voices.

## Purpose and boundaries

The user requested a saved Soniox TTS corpus on 2026-11-02 for repeatable tests of
voice profiles and multi-speaker recordings. It expands the earlier
[SAPI synthetic Windows checks](../test-reports/2026-11-02-full-automatic-acceptance.md)
with Russian speech, six synthetic identities, unseen voices and degraded audio.

The corpus lives at `.git/local-tests/20261102-soniox-voice-corpus/`, relative to
the repository root. Its local generator is
`.git/local-tools/generate-soniox-voice-corpus.py`. These paths are inside Git's
local metadata and are not tracked source. Keep audio, scripts containing sample
texts, transcripts, provider responses and credentials out of commits. There is
no remote publication or automatic regeneration in CI. A clone will need its own
explicitly generated corpus; the tracked guide does not carry these artifacts.

Only fictional text is sent to Soniox for synthesis. Reuse the saved audio for
local recognition checks; do not upload the user's voice profiles or meetings.
Generation does not alter recording defaults or enroll profiles in a Bazza library.
Provider voice names identify synthesis configurations, not guaranteed acoustic
separation or stable output across future model updates.

## Local commands

Run from the repository root. The utility needs Python, NumPy and
`websocket-client`; stress audio assembly also needs FFmpeg. It reads the Soniox
credential in this order: the process environment (`SONIOX_API_KEY`), repository-root
`Secrets.txt`, then the configured local Bazza acceptance library read-only. In
`Secrets.txt`, locate the `Soniox` section; its credential is on the following
line. This is the user-designated local credential source for other agents. The
file is excluded locally through `.git/info/exclude` (`/Secrets.txt`); preserve
that exclusion and never stage the file. Read only the required credential into
process memory. Never print or copy it to documentation, logs, chat, command-line
arguments, shell history or generated metadata.

```powershell
python .git/local-tools/generate-soniox-voice-corpus.py --catalog
python .git/local-tools/generate-soniox-voice-corpus.py --generate --limit 1
python .git/local-tools/generate-soniox-voice-corpus.py --generate
python .git/local-tools/generate-soniox-voice-corpus.py --build
python .git/local-tools/generate-soniox-voice-corpus.py --verify
```

`--catalog` makes authenticated metadata requests. `--generate` makes paid TTS
requests, with two workers and no automatic retries. `--limit 1` is an initial
probe; the full command reuses valid cached clips rather than regenerating that
probe. `--build` and `--verify` are offline. Generation refuses to overwrite a
cached clip whose request or audio hash differs. Review an error before rerunning;
do not delete the existing corpus to force a fresh paid generation.

For a reviewed defective provider output, `--generate --only-id <utterance-id>`
limits a new paid request to that utterance. Preserve the original WAV, metadata
and rejection reason before replacing a defective clip. This does not authorize
changing voices to improve a recognition score; original rejected outputs belong
in the separate local validation evidence.

Each generated utterance is stored as a 24 kHz mono PCM16 WAV with a matching JSON
metadata file. Metadata records the synthesis request hash, audio hash and stats,
provider character timestamps, and successful `audio_end` plus `terminated`
completion. A partial stream is not a completed cached clip.
The protocol follows the official [TTS WebSocket API](https://docs.soniox.com/api-reference/tts/websocket-api);
the provider's [model](https://docs.soniox.com/tts/models) and
[voice](https://docs.soniox.com/tts/concepts/voices) references explain the selected
IDs. Saved catalog responses and request metadata own this corpus's actual setup.

| Local path inside the corpus | Contents |
| --- | --- |
| `scenario.json` | Fictional speakers, enrollment roles and independent sample/podcast texts |
| `enrollment.json` | Exact known-profile names/sample paths and the two IDs that must remain unknown |
| `provider-models.json`, `provider-voices.json` | Provider catalog metadata used to select the model and voices |
| `samples/known/{sample.id}.wav` and `.json` | Eight samples for the four profiles to enroll |
| `samples/unknown-reference/{sample.id}.wav` and `.json` | Four diagnostic samples; never enroll in the unknown-rejection run |
| `clips/{podcast.id}-turn-XX.wav` and `.json` | Independently synthesized podcast turns |
| `podcasts/{podcast.id}/{clean,noisy,codec,overlap}.wav` | Three podcasts, each with four audio conditions |
| Matching podcast `.json`, `.vtt`, `.clip-bounds.rttm` | Assembly/reference metadata, listening captions and clip-placement bounds |
| `podcasts/{podcast.id}/codec-8k-32kbps.mp3` | Lossy codec intermediate retained for inspection |
| `manifest.json`, `verification.json` | Corpus inventory, hashes, parameters and offline verification results |
| `index.html`, `README.txt` | Local listening entry point and quick reuse notes |
| `validation/` | Separate local native-model evidence when actually run |

## Test design

The current scenario defines these fictional identities. Use stable IDs for
scoring; display names can be entered into the disposable library.

| ID | Fictional profile name | Soniox voice | Enrollment role |
| --- | --- | --- | --- |
| `s1` | Александр Листов | `Daniel` | Known |
| `s2` | Марина Ясная | `Victoria` | Known |
| `s3` | Павел Орехов | `Adrian` | Known |
| `s4` | Елена Мостова | `Grace` | Known |
| `s5` | Игорь Ветров | `Trevor` | Unknown; do not enroll |
| `s6` | Ольга Речная | `Nina` | Unknown; do not enroll |

- Six speakers have stable scenario IDs. The first four are **known**; only their
  sample files belong in the enrollment set. The last two are **unknown** and
  must remain absent from the test library's voice profiles.
- Each voice has two independently generated clean sample texts. Samples for an
  unknown voice are reference material for diagnostics, not enrollment material.
- Three clean podcasts contain two known speakers, all four known speakers, and
  all four known plus two unknown speakers, respectively. Podcast text differs
  from the sample text and is synthesized in separate requests.
- Stress versions add noise, codec degradation and overlapping turns. Keep clean
  results separate from each stress condition; one combined score hides the
  boundary that failed.

| Variant | Applied condition |
| --- | --- |
| `clean` | Separate generated turns with the scenario's gaps |
| `noisy` | Deterministic white noise at 15 dB SNR before global gain; seed and gains recorded |
| `codec` | MP3 at 8 kHz mono, 32 kbps; decoded back to 24 kHz PCM16 with recorded length adjustment |
| `overlap` | One second of adjacent clip overlap at alternate boundaries; clip overlap is not necessarily voiced overlap |

The saved scenario owns speaker roles and text. The generated manifest owns
actual provider model/voice IDs, file paths, audio durations, hashes, parameters
and turn placement. Do not reconstruct expected identity from Bazza's own output
or treat Soniox STT diarization as the reference for local voice matching.

## Reuse in Bazza

1. Verify the saved manifest and audio before testing. Prefer existing files to
   another paid/network generation; a later generation may produce different
   audio even with the same text and voice name.
2. Use a disposable local library. Create exactly the four known profiles and
   add their two clean samples through **Voices**. Leave unknown reference samples
   outside the library. Record the Bazza revision, OS, backend and model hashes.
3. Process the clean podcasts first. Check separately which speaker turns were
   separated and which known names were suggested. Unknown participants should
   remain unnamed; suggested names require user confirmation.
4. Repeat for the stress versions. Count correct suggestions, missed known
   speakers, wrong known names and false suggestions for unknown speakers.
   Preserve per-speaker results instead of reporting only a total percentage.
5. Exercise profile/sample deletion, recomputation and restart independently of
   acoustic quality. Keep results and evidence in a new local run directory, then
   add a concise tracked report with the actual commands and limits.

Assembly timestamps describe placed clips. A clip may contain leading, internal
or trailing silence; those bounds are not an exact speech-activity annotation.
Provider character timestamps, if retained, are another reference with their own
alignment limits. Before reporting speech DER, establish and inspect a speech
reference, state collar and overlap scoring rules, and map inferred cluster IDs
to reference identities. Do not report clip coverage as human-speech DER.

## Verification and remaining acceptance

Generation completed with Soniox `tts-rt-v2`, Russian, 24 kHz mono PCM16. There are
48 accepted source clips: 12 samples and 36 independent podcast turns. The build
produced 12 podcast WAVs (three podcasts times four conditions), their reference
files, the enrollment manifest and a local listening page. Samples last
20.821333–25.856000 seconds.

| Clean podcast | Speakers | Duration in seconds |
| --- | --- | --- |
| `01-two-known` | `s1`, `s2` | 211.800667 |
| `02-four-known` | `s1`–`s4` | 206.410000 |
| `03-known-and-unknown` | `s1`–`s6` | 203.032000 |

The completed command sequence used `--catalog`, an initial `--generate --limit
1` probe, cached full `--generate`, then offline `--build` and `--verify`. One
provider output, `03-known-and-unknown-turn-05`, contained four consecutive
full-scale PCM samples. Its original WAV, JSON and rejection reason remain under
`validation/provider-rejected/`. A reviewed `--generate --only-id
03-known-and-unknown-turn-05` request replaced only that utterance, keeping the
text and voice unchanged; build and verification were then repeated. There were
49 synthesis requests in total, including that rejected output. The initial
probe was reused.

Final `verification.json` has `passed: true` and an empty `failed_checks` list.
It checked hashes for 209 inventoried artifacts and validated the accepted audio's
PCM integrity/clipping and signal levels, stream completion, provider timestamp/text
coverage, enrollment roles, reference bounds and the clean podcasts'
two-to-four-minute duration. All 48 accepted clips matched the provider-normalized
text alignment exactly, and the accepted source and assembled podcast WAVs had
zero full-scale PCM samples. Accepted source audio totals 881.322667 seconds.
The directory contains 211 files (209 hashed artifacts plus manifest and
verification), totaling 174,357,203 bytes. The final
`manifest.json` SHA-256 is
`a29ae2714b1807e696a6c9ae2691b9e8f9110a67d447ed2761ea1bbdfe8ecf04`.
Open `index.html` to listen locally; follow `enrollment.json` when creating a
disposable test library. Audio integrity passed; listening is still useful and
does not substitute for native recognition tests.

An exact-key scan of 140 text files across the corpus, owned documentation and
local utility found zero credential matches. `Secrets.txt` remained locally excluded.

The completed sample-level native probe is saved in `validation/summary.json`,
`cases.json` and `separability.json`. All 12 samples produced exactly one cluster
and one finite, nonzero 192-dimensional embedding. The serial CPU run used two
threads per job and took 25.39 seconds. It used the existing pre-rename executable
with recorded binary, model and runtime hashes; the concurrent Bazza source rename
was not rebuilt for this probe.

Within-speaker cross-sample cosine was 0.924315–0.963452, while the highest
between-speaker sample cosine was 0.830432. Some synthetic identities are close:
unknown `s5` versus known `s3` had centroid cosine 0.851925, and unknown `s6`
versus known `s2` had 0.798397. These values exceeded the original nearest-centroid
rule's 0.62 suggestion threshold and could interact with its 0.75 diarization
merge heuristic.

`validation/matching-probe.json` records an offline probe on the native embeddings
using the then-current app's float32 threshold and tie calculation. With known sample A as
enrollment, all four known sample B holdouts suggested the correct profile.
However, **all four unknown samples received false known-name suggestions**:
`s5` suggested `s3`, and `s6` suggested `s2`. The same four false suggestions
remained when both samples were used in each known profile's centroid. This is
an observed limitation of the offline probe, not an app/UI acceptance result.
Single-speaker sample acceptance therefore does **not** mean unknown rejection
passed. Keep these hard cases; do not swap voices or tune production thresholds
to make this corpus pass.

The subsequent [native matching report](../test-reports/2026-11-02-voice-matching.md)
now records the actual app baseline: it reproduced false unknown-name suggestions
and codec processing failures. The main fixed run now withholds unknowns without
wrong-name suggestions, at the cost of missed known suggestions; the report has
the exact counts and stress limits. Main lifecycle/restart checks, the separate
confirmation/global-invalidation probe and delivered-build reopen checks passed;
see the [completed matching plan](../plans/2026-11-02-voice-matching.md). The original
probes above remain frozen historical evidence.
This corpus does not establish recognition quality for real people, microphones,
rooms, accents or real meeting overlap. Do not calibrate the production threshold
solely on synthetic voices.

See [voice memory](../voice-memory.md) for application behavior and existing native
coverage, and [testing](../testing.md) for local model/runtime prerequisites.
