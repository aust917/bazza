# Maintaining this fork

This repository is a development fork of [Dallenlol/hark](https://github.com/Dallenlol/hark),
starting from upstream commit `ecf7e0c` (0.4.0). The initial fork adds navigation,
agent instructions, documentation checks and repository hygiene. Runtime identity,
updates and storage behavior are inherited; an independent distribution is not yet configured.

## Remotes and upstream changes

In the initial local checkout, `origin` points to `aust917/hark`, `upstream` points
to `Dallenlol/hark`, and `remote.pushDefault` is `origin`. A fresh clone has only
`origin`; add upstream once if needed:

```powershell
git remote add upstream https://github.com/Dallenlol/hark.git
git config remote.pushDefault origin
```

Check `git remote -v` before pushing. Feature branches and PRs belong in the fork
unless contributing upstream is the task. Keep the MIT license and original attribution.

For an upstream update, start with a clean working tree and integrate on a branch:

```powershell
git fetch upstream
git switch -c chore/sync-upstream
git merge upstream/main
```

Resolve conflicts, check changed manifests/lockfiles and native build configuration,
then run checks for the affected areas. Preserve the agent entry points and update
their routes when upstream moves files. Review the integration through the fork's PR flow.
Do not treat historical unchecked plan items as outstanding upstream work.

## Before distributing an independent build

Choose whether the fork replaces Hark or coexists with it. That decision affects
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
| Pages deployment | `.github/workflows/pages.yml` | Configure Pages, then set repository variable `HARK_ENABLE_PAGES=true` to enable deployment |

The current updater endpoint is
`https://github.com/Dallenlol/hark/releases/latest/download/latest.json` and its public
key belongs to upstream. Do not use a fabricated key or assume upstream signatures
will verify the fork's releases.

The default data directory is `%APPDATA%\Hark` on Windows and
`~/Library/Application Support/Hark` on macOS. `data-dir.txt` can redirect it to another
folder. This is controlled by `hark-store`, independently of the Tauri identifier;
changing only the identifier does not isolate an installed original's library.
Use disposable data when validating migrations or recovery.

The release workflow runs on `v*` tags or manual dispatch. Tags can create a draft
release and upload artifacts; manual dispatch builds artifacts. Updater signatures
require `TAURI_SIGNING_PRIVATE_KEY`. macOS signing/notarization needs separate setup.
The website workflow builds changes under `site/` on `main`; deployment runs only
when repository variable `HARK_ENABLE_PAGES` is `true`. Leave it unset until the
fork's URLs and Pages destination are configured.

Use [testing.md](testing.md) for native validation and report the tested OS, backend,
sidecars and model files. The initial documentation work does not establish that an
independently branded release is ready.
