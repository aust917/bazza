# Local development and upstream updates

**Bazza** is a local development fork of [Dallenlol/hark](https://github.com/Dallenlol/hark),
starting from upstream commit `ecf7e0c` (0.4.0). The [rename plan](plans/2026-11-02-bazza-rename.md)
records product naming and file-format decisions. Source changes and
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

These notes document local configuration, not permission to publish. Bazza uses
`app.bazza.desktop` and Bazza data paths. Installer coexistence and a production migration
still need platform acceptance before any future distribution.

| Concern | Current location | Required release decision |
| --- | --- | --- |
| Update channel and verification key | `src-tauri/tauri.conf.json`, `app/src/lib/updates.ts` | Disabled locally; future updates need an independent signed channel/key |
| Application/installer identity | `src-tauri/tauri.conf.json` (`app.bazza.desktop`) | Choose replacement or separate identity; test install/uninstall behavior |
| User library | `crates/bazza-store/src/db.rs` | Only `Bazza/bazza.db` and the Bazza root's own folder redirect are used |
| Asset access to recordings | `src-tauri/tauri.conf.json`, `src-tauri/capabilities/` | Keep media access aligned with the chosen storage paths without broadening scope |
| Signing and release variants | `.github/workflows/release.yml` | Installer signing and an independent update protocol require future setup |
| Project and download links | `site/src/site.ts`, `site/astro.config.mjs`, README, install docs, Cargo metadata | Point distribution links at the chosen project; retain attribution |
| Pages deployment | `.github/workflows/pages.yml` | Requires an explicitly authorized publication destination and `BAZZA_ENABLE_PAGES=true` |

The upstream updater endpoint/key and desktop updater plugin have been removed.
`createUpdaterArtifacts` is false; the UI makes no update requests. Do not restore
the original project's channel or fabricate a Bazza signing key.

The default data directory is `%APPDATA%\Bazza` on Windows and
`~/Library/Application Support/Bazza` on macOS. `data-dir.txt` can redirect it to another
folder. Only this pointer is read; the database is always `bazza.db`.
Settings > Data > Change folder requires an empty,
non-nested destination and writes the active database as `bazza.db` using a
consistent SQLite snapshot including committed WAL data; the source remains
intact. This is controlled by `bazza-store` and
the desktop data command, independently of the Tauri identifier. Use disposable
data when validating migration or recovery.

Portable imports and exports require a `.bazza` filename and `app: "bazza"`
in the manifest. No alternate product directories, database names, import
markers or file extensions are discovered or converted automatically.

The release workflow runs on `v*` tags or manual dispatch and builds artifacts.
Draft release uploads additionally require `BAZZA_ENABLE_RELEASES=true`; no updater
manifest is produced. macOS signing/notarization needs separate setup.
The website workflow builds changes under `site/` on `main`; deployment runs only
when repository variable `BAZZA_ENABLE_PAGES` is `true`. Do not trigger remote
workflows or enable publication under the current local-only requirement.

Use [testing.md](testing.md) for native validation and report the tested OS, backend,
sidecars and model files. The initial documentation work does not establish that an
independently branded release is ready.
