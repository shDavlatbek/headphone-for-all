#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Render every raster app icon from ``packaging/icon/hfa.svg``.

Outputs (paths relative to the repository root):

* ``app/windows/runner/resources/app_icon.ico``: multi-size Windows icon
  (16 to 256 px, the sizes Explorer, the taskbar and Alt+Tab ask for).
* ``app/linux/icons/hicolor/<N>x<N>/apps/io.github.shdavlatbek.hfa.png`` and
  ``.../scalable/apps/io.github.shdavlatbek.hfa.svg``: freedesktop icon theme
  layout; the Linux runner and the AppImage / Flatpak packaging use it.
* ``packaging/icon/out/``: icon sets for the other platforms, ready for their
  work packages to adopt (drop-in file names of the Flutter templates):
  ``android/`` (legacy mipmaps + adaptive icon foreground / background),
  ``ios/AppIcon.appiconset`` (full-bleed, opaque), ``macos/AppIcon.appiconset``
  (tile inset like other macOS icons) and ``png/`` (store / web sizes).

Rendering uses ``cairosvg`` when it is importable, else the ``rsvg-convert``
command (librsvg). ``Pillow`` assembles the ``.ico``. Install with
``pip install cairosvg Pillow`` or ``apt-get install librsvg2-bin python3-pil``.

