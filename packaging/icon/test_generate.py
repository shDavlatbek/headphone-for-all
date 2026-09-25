#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for generate.py and the committed icon files.

Run with ``python3 -m unittest discover -s packaging/icon -p 'test_*.py'``.
The checks on committed rasters need Pillow; rendering is not exercised, so
neither cairosvg nor rsvg-convert is required.
"""

from __future__ import annotations

import json
import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import generate  # noqa: E402  (after the path tweak)

NS = {"svg": generate.SVG_NS}


def parse(svg: bytes) -> ET.Element:
    return ET.fromstring(svg)


def by_id(root: ET.Element, element_id: str) -> ET.Element | None:
    for element in root.iter():
        if element.get("id") == element_id:
            return element
    return None


class VariantTests(unittest.TestCase):
    def test_source_has_the_ids_the_variants_rely_on(self) -> None:
        root = parse(generate.SOURCE.read_bytes())
        self.assertIsNotNone(by_id(root, "tile"))
        self.assertIsNotNone(by_id(root, "glyph"))
        self.assertEqual(root.get("viewBox"), "0 0 256 256")

    def test_standard_variant_is_the_source_drawing(self) -> None:
        root = parse(generate.variant_standard())
        tile = by_id(root, "tile")
        assert tile is not None
        self.assertEqual((tile.get("x"), tile.get("width"), tile.get("rx")), ("8", "240", "56"))

    def test_full_bleed_variant_fills_the_canvas_without_rounding(self) -> None:
        root = parse(generate.variant_full_bleed())
        tile = by_id(root, "tile")
        assert tile is not None
        self.assertEqual(
            [tile.get(key) for key in ("x", "y", "width", "height", "rx", "ry")],
            ["0", "0", "256", "256", "0", "0"],
        )
        self.assertIsNotNone(by_id(root, "glyph"))

    def test_macos_variant_insets_the_tile_to_824_of_1024(self) -> None:
        root = parse(generate.variant_macos())
        groups = [child for child in root if child.tag == f"{{{generate.SVG_NS}}}g"]
        self.assertEqual(len(groups), 1, "everything drawable is wrapped in one scaled group")
        transform = groups[0].get("transform", "")
        scale = float(transform.split("scale(")[1].rstrip(")"))
        self.assertAlmostEqual(240 * scale / 256, 824 / 1024, places=4)
        # defs stay at the top level so url(#bg) still resolves.
        self.assertIsNotNone(root.find("svg:defs", NS))

    def test_adaptive_foreground_has_no_tile_and_fits_the_safe_zone(self) -> None:
        root = parse(generate.variant_adaptive_foreground())
        self.assertIsNone(by_id(root, "tile"))
        self.assertIsNotNone(by_id(root, "glyph"))
        transform = root.find("svg:g", NS)
        assert transform is not None
        scale = float(transform.get("transform", "").split("scale(")[1].rstrip(")"))
        # The glyph is ~196 units tall; the safe zone is 66/108 of the canvas.
        self.assertLessEqual(196 * scale, 256 * 66 / 108)


@unittest.skipIf(generate.Image is None, "Pillow is not installed")
class CommittedIconTests(unittest.TestCase):
    def test_windows_ico_has_every_size(self) -> None:
        with generate.Image.open(generate.WINDOWS_ICO) as image:
            sizes = sorted(size for size, _ in image.info["sizes"])
        self.assertEqual(sizes, sorted(generate.WINDOWS_SIZES))

    def test_linux_icons_have_their_nominal_size(self) -> None:
        for size in generate.LINUX_SIZES:
            path = generate.LINUX_ICONS / f"{size}x{size}" / "apps" / f"{generate.APP_ID}.png"
            with generate.Image.open(path) as image:
                self.assertEqual(image.size, (size, size), path)
        scalable = generate.LINUX_ICONS / "scalable" / "apps" / f"{generate.APP_ID}.svg"
        self.assertEqual(scalable.read_bytes(), generate.SOURCE.read_bytes())

    def test_ios_icons_are_opaque_and_match_contents_json(self) -> None:
        folder = generate.OUT / "ios" / "AppIcon.appiconset"
        contents = json.loads((folder / "Contents.json").read_text())
        self.assertEqual(len(contents["images"]), len(generate.IOS_ICONS))
        for entry in contents["images"]:
            points = float(entry["size"].split("x")[0])
            scale = int(entry["scale"].rstrip("x"))
            with generate.Image.open(folder / entry["filename"]) as image:
                self.assertEqual(image.size, (round(points * scale),) * 2, entry["filename"])
                self.assertEqual(image.mode, "RGB", "App Store icons must not have alpha")

    def test_macos_icons_match_contents_json(self) -> None:
        folder = generate.OUT / "macos" / "AppIcon.appiconset"
        contents = json.loads((folder / "Contents.json").read_text())
        for entry in contents["images"]:
            points = int(entry["size"].split("x")[0])
            scale = int(entry["scale"].rstrip("x"))
            with generate.Image.open(folder / entry["filename"]) as image:
                self.assertEqual(image.size, (points * scale,) * 2, entry["filename"])

    def test_android_densities(self) -> None:
        base = generate.OUT / "android" / "res"
        for density, factor in generate.ANDROID_DENSITIES.items():
            with generate.Image.open(base / f"mipmap-{density}" / "ic_launcher.png") as image:
                self.assertEqual(image.size, (round(48 * factor),) * 2)
            with generate.Image.open(base / f"mipmap-{density}" / "ic_launcher_foreground.png") as image:
                self.assertEqual(image.size, (round(108 * factor),) * 2)
        self.assertTrue((base / "mipmap-anydpi-v26" / "ic_launcher.xml").is_file())


if __name__ == "__main__":
    unittest.main()
