#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Builds a single-file Flatpak bundle (.flatpak) from the Flutter Linux
# release bundle, using io.github.shdavlatbek.hfa.yml.
#
#   packaging/linux/build-flatpak.sh [--bundle DIR] [--version X.Y.Z]
#                                    [--output DIR] [--arch x86_64|aarch64]
#
# Prerequisites: `flutter build linux --release` (run from app/), flatpak and
# flatpak-builder (apt-get install flatpak flatpak-builder). The runtime and
# SDK named in the manifest are installed from Flathub (--user) when missing.
# The result is packaging/dist/Headphone_for_All-<version>-<arch>.flatpak
# unless --output says otherwise; install it with
# `flatpak install --user <file>.flatpak`.

set -euo pipefail

readonly APP_ID="io.github.shdavlatbek.hfa"
readonly APP_NAME="Headphone_for_All"
readonly FLATHUB_REPO="https://dl.flathub.org/repo/flathub.flatpakrepo"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
readonly SCRIPT_DIR REPO_ROOT

die() {
  echo "build-flatpak: error: $*" >&2
  exit 1
}

log() {
  echo "build-flatpak: $*"
}

arch="$(uname -m)"
bundle=""
version=""
output="${REPO_ROOT}/packaging/dist"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle) bundle="${2:?--bundle needs a directory}"; shift 2 ;;
    --version) version="${2:?--version needs a value}"; shift 2 ;;
    --output) output="${2:?--output needs a directory}"; shift 2 ;;
    --arch) arch="${2:?--arch needs a value}"; shift 2 ;;
    -h | --help) sed -n '4,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

case "${arch}" in
  x86_64 | amd64) arch="x86_64"; flutter_arch="x64" ;;
  aarch64 | arm64) arch="aarch64"; flutter_arch="arm64" ;;
  *) die "unsupported architecture: ${arch}" ;;
esac

if [[ -z "${bundle}" ]]; then
  bundle="${REPO_ROOT}/app/build/linux/${flutter_arch}/release/bundle"
fi
if [[ -z "${version}" ]]; then
  version="$(sed -n 's/^version:[[:space:]]*\([^+[:space:]]*\).*/\1/p' "${REPO_ROOT}/app/pubspec.yaml" | head -n 1)"
  [[ -n "${version}" ]] || die "cannot read the version from app/pubspec.yaml; pass --version"
fi

[[ -x "${bundle}/headphone_for_all" ]] || die "no headphone_for_all in ${bundle}; run 'flutter build linux --release' first"
[[ -f "${bundle}/lib/libhfa_ffi.so" ]] || die "no lib/libhfa_ffi.so in ${bundle}"
command -v flatpak > /dev/null 2>&1 || die "flatpak is not installed"
command -v flatpak-builder > /dev/null 2>&1 || die "flatpak-builder is not installed"

# The manifest reads the bundle from _flatpak/bundle (see its sources).
staging="${SCRIPT_DIR}/_flatpak"
rm -rf "${staging}"
mkdir -p "${staging}"
cp -a "${bundle}" "${staging}/bundle"
trap 'rm -rf "${staging}"' EXIT

flatpak remote-add --user --if-not-exists flathub "${FLATHUB_REPO}"

work="$(mktemp -d)"
trap 'rm -rf "${staging}" "${work}"' EXIT

log "building ${APP_ID} ${version} (${arch})"
flatpak-builder --user --arch="${arch}" --install-deps-from=flathub --force-clean \
  --disable-rofiles-fuse --state-dir="${work}/state" --repo="${work}/repo" \
  "${work}/build" "${SCRIPT_DIR}/${APP_ID}.yml"

mkdir -p "${output}"
target="${output}/${APP_NAME}-${version}-${arch}.flatpak"
flatpak build-bundle --arch="${arch}" --runtime-repo="${FLATHUB_REPO}" \
  "${work}/repo" "${target}" "${APP_ID}"
log "wrote ${target}"