Usage: ``python3 packaging/icon/generate.py [--only windows,linux,other]``.
"""

from __future__ import annotations

import argparse
import copy
import io
import json
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

try:
    from PIL import Image
except ImportError:  # pragma: no cover - reported in main()
    Image = None  # type: ignore[assignment]

APP_ID = "io.github.shdavlatbek.hfa"
SVG_NS = "http://www.w3.org/2000/svg"
ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "packaging" / "icon" / "hfa.svg"
OUT = ROOT / "packaging" / "icon" / "out"

WINDOWS_ICO = ROOT / "app" / "windows" / "runner" / "resources" / "app_icon.ico"
WINDOWS_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]

LINUX_ICONS = ROOT / "app" / "linux" / "icons" / "hicolor"
LINUX_SIZES = [16, 24, 32, 48, 64, 128, 256, 512]

# Android density buckets: legacy launcher icon (48 dp) and adaptive icon
# layers (108 dp).
ANDROID_DENSITIES = {"mdpi": 1.0, "hdpi": 1.5, "xhdpi": 2.0, "xxhdpi": 3.0, "xxxhdpi": 4.0}
ANDROID_BACKGROUND = "#3949AB"

# Flutter's iOS template (Icon-App-<size>@<scale>x.png) and its Contents.json.
IOS_ICONS = [
    ("iphone", "20x20", 2), ("iphone", "20x20", 3),
    ("iphone", "29x29", 1), ("iphone", "29x29", 2), ("iphone", "29x29", 3),
    ("iphone", "40x40", 2), ("iphone", "40x40", 3),
    ("iphone", "60x60", 2), ("iphone", "60x60", 3),
    ("ipad", "20x20", 1), ("ipad", "20x20", 2),
    ("ipad", "29x29", 1), ("ipad", "29x29", 2),
    ("ipad", "40x40", 1), ("ipad", "40x40", 2),
    ("ipad", "76x76", 1), ("ipad", "76x76", 2),
    ("ipad", "83.5x83.5", 2),
    ("ios-marketing", "1024x1024", 1),
]

# Flutter's macOS template (app_icon_<px>.png) and its Contents.json.
MACOS_ICONS = [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)]
# Big Sur style: the rounded tile covers 824 of 1024 px, centred.
MACOS_TILE_FRACTION = 824 / 1024

STORE_SIZES = [512, 1024]


def _load_svg() -> ET.ElementTree:
    ET.register_namespace("", SVG_NS)
    return ET.parse(SOURCE)


def _find(tree: ET.ElementTree, element_id: str) -> ET.Element:
    for element in tree.iter():
        if element.get("id") == element_id:
            return element
    raise SystemExit(f"{SOURCE}: no element with id '{element_id}'")


def _serialize(tree: ET.ElementTree) -> bytes:
    buffer = io.BytesIO()
    tree.write(buffer, encoding="utf-8", xml_declaration=True)
    return buffer.getvalue()


def variant_standard() -> bytes:
    """The icon as drawn: rounded tile with a small margin."""
    return _serialize(_load_svg())


def variant_full_bleed() -> bytes:
    """Opaque square tile without rounding (iOS masks the corners itself)."""
    tree = _load_svg()
    tile = _find(tree, "tile")
    for key, value in {"x": "0", "y": "0", "width": "256", "height": "256", "rx": "0", "ry": "0"}.items():
        tile.set(key, value)
    return _serialize(tree)


def _wrap_scaled(tree: ET.ElementTree, scale: float) -> None:
    """Scale every drawable child of the root around the canvas centre."""
    root = tree.getroot()
    group = ET.Element(f"{{{SVG_NS}}}g")
    offset = 128 * (1 - scale)
    group.set("transform", f"translate({offset:.3f} {offset:.3f}) scale({scale:.5f})")
    for child in list(root):
        tag = child.tag.split("}")[-1]
        if tag in ("title", "defs"):
            continue
        root.remove(child)
        group.append(child)
    root.append(group)


def variant_macos() -> bytes:
    """The tile inset to macOS proportions (824 of 1024 px)."""
    tree = _load_svg()
    tile = _find(tree, "tile")
    tile_fraction = float(tile.get("width", "240")) / 256
    _wrap_scaled(tree, MACOS_TILE_FRACTION / tile_fraction)
    return _serialize(tree)


def variant_adaptive_foreground() -> bytes:
    """The glyph alone on a transparent canvas, inside the adaptive icon safe
    zone (a 66 dp circle of the 108 dp layer)."""
    tree = _load_svg()
    tree.getroot().remove(_find(tree, "tile"))
    # The glyph spans about 180 x 196 units; 66/108 of 256 is ~156 units.
    _wrap_scaled(tree, 0.76)
    return _serialize(tree)


def render(svg: bytes, size: int) -> bytes:
    """Render ``svg`` to a ``size`` x ``size`` PNG."""
    try:
        import cairosvg  # type: ignore[import-not-found]

        return cairosvg.svg2png(bytestring=svg, output_width=size, output_height=size)
    except ImportError:
        pass
    tool = shutil.which("rsvg-convert")
    if tool is None:
        raise SystemExit("need cairosvg (pip install cairosvg) or rsvg-convert (librsvg2-bin)")
    result = subprocess.run(
        [tool, "--width", str(size), "--height", str(size), "--format", "png"],
        input=svg,
        capture_output=True,
        check=True,
    )
    return result.stdout


def _image(png: bytes) -> "Image.Image":
    image = Image.open(io.BytesIO(png))
    image.load()
    return image.convert("RGBA")


def _write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    print(f"wrote {path.relative_to(ROOT)}")


def _write_png(path: Path, image: "Image.Image") -> None:
    buffer = io.BytesIO()
    image.save(buffer, format="PNG", optimize=True)
    _write(path, buffer.getvalue())


def _write_json(path: Path, value: object) -> None:
    _write(path, (json.dumps(value, indent=2) + "\n").encode("utf-8"))


def generate_windows() -> None:
    svg = variant_standard()
    images = [_image(render(svg, size)) for size in WINDOWS_SIZES]
    buffer = io.BytesIO()
    largest = images[-1]
    largest.save(
        buffer,
        format="ICO",
        sizes=[(size, size) for size in WINDOWS_SIZES],
        append_images=images[:-1],
    )
    _write(WINDOWS_ICO, buffer.getvalue())


def generate_linux() -> None:
    svg = variant_standard()
    for size in LINUX_SIZES:
        _write_png(LINUX_ICONS / f"{size}x{size}" / "apps" / f"{APP_ID}.png", _image(render(svg, size)))
    _write(LINUX_ICONS / "scalable" / "apps" / f"{APP_ID}.svg", SOURCE.read_bytes())


def generate_android() -> None:
    standard = variant_standard()
    foreground = variant_adaptive_foreground()
    base = OUT / "android" / "res"
    for density, factor in ANDROID_DENSITIES.items():
        legacy = round(48 * factor)
        layer = round(108 * factor)
        _write_png(base / f"mipmap-{density}" / "ic_launcher.png", _image(render(standard, legacy)))
        _write_png(base / f"mipmap-{density}" / "ic_launcher_foreground.png", _image(render(foreground, layer)))
    adaptive = (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
        '    <background android:drawable="@color/ic_launcher_background" />\n'
        '    <foreground android:drawable="@mipmap/ic_launcher_foreground" />\n'
        '    <monochrome android:drawable="@mipmap/ic_launcher_foreground" />\n'
        "</adaptive-icon>\n"
    )
    _write(base / "mipmap-anydpi-v26" / "ic_launcher.xml", adaptive.encode("utf-8"))
    colors = (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<resources>\n"
        f'    <color name="ic_launcher_background">{ANDROID_BACKGROUND}</color>\n'
        "</resources>\n"
    )
    _write(base / "values" / "ic_launcher_background.xml", colors.encode("utf-8"))


def generate_ios() -> None:
    svg = variant_full_bleed()
    folder = OUT / "ios" / "AppIcon.appiconset"
    images = []
    written: set[str] = set()
    for idiom, size, scale in IOS_ICONS:
        label = size.split("x")[0]
        pixels = round(float(label) * scale)
        filename = f"Icon-App-{label}x{label}@{scale}x.png"
        if filename not in written:
            # App Store icons must be opaque: drop the alpha channel.
            _write_png(folder / filename, _image(render(svg, pixels)).convert("RGB"))
            written.add(filename)
        images.append({"size": size, "idiom": idiom, "filename": filename, "scale": f"{scale}x"})
    _write_json(folder / "Contents.json", {"images": images, "info": {"version": 1, "author": "xcode"}})


def generate_macos() -> None:
    svg = variant_macos()
    folder = OUT / "macos" / "AppIcon.appiconset"
    images = []
    written: set[int] = set()
    for points, scale in MACOS_ICONS:
        pixels = points * scale
        filename = f"app_icon_{pixels}.png"
        if pixels not in written:
            _write_png(folder / filename, _image(render(svg, pixels)))
            written.add(pixels)
        images.append({"size": f"{points}x{points}", "idiom": "mac", "filename": filename, "scale": f"{scale}x"})
    _write_json(folder / "Contents.json", {"images": images, "info": {"version": 1, "author": "xcode"}})


def generate_store() -> None:
    svg = variant_standard()
    for size in STORE_SIZES:
        _write_png(OUT / "png" / f"hfa-{size}.png", _image(render(svg, size)))


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--only",
        default="windows,linux,other",
        help="comma-separated subset of: windows, linux, other (default: all)",
    )
    args = parser.parse_args(argv)
    if Image is None:
        print("Pillow is required: pip install Pillow", file=sys.stderr)
        return 1
    wanted = {part.strip() for part in args.only.split(",") if part.strip()}
    unknown = wanted - {"windows", "linux", "other"}
    if unknown:
        parser.error(f"unknown target(s): {', '.join(sorted(unknown))}")
    if "windows" in wanted:
        generate_windows()
    if "linux" in wanted:
        generate_linux()
    if "other" in wanted:
        generate_android()
        generate_ios()
        generate_macos()
        generate_store()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
