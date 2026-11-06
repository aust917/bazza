#!/usr/bin/env python3
"""Rasterize the Bazza icon from assets/logo.svg using Tauri's SVG renderer.

    python scripts/make-icons.py            # refresh the 1024 PNGs and size preview
    python scripts/make-icons.py --tauri    # also regenerate Windows and macOS icons

Requires Pillow and the project's installed Tauri CLI.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "logo.svg"
TAURI_CLI = ROOT / "node_modules" / "@tauri-apps" / "cli" / "tauri.js"
MASTER_SIZE = 1024


def generate_icons(output: Path, *options: str) -> None:
    subprocess.run(
        ["node", str(TAURI_CLI), "icon", str(SOURCE), "--output", str(output), *options],
        cwd=ROOT,
        check=True,
    )


def write_size_preview(icon: Image.Image) -> None:
    # A contact sheet at the sizes that matter for legibility.
    sizes = [16, 24, 32, 48, 64, 128, 256]
    sheet = Image.new("RGBA", (sum(sizes) + 16 * len(sizes), 256 + 32), (245, 243, 239, 255))
    x = 8
    for size in sizes:
        sheet.alpha_composite(icon.resize((size, size), Image.LANCZOS), (x, 16 + (256 - size) // 2))
        x += size + 16
    preview = ROOT / "docs" / "img" / "icon-sizes.png"
    preview.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(preview)
    print(f"wrote {preview.relative_to(ROOT)}")


def main() -> int:
    with TemporaryDirectory(prefix="bazza-icons-") as temp:
        work = Path(temp)
        generate_icons(work, "--png", str(MASTER_SIZE))
        master = work / f"{MASTER_SIZE}x{MASTER_SIZE}.png"
        for output in (ROOT / "assets" / "icon.png", ROOT / "app-icon.png"):
            shutil.copyfile(master, output)
            print(f"wrote {output.relative_to(ROOT)} ({MASTER_SIZE}x{MASTER_SIZE})")
        with Image.open(master) as icon:
            write_size_preview(icon.convert("RGBA"))

        if "--tauri" in sys.argv:
            desktop = ROOT / "src-tauri" / "icons"
            generated = work / "platform"
            generated.mkdir()
            generate_icons(generated)
            # Mobile icon sets are outside Bazza's supported platforms.
            desktop.mkdir(parents=True, exist_ok=True)
            for icon in generated.iterdir():
                if icon.is_file() and icon.suffix in {".png", ".ico", ".icns"}:
                    shutil.copyfile(icon, desktop / icon.name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
