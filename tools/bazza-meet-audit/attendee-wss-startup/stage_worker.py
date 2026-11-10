"""Stage a mount-ready, numeric-only baseline or WSS startup candidate worker."""

import argparse
import ast
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

import patch as startup


TOOL_ROOT = Path(__file__).resolve().parent.parent
BROWSER = "bots/google_meet_bot_adapter/google_meet_chromedriver_payload.js"
MOUNT_ROOT = "${BAZZA_AUDIT_ROOT:?}"
VARIANTS = ("baseline", "candidate")


def load_provider_patch():
    path = TOOL_ROOT / "attendee-wss" / "patch.py"
    spec = importlib.util.spec_from_file_location("attendee_numeric_patch", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if module.PIN != startup.PIN:
        raise ValueError("Provider audit and WSS startup target different revisions")
    return module


PROVIDER_AUDIT = load_provider_patch()
SOURCE_FILES = frozenset(PROVIDER_AUDIT.HASHES) | frozenset(startup.HASHES)


def build_provider(sources, variant):
    if variant not in VARIANTS:
        raise ValueError(f"Invalid worker variant: {variant}")
    pinned = startup.check_sources(sources)
    provider = PROVIDER_AUDIT.build_overlay(sources)
    provider[startup.MANAGER] = pinned[startup.MANAGER]
    if variant == "candidate":
        provider[startup.CONTROLLER] = startup.patch_controller(provider[startup.CONTROLLER])
        provider[startup.MANAGER] = startup.patch_manager(provider[startup.MANAGER])
        provider[startup.CLIENT] = startup.patch_client(provider[startup.CLIENT], numeric_audit=True)
    for name, source in provider.items():
        ast.parse(source, filename=name)
    return provider


def build_browser(source):
    patcher = TOOL_ROOT / "attendee-browser" / "patch.mjs"
    browser = subprocess.run(
        [
            "node", "--input-type=module", "--eval",
            "import { readFileSync } from 'node:fs';\n"
            "const { instrumentBrowser, revision } = await import(process.argv[1]);\n"
            "if (revision !== process.argv[3]) throw new Error('Browser audit revision mismatch');\n"
            "process.stdout.write(instrumentBrowser(readFileSync(process.argv[2], 'utf8')));\n",
            patcher.as_uri(), str(source / BROWSER), startup.PIN,
        ],
        check=True, capture_output=True,
    ).stdout
    subprocess.run(["node", "--check", "-"], input=browser, check=True, capture_output=True)
    return browser


def compose_overlay(provider_paths):
    mounts = {"browser-audit.js": BROWSER}
    mounts.update({f"provider-overlay/{name}": name for name in sorted(provider_paths)})
    lines = [
        "services:",
        "  worker:",
        "    environment:",
        "      BAZZA_MEET_AUDIT: '1'",
        "    volumes:",
    ]
    for source, target in mounts.items():
        lines.extend(
            [
                "      - type: bind",
                f"        source: {MOUNT_ROOT}/{source}",
                f"        target: /attendee/{target}",
                "        read_only: true",
                "        bind:",
                "          create_host_path: false",
            ]
        )
    return "\n".join(lines) + "\n"


def stage_worker(source, output, variant):
    source, output = source.resolve(), output.resolve()
    if output.is_relative_to(source) or output.exists():
        raise ValueError("The output directory must be new and outside the upstream source")
    sources = {name: (source / name).read_text(encoding="utf-8") for name in sorted(SOURCE_FILES)}
    provider = build_provider(sources, variant)
    browser = build_browser(source)
    files = {
        "browser-audit.js": browser,
        "worker.compose.yaml": compose_overlay(provider).encode("utf-8"),
        "variant.json": (json.dumps({"variant": variant, "attendee_revision": startup.PIN}) + "\n").encode("utf-8"),
        **{f"provider-overlay/{name}": text.encode("utf-8") for name, text in provider.items()},
    }
    output.mkdir()
    for name, data in files.items():
        target = output / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("variant", choices=VARIANTS)
    parser.add_argument("source", type=Path, help="unmodified pinned Attendee source directory")
    parser.add_argument("output", type=Path, help="new directory for worker bind mounts")
    args = parser.parse_args()
    try:
        stage_worker(args.source, args.output, args.variant)
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode("utf-8", errors="replace").strip() if error.stderr else ""
        print(f"Could not verify or stage browser payload: {detail}", file=sys.stderr)
        return 1
    except (OSError, ValueError, SyntaxError) as error:
        print(f"Could not stage worker overlay: {error}", file=sys.stderr)
        return 1
    print(f"Staged {args.variant} worker overlay for Attendee {startup.PIN}: {args.output / 'worker.compose.yaml'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
