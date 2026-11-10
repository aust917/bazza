"""Render a reviewable managed provisioner diff from the tested worker change."""

import argparse
import difflib
import hashlib
from pathlib import Path

from patch import HASHES, PIN, build_overlay


NAMES = tuple(Path(name).name for name in HASHES)
TARGET = "scripts/attendee-managed/provision.sh"


def replace_once(source, old, new):
    if source.count(old) != 1:
        raise ValueError("Managed provisioner layout has changed")
    return source.replace(old, new, 1)


def render(source, pinned):
    if f"revision={PIN}\n" not in source:
        raise ValueError("Managed provisioner does not use the tested Attendee revision")
    overlay = build_overlay({name: (pinned / name).read_text(encoding="utf-8") for name in HASHES})
    sums = "\n".join(f"{hashlib.sha256(body.encode()).hexdigest()}  {Path(name).name}"
                     for name, body in overlay.items())
    patcher = Path(__file__).with_name("patch.py").read_text(encoding="utf-8").split("\ndef main():\n")[0]
    patcher = patcher[patcher.index("import ast"):].replace("import sys\n", "")
    stage = '''# Mount the verified WSS startup change on the worker in Setup and Start.
worker_patch_dir=./worker-wss-startup-$revision-v1
worker_patch_refreshed=0
worker_patch_repaired=0
worker_patch_files=(BOT_FILES)
worker_patch_valid() (
    cd -- "$1" 2>/dev/null && sha256sum --check --status 2>/dev/null <<'WSS_HASHES'
OUTPUT_HASHES
WSS_HASHES
)
if ! worker_patch_valid "$worker_patch_dir"; then
    worker_patch_stage=$(mktemp -d "$home_dir/.worker-wss-startup.XXXXXX")
    cleanup_worker_patch() {
        local file
        for file in "${worker_patch_files[@]}"; do rm -f -- "$worker_patch_stage/$file"; done
        rmdir -- "$worker_patch_stage" 2>/dev/null || true
    }
    trap cleanup_worker_patch EXIT
    docker run --rm --pull never --network none --read-only --user 0:0 --entrypoint python \\
        --mount "type=bind,source=$worker_patch_stage,target=/bazza-worker-patch" \\
        -i "$image" - <<'BAZZA_WORKER_WSS_PATCH_PY'
PATCHER_CODE
upstream = Path("/attendee")
output = Path("/bazza-worker-patch")
files = build_overlay({name: (upstream / name).read_text(encoding="utf-8") for name in HASHES})
for name, contents in files.items():
    destination = output / Path(name).name
    with destination.open("xb") as stream:
        stream.write(contents.encode("utf-8"))
    destination.chmod(0o644)
BAZZA_WORKER_WSS_PATCH_PY
    worker_patch_valid "$worker_patch_stage"
    if [[ -d $worker_patch_dir ]]; then
        for file in "${worker_patch_files[@]}"; do
            mv -f -- "$worker_patch_stage/$file" "$worker_patch_dir/$file"
        done
        rmdir -- "$worker_patch_stage"
        worker_patch_repaired=1
    else
        chmod 755 "$worker_patch_stage"
        mv -T -- "$worker_patch_stage" "$worker_patch_dir"
    fi
    trap - EXIT
    worker_patch_refreshed=1
fi

'''.replace("BOT_FILES", " ".join(NAMES)).replace("OUTPUT_HASHES", sums).replace("PATCHER_CODE", patcher.rstrip())
    result = replace_once(source, 'fi\ndocker image inspect "$image" >/dev/null\n\n',
                          'fi\ndocker image inspect "$image" >/dev/null\n\n' + stage)

    mounts = '    volumes:\n      - ./container-config:/etc/bazza-attendee:ro\n'
    for name in NAMES:
        mounts += (f'      - type: bind\n        source: ${{BAZZA_WSS_PATCH_DIR:?}}/{name}\n'
                   f'        target: /attendee/bots/bot_controller/{name}\n'
                   '        read_only: true\n        bind:\n          create_host_path: false\n')
    result = replace_once(result, '    shm_size: 1gb\n  postgres:\n',
                          '    shm_size: 1gb\n' + mounts + '  postgres:\n')
    result = replace_once(result, "BAZZA_API_PORT=%s\\n' \\\n",
                          "BAZZA_API_PORT=%s\\nBAZZA_WSS_PATCH_DIR=%s\\n' \\\n")
    result = replace_once(result, '"$image" "$windows_host" "$api_port" > runtime.env.next',
                          '"$image" "$windows_host" "$api_port" "$worker_patch_dir" > runtime.env.next')
    result = replace_once(result, '''compose up -d api worker

# Check the caller can reach the Windows callback before claiming setup is
# complete. The Windows script temporarily holds this listener open.
if [[ $mode == Setup ]]; then
''', '''if (( worker_patch_repaired )); then
    compose up -d api
    compose up -d --no-deps --force-recreate worker
else
    compose up -d api worker
fi

if [[ $mode == Setup ]] || (( worker_patch_refreshed )); then
''')
    result = replace_once(result, '    [[ $attempt -lt 12 ]]\n    compose exec -T worker python -c', '''    [[ $attempt -lt 12 ]]
fi

# The Windows setup script temporarily holds the callback listener open.
if [[ $mode == Setup ]]; then
    compose exec -T worker python -c''')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provisioner", type=Path)
    parser.add_argument("pinned_source", type=Path)
    parser.add_argument("output_patch", type=Path)
    args = parser.parse_args()
    original = args.provisioner.read_text(encoding="utf-8")
    proposed = render(original, args.pinned_source)
    patch = f"diff --git a/{TARGET} b/{TARGET}\n" + "".join(difflib.unified_diff(
        original.splitlines(keepends=True), proposed.splitlines(keepends=True),
        fromfile=f"a/{TARGET}", tofile=f"b/{TARGET}",
    ))
    with args.output_patch.open("x", encoding="utf-8", newline="\n") as output:
        output.write(patch)


if __name__ == "__main__":
    main()
