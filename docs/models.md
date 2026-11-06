# Models and hardware

Everything runs on your machine. Bazza chooses model sizes from what it finds:

| Tier | Picked when | Live captions | Final transcript | Summaries and chat |
|---|---|---|---|---|
| CPU, light | < 12 GB RAM, no GPU | Whisper base | Whisper small | Qwen3 1.7B |
| CPU, roomy | 12 GB+ RAM, no GPU | Whisper base | Whisper small | Qwen3 4B |
| GPU | NVIDIA 6 GB+ VRAM (CUDA build) or Apple Silicon | Whisper small | Whisper large-v3-turbo | Qwen3 8B |

On CPU-only machines Bazza also measures how fast each speech model really runs and, for long recordings, picks the best downloaded model that finishes within the recording's own length (a 90-minute call never becomes a three-hour wait). Whisper medium stays available in Settings for short clips where accuracy matters more than time. Speaker detection runs alongside transcription.

The plain Windows (CPU) installer never picks the GPU row, even on a machine with an NVIDIA card: it cannot use the card, and the GPU-sized models would crawl on the CPU. Install the CUDA build to use the card, or pick a tier by hand in Settings.

Meeting speaker detection prefers **Nemotron 3 Q8** (107 MB) when downloaded.
The bundled CPU runtime processes the recording as one continuous stream, reading
audio in 15-second windows and preserving speaker context across them. The same
short-turn policy applies before and after native history compaction; recording
length does not determine independent speaker shards. The model supports up to
eight anonymous speakers. It does not transcribe speech or supply a person's name.

**NeMo TitaNet Small** (40 MB) still computes voiceprints in the existing space,
so saved voice profiles and samples remain compatible. **pyannote segmentation
3.0** (6 MB) remains required to validate explicitly added voice samples. It also
provides the previous parallel-shard meeting path before Nemotron is downloaded.
Downloading a new model remains explicit in Settings or onboarding; processing
does not download models. A Nemotron runtime/inference failure is reported as an
optional processing error, rather than silently switching algorithms.

Invalid TitaNet meeting queries can be recovered from a complete partition into
shorter windows. Valid original queries keep their existing values. Additional
windows sampled across each cluster's complete timeline can veto conflicting
name suggestions and prevent uncertain pooled queries from teaching Voices.
These checks keep the same model and existing voice database; they do not promise
recognition of isolated short replies. See the [voice evidence policy](dev/voice-memory.md)
for the separate naming and reuse rules.

The pinned Nemotron CPU runtime is available for Windows x64, macOS ARM64 and
macOS x64; its macOS binaries require 13.3 or newer. macOS 13.0–13.2 retains the
legacy meeting path. Windows ARM64 has no pinned native asset. macOS native
acceptance must be performed on a Mac; a Windows build cannot establish it.
See [voice profiles](dev/voice-memory.md) for matching policy and measured limits.

Semantic search in Ask Bazza uses BGE small v1.5 (37 MB); without it, search is keyword-only.

Live captions and live notes during a call use the tier's live-caption model and the language model; they are provisional and replaced by the full pass when you stop.

Measured on the CUDA build with an RTX 3080: an 88-minute call is transcribed in under 4 minutes while speakers are detected in parallel, then cleaned up and summarised in about 7 minutes. The language model generates around 90 tokens per second there versus about 10 on the same machine's CPU.

On first run Bazza suggests downloading the *essentials* (live-caption model, speaker models, search model) and finishing the larger transcript and language models in the background. Choose **Skip for now** to open the library and download any remaining models later in Settings. Enable automatic cleanup or summaries in Settings to queue that step when the language model is still downloading.

Override any of this in Settings > AI models. Bigger models are better but slower; on CPU the 8B model can take several minutes to summarise a long meeting, and whisper medium runs at roughly two to three times real time on a fast desktop CPU (a 90-minute call would take hours), which is why the CPU tiers default to small.

## Using your own model server

If you already run Ollama, LM Studio, or anything with an OpenAI-compatible API, point Bazza at it: Settings > AI assistant > "On an endpoint I run". Transcription still runs in Bazza; only summaries, cleanup and chat go to your endpoint.

## Where models live

`<data folder>/models`. Delete files there (or from Settings) to free space; Bazza re-downloads on demand.

## Adding models

`crates/bazza-models/catalog.json` lists every model with its URL, size and SHA-256. Pull requests adding models are welcome: whisper.cpp `ggml-*.bin` files for speech and GGUF files with a chat template for language models.
