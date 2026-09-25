#!/usr/bin/env python3
"""Writes app/assets/licenses/rust.txt: the licences of everything linked into
the Rust core (hfa-ffi and its normal dependencies on every target, libopus
included), which the app shows on its licence page (lib/src/licenses.dart).

Run it after changing core/Cargo.lock:
    python3 app/tool/gen_rust_licenses.py

Needs cargo and the crates in the local registry (`cargo fetch`). Crates that
ship no licence file get the standard text of the first licence of their SPDX
expression, with their authors as the copyright holders.

Format (parsed by lib/src/licenses.dart): entries separated by a line of 80
'-'; each entry is one line of comma-separated package names, an empty line
and the licence text.
"""

import hashlib
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
APP = os.path.dirname(HERE)
ROOT = os.path.dirname(APP)
CORE = os.path.join(ROOT, "core")
OUT = os.path.join(APP, "assets", "licenses", "rust.txt")
SEPARATOR = "-" * 80
LICENSE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "UNLICENSE")

MIT_TEMPLATE = """MIT License

Copyright (c) {holders}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=CORE,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def linked_packages(meta):
    """hfa-ffi and its normal (not build or dev) dependencies, all targets."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == "hfa-ffi")
    seen, stack = set(), [root]
    while stack:
        node = stack.pop()
        if node in seen:
            continue
        seen.add(node)
        for dep in nodes[node]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    # Workspace crates are this project's own (LICENSE-MIT / LICENSE-APACHE).
    return [packages[i] for i in seen if packages[i]["source"]]


def licence_texts(package):
    """(package name, text) pairs for [package]."""
    directory = os.path.dirname(package["manifest_path"])
    name_of = package["name"]
    texts = []
    for name in sorted(os.listdir(directory)):
        path = os.path.join(directory, name)
        if name.upper().startswith(LICENSE_PREFIXES) and os.path.isfile(path):
            with open(path, encoding="utf-8", errors="replace") as f:
                texts.append((name_of, f.read().strip()))
    if name_of == "opusic-sys":
        # libopus itself (BSD-3-Clause), built from the bundled source.
        with open(os.path.join(directory, "opus", "COPYING"), encoding="utf-8") as f:
            texts.append(("libopus", f.read().strip()))
    if texts:
        return texts
    expression = package.get("license") or ""
    first = re.split(r"\s+OR\s+|\s+AND\s+|/", expression.strip("() "))[0].strip("() ")
    holders = ", ".join(a.split(" <")[0] for a in package.get("authors") or []) or (
        f"the {package['name']} authors"
    )
    if first == "MIT":
        return [(name_of, MIT_TEMPLATE.format(holders=holders).strip())]
    if first == "Apache-2.0":
        with open(os.path.join(ROOT, "LICENSE-APACHE"), encoding="utf-8") as f:
            return [(name_of, f"Copyright {holders}\n\n" + f.read().strip())]
    if first == "Zlib":
        return [
            (
                name_of,
                f"Copyright (c) {holders}\n\nLicensed under the zlib license (SPDX: Zlib).\n"
                "This software is provided 'as-is', without any express or implied warranty.",
            )
        ]
    sys.exit(f"{package['name']}: no licence file and no template for {expression!r}")


def main():
    by_text = {}
    for package in sorted(linked_packages(metadata()), key=lambda p: p["name"]):
        for name, text in licence_texts(package):
            key = hashlib.sha256(text.encode()).hexdigest()
            names = by_text.setdefault(key, (text, []))[1]
            if name not in names:
                names.append(name)
    entries = sorted(by_text.values(), key=lambda e: (e[1][0], e[0]))
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write(f"\n{SEPARATOR}\n".join(f"{', '.join(names)}\n\n{text}" for text, names in entries))
        f.write("\n")
    print(f"{OUT}: {len(entries)} licence texts")


if __name__ == "__main__":
    main()
