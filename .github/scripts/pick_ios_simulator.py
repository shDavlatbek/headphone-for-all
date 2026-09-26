#!/usr/bin/env python3
"""Pick an iPhone simulator that the selected Xcode can run tests on.

Usage: pick_ios_simulator.py <iphonesimulator-sdk-version> <runtimes.json> <devices.json>

The JSON files are the output of `xcrun simctl list runtimes available --json` and
`xcrun simctl list devices available --json`. CoreSimulator lists the runtimes of every installed
Xcode, so a runner image can offer iOS runtimes newer than the active SDK supports; `xcodebuild
test` cannot use those. This script keeps the iOS runtimes whose major.minor version is <= the
SDK's (compared as integer tuples, not as strings), takes the newest of them that has an available
iPhone, and prints that iPhone's UDID. It prints nothing (and exits 0) when there is none.
Used by .github/workflows/flutter.yml (job apple-unit-tests).
"""

from __future__ import annotations

import json
import sys


def version_key(version: str) -> tuple[int, int]:
    """Return (major, minor) of a dotted version string such as "18.5" or "26.0.1"."""
    parts: list[int] = []
    for piece in version.split(".")[:2]:
        digits = "".join(ch for ch in piece if ch.isdigit())
        parts.append(int(digits) if digits else 0)
    while len(parts) < 2:
        parts.append(0)
    return parts[0], parts[1]


def is_ios_runtime(runtime: dict) -> bool:
    """True for an available iOS runtime (not tvOS, watchOS, visionOS)."""
    if not runtime.get("isAvailable", True):
        return False
    platform = runtime.get("platform")
    if platform is not None:
        return platform == "iOS"
    return ".iOS-" in runtime.get("identifier", "")


def pick(sdk_version: str, runtimes: dict, devices: dict) -> str | None:
    """Return the UDID of an iPhone on the newest runtime the SDK supports, or None."""
    sdk = version_key(sdk_version)
    candidates = [
        rt
        for rt in runtimes.get("runtimes", [])
        if is_ios_runtime(rt) and version_key(rt.get("version", "0")) <= sdk
    ]
    candidates.sort(key=lambda rt: version_key(rt.get("version", "0")), reverse=True)
    by_runtime = devices.get("devices", {})
    for runtime in candidates:
        for device in by_runtime.get(runtime.get("identifier", ""), []):
            if device.get("isAvailable", True) and device.get("name", "").startswith("iPhone"):
                return device["udid"]
    return None


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[2], encoding="utf-8") as f:
        runtimes = json.load(f)
    with open(argv[3], encoding="utf-8") as f:
        devices = json.load(f)
    udid = pick(argv[1], runtimes, devices)
    if udid:
        print(udid)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
