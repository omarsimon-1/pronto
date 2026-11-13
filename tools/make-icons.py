#!/usr/bin/env python3
"""Pronto app icons from the approved pixel-grid SVG.

    python3 tools/make-icons.py packaging/icons

Reads docs/brand/pronto-icon.svg and renders
hicolor/{16,24,32,48,64,128,256,512}x…/apps/<id>.png with the SVG's crispEdges setting.
Copies the unchanged master to hicolor/scalable/apps/<id>.svg for docks, launchers,
and the app's themed icon. Needs librsvg's `rsvg-convert` command.
"""

import pathlib
import shutil
import subprocess
import sys

APP_ID = "io.github.omarsimon1.Pronto"
SIZES = (16, 24, 32, 48, 64, 128, 256, 512)
MASTER = pathlib.Path(__file__).resolve().parents[1] / "docs" / "brand" / "pronto-icon.svg"


def main():
    out = pathlib.Path(sys.argv[1])
    master = MASTER
    if not master.is_file():
        sys.exit(f"{master} is missing")
    for old in out.glob(f"hicolor/*x*/apps/{APP_ID}.svg"):
        old.unlink()
        print(f"removed {old}")
    for size in SIZES:
        path = out / "hicolor" / f"{size}x{size}" / "apps" / f"{APP_ID}.png"
        path.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["rsvg-convert", "--width", str(size), "--height", str(size),
             "--output", str(path), str(master)],
            check=True,
        )
        print(path)
    scalable = out / "hicolor" / "scalable" / "apps" / f"{APP_ID}.svg"
    scalable.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(master, scalable)
    print(scalable)
    for empty in sorted(out.glob("hicolor/*/apps"), reverse=True):
        if not any(empty.iterdir()):
            empty.rmdir()
            empty.parent.rmdir()


if __name__ == "__main__":
    main()
