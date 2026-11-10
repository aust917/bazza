"""Verify the managed provisioner proposal without running a Docker daemon."""

import argparse
import ast
from pathlib import Path
import shutil
import subprocess
import tempfile

from patch import HASHES, build_overlay
from render_provision_patch import NAMES, render


def check(provisioner, pinned, temp_root, bash):
    original = provisioner.read_text(encoding="utf-8")
    proposed = render(original, pinned)
    stage = proposed.split('# Mount the verified WSS startup change', 1)[1].split(
        '\nif [[ ! -s credentials.env ]]; then', 1)[0]
    stage = '# Mount the verified WSS startup change' + stage
    embedded = stage.split("<<'BAZZA_WORKER_WSS_PATCH_PY'\n", 1)[1].split('\nBAZZA_WORKER_WSS_PATCH_PY', 1)[0]
    ast.parse(embedded)
    reference = {Path(name).name: content.encode('utf-8') for name, content in build_overlay({
        name: (pinned / name).read_text(encoding='utf-8') for name in HASHES
    }).items()}
    with tempfile.TemporaryDirectory(prefix='wss-provision-check-', dir=temp_root) as directory:
        root = Path(directory).resolve()
        if not root.is_relative_to(temp_root.resolve()):
            raise ValueError('Temporary fixture escaped its directory')
        (root / 'provision.sh').write_text(proposed, encoding='utf-8', newline='\n')
        (root / 'stage.sh').write_text(stage, encoding='utf-8', newline='\n')
        subprocess.run([bash, '-n', (root / 'provision.sh').as_posix()], check=True)

        fixtures = root / 'fixtures'
        fixtures.mkdir()
        for name in HASHES:
            destination = fixtures / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(pinned / name, destination)
        output = root / 'expected'
        output.mkdir()
        local = embedded.replace('Path("/attendee")', f'Path({fixtures.as_posix()!r})').replace(
            'Path("/bazza-worker-patch")', f'Path({output.as_posix()!r})')
        exec(compile(local, '<provisioner worker overlay>', 'exec'), {})
        assert {path.name: path.read_bytes() for path in output.iterdir()} == reference

        bad_output = root / 'bad-output'
        bad_output.mkdir()
        with (fixtures / next(iter(HASHES))).open('ab') as drift:
            drift.write(b'\n# fixture drift\n')
        try:
            exec(compile(local.replace(f'Path({output.as_posix()!r})', f'Path({bad_output.as_posix()!r})'),
                         '<provisioner worker overlay with drift>', 'exec'), {})
        except ValueError:
            assert not list(bad_output.iterdir())
        else:
            raise AssertionError('Embedded worker patch accepted drifted input')

        shell = '''set -Eeuo pipefail
cd -- "$1"
home_dir=$PWD
revision=$2
image=fixture-no-image
worker_stages=0
docker() {
    [[ $1 == run && "$*" == *'--pull never --network none --read-only'* ]]
    cat >/dev/null
    worker_stages=$((worker_stages + 1))
    cp -- expected/*.py "$worker_patch_stage/"
}
source ./stage.sh
[[ $worker_stages == 1 && $worker_patch_refreshed == 1 && $worker_patch_repaired == 0 ]]
source ./stage.sh
[[ $worker_stages == 1 && $worker_patch_refreshed == 0 && $worker_patch_repaired == 0 ]]
printf 'drift' >> "$worker_patch_dir/bot_websocket_client.py"
source ./stage.sh
[[ $worker_stages == 2 && $worker_patch_refreshed == 1 && $worker_patch_repaired == 1 ]]
worker_patch_valid "$worker_patch_dir"
'''
        (root / 'verify-stage.sh').write_text(shell, encoding='utf-8', newline='\n')
        revision = next(line.removeprefix('revision=') for line in proposed.splitlines() if line.startswith('revision='))
        subprocess.run([bash, (root / 'verify-stage.sh').as_posix(), root.as_posix(), revision], check=True)

    base, worker = proposed.split('  worker:\n', 1)
    worker = worker.split('  postgres:\n', 1)[0]
    assert './container-config:/etc/bazza-attendee:ro' in worker and 'BAZZA_WSS_PATCH_DIR' not in base
    for name in NAMES:
        assert f'target: /attendee/bots/bot_controller/{name}\n        read_only: true\n' in worker
    assert worker.count('create_host_path: false') == len(NAMES)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('provisioner', type=Path)
    parser.add_argument('pinned_source', type=Path)
    parser.add_argument('temporary_fixture_root', type=Path)
    parser.add_argument('--bash', default='bash')
    args = parser.parse_args()
    check(args.provisioner, args.pinned_source, args.temporary_fixture_root, args.bash)
    print('Verified Bash syntax, exact patched files, pin rejection, cold/warm/repair staging, and worker-only mounts.')


if __name__ == '__main__':
    main()
