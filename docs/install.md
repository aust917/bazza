# Installing Bazza

This local fork has no public installers. Build from this working copy using
the [development guide](dev/testing.md#desktop-development-and-native-build).
The original project's release downloads are not Bazza builds.

## Windows

For a locally built installer, choose the variant matching your hardware:

- **CUDA** installer if you have an NVIDIA GPU (GTX 10-series or newer, 6 GB+ VRAM, a recent driver). Everything it needs is inside the installer; no CUDA toolkit to install. Transcription and summaries run several times faster: an 88-minute call is fully processed in about 10 minutes.
- **CPU** installer otherwise. Works on any 64-bit Windows 10/11 machine with 8 GB RAM and a CPU from 2013 or later (AVX2). Bazza picks lighter speech models so long recordings still finish in reasonable time.

Run the `.exe` (per-user install, no admin needed). Windows SmartScreen may warn because the build is not code-signed yet; choose "More info > Run anyway". If you prefer, verify the SHA-256 listed on the release.

The Windows `.exe` installer opens in Russian.

On a fresh Windows installation, setup offers to connect Bazza to Codex and
separately to Claude Code and Claude Desktop. Accepting an offer gives each
selected client its own read grant for the new Bazza library. Restart any
client that was open during setup. Check `/mcp` in Codex or Claude Code, or
the local connectors list in Claude Desktop, for `bazza`.

Windows starts the local library service on demand under your account, even
when the Bazza window is closed. Use **Settings > Codex MCP** or
**Settings > Claude MCP** to manage access. Turning a client off revokes its
grant immediately, including for open sessions. Restart that client after
turning it back on. If Claude Desktop was installed after Bazza, turn its
access off and back on to pick up the new client's configuration path.

## macOS

Build the `.dmg` for **Apple Silicon** (M1 and later, uses Metal) or **Intel**. Drag Bazza to Applications.

The build is not notarized yet: on first launch right-click the app > Open, or run `xattr -dr com.apple.quarantine /Applications/Bazza.app`.

Permissions Bazza asks for, and why:

- **Microphone**: your side of the call.
- **Audio recording (system audio)**: the other participants. Requires macOS 14.6 or later; on older versions Bazza records mic only.
- **Screen recording**: only if you turn on screen video.

## First run

1. Check the mic and system audio meters move.
2. Bazza picks model sizes for your hardware; override if you like.
3. Download the models (one time, from Hugging Face, checksum-verified). Start with the essentials and record right away; the bigger models finish in the background and Bazza catches up on clean-up and summaries by itself.

## Recording

In a new Windows managed library, open the picker with **Record now**.
The regular desktop also opens the picker from the call popup, the hotkey
(`Ctrl/Cmd+Shift+R`) and the tray.

- **Screen.** Choose which full screen to record when several are connected. Turn off video to record audio only.
- **Audio.** The picker shows the microphone and the output used for system sound. Bazza records all sound on that output. Choose the output or turn off system sound in Settings.

While recording, open the meeting from the Library to watch captions. Stop
takes a few seconds to finish the last captions and video trailer. Bazza then
processes the transcript and speakers. The regular desktop also offers
running notes, summaries and Ask Bazza.

You can also **Import** an audio or video file you already have (Library > Import): it goes through the same pipeline.

## Data location

- Windows per-user `.exe` with the managed library: `%USERPROFILE%\Bazza Library`
- Windows legacy library: `%APPDATA%\Bazza`
- macOS: `~/Library/Application Support/Bazza`

In the regular desktop, move a library from Settings > Data > Change folder.
The desktop uses `bazza.db` and the Bazza folder's own `data-dir.txt` redirect.
Choose an empty destination to copy the library, then restart Bazza.
