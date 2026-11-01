# Local development and upstream updates

This is a local development copy of [Dallenlol/hark](https://github.com/Dallenlol/hark),
starting from upstream commit `ecf7e0c` (0.4.0). Local changes add navigation,
agent instructions, documentation checks and repository hygiene. Source changes and
development documentation stay on this computer. Preserve the MIT license and
original attribution.

Do not create remote repositories/forks, push code, create PRs, or publish/deploy
this fork unless the user explicitly changes the local-only requirement. Local
Git commits and fetching upstream changes are allowed. Independent distribution
is optional future work, not an authorized task.

## Read-only upstream

`upstream` is the read-only source at `https://github.com/Dallenlol/hark.git`.
Development uses local branches and commits; it needs no publishing remote or PR
flow. Inspect `git status` before integrating upstream changes and preserve local
work.

For an upstream update, start with a clean working tree and integrate locally:

```powershell
git fetch upstream
git switch -c chore/sync-upstream
git merge upstream/main
```

Resolve conflicts, check changed manifests/lockfiles and native build configuration,
then run checks for the affected areas. Preserve the agent entry points and update
their routes when upstream moves files. Review the local diff and verification results.
Do not treat historical unchecked plan items as outstanding upstream work.

## Optional future distribution

These notes document inherited configuration, not permission to publish. If the
user later authorizes distribution, choose whether the fork replaces Hark or
coexists with it. That decision affects
installer identity, storage migration and update compatibility; changing a repository
URL alone is insufficient.

| Concern | Current inherited location | Required release decision |
| --- | --- | --- |
| Update channel and verification key | `src-tauri/tauri.conf.json`, `app/src/lib/updates.ts` | Configure the fork's signed channel/key, or deliberately disable updates |
| Application/installer identity | `src-tauri/tauri.conf.json` (`app.hark.desktop`) | Choose replacement or separate identity; test install/uninstall behavior |
| User library | `crates/hark-store/src/db.rs` | Default remains `Hark`; choose compatibility, isolation or an explicit migration |
| Asset access to recordings | `src-tauri/tauri.conf.json`, `src-tauri/capabilities/` | Keep media access aligned with the chosen storage paths without broadening scope |
| Signing and release variants | `.github/workflows/release.yml`, `src-tauri/src/lib.rs` | Own signing secrets and verify CPU/CUDA/Metal update platform keys |
| Project and download links | `site/src/site.ts`, `site/astro.config.mjs`, README, install docs, Cargo metadata | Point distribution links at the chosen project; retain attribution |
| Pages deployment | `.github/workflows/pages.yml` | Requires an explicitly authorized publication destination and `HARK_ENABLE_PAGES=true` |

The current updater endpoint is
`https://github.com/Dallenlol/hark/releases/latest/download/latest.json` and its public
key belongs to upstream. Do not use a fabricated key or assume upstream signatures
will verify the fork's releases.

The default data directory is `%APPDATA%\Hark` on Windows and
`~/Library/Application Support/Hark` on macOS. `data-dir.txt` can redirect it to another
folder. This is controlled by `hark-store`, independently of the Tauri identifier;
changing only the identifier does not isolate an installed original's library.
Use disposable data when validating migrations or recovery.

The inherited release workflow runs on `v*` tags or manual dispatch. Tags can create a draft
release and upload artifacts; manual dispatch builds artifacts. Updater signatures
require `TAURI_SIGNING_PRIVATE_KEY`. macOS signing/notarization needs separate setup.
The website workflow builds changes under `site/` on `main`; deployment runs only
when repository variable `HARK_ENABLE_PAGES` is `true`. Do not trigger remote
workflows or enable publication under the current local-only requirement.

Use [testing.md](testing.md) for native validation and report the tested OS, backend,
sidecars and model files. The initial documentation work does not establish that an
independently branded release is ready.
