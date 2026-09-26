#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Packs the Flutter Linux release bundle into an AppImage.
#
#   packaging/linux/build-appimage.sh [--bundle DIR] [--version X.Y.Z]
#                                     [--output DIR] [--arch x86_64|aarch64]
#
# Prerequisite: `flutter build linux --release` (run from app/), which produces
# app/build/linux/<x64|arm64>/release/bundle. Build on the oldest distribution
# you want to support (glibc is not bundled; CI uses Ubuntu 24.04, the oldest
# base the PipeWire bindings build on).
#
# The AppImage bundles the app and its Flutter/plugin libraries only. It
# requires from the host, like every Flutter Linux app: GTK 3, GLib, libepoxy,
# fontconfig, X11/Xi, and **libpipewire-0.3** (hfa-capture links it
# dynamically; it must match the host's PipeWire daemon and its SPA plugins,
# so it is deliberately not bundled).
#
# appimagetool is taken from $APPIMAGETOOL, else from PATH, else downloaded
# (continuous build from github.com/AppImage/appimagetool) into
# ${XDG_CACHE_HOME:-~/.cache}/hfa-packaging. It always runs with
# APPIMAGE_EXTRACT_AND_RUN=1, so FUSE is not needed (containers, CI).
# The AppImage is written to packaging/dist/ unless --output says otherwise.
#
# Environment: APPIMAGETOOL, APPIMAGE_UPDATE_INFO (embedded update information,
# e.g. "gh-releases-zsync|owner|repo|latest|Headphone_for_All-*x86_64.AppImage.zsync").

set -euo pipefail

readonly APP_ID="io.github.shdavlatbek.hfa"
readonly APP_NAME="Headphone_for_All"
readonly BINARY="headphone_for_all"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
readonly SCRIPT_DIR REPO_ROOT

die() {
  echo "build-appimage: error: $*" >&2
  exit 1
}

log() {
  echo "build-appimage: $*"
}

usage() {
  sed -n '4,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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
    -h | --help) usage; exit 0 ;;
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
  # pubspec.yaml "version: 1.2.3+4" -> "1.2.3"
  version="$(sed -n 's/^version:[[:space:]]*\([^+[:space:]]*\).*/\1/p' "${REPO_ROOT}/app/pubspec.yaml" | head -n 1)"
  [[ -n "${version}" ]] || die "cannot read the version from app/pubspec.yaml; pass --version"
fi

[[ -x "${bundle}/${BINARY}" ]] || die "no ${BINARY} in ${bundle}; run 'flutter build linux --release' first"
[[ -f "${bundle}/lib/libhfa_ffi.so" ]] || die "no lib/libhfa_ffi.so in ${bundle}; the Rust library was not bundled"
[[ -d "${bundle}/data/flutter_assets" ]] || die "no data/flutter_assets in ${bundle}"

icons="${REPO_ROOT}/app/linux/icons/hicolor"
[[ -f "${icons}/256x256/apps/${APP_ID}.png" ]] || die "missing icons; run packaging/icon/generate.py"

find_appimagetool() {
  if [[ -n "${APPIMAGETOOL:-}" ]]; then
    [[ -x "${APPIMAGETOOL}" ]] || die "APPIMAGETOOL=${APPIMAGETOOL} is not executable"
    echo "${APPIMAGETOOL}"
    return
  fi
  if command -v appimagetool > /dev/null 2>&1; then
    command -v appimagetool
    return
  fi
  local cache="${XDG_CACHE_HOME:-${HOME}/.cache}/hfa-packaging"
  local host_arch
  host_arch="$(uname -m)"
  local tool="${cache}/appimagetool-${host_arch}.AppImage"
  if [[ ! -x "${tool}" ]]; then
    mkdir -p "${cache}"
    local url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${host_arch}.AppImage"
    log "downloading ${url}" >&2
    curl -fsSL --retry 3 -o "${tool}.part" "${url}" || die "download of appimagetool failed"
    chmod +x "${tool}.part"
    mv "${tool}.part" "${tool}"
  fi
  echo "${tool}"
}

workdir="$(mktemp -d)"
trap 'rm -rf "${workdir}"' EXIT
appdir="${workdir}/${APP_NAME}.AppDir"
prefix="${appdir}/usr/lib/headphone-for-all"

log "staging ${bundle} (version ${version}, ${arch})"
mkdir -p "${prefix}" "${appdir}/usr/bin" "${appdir}/usr/share/applications" \
  "${appdir}/usr/share/metainfo" "${appdir}/usr/share/icons"
cp -a "${bundle}/." "${prefix}/"
ln -s "../lib/headphone-for-all/${BINARY}" "${appdir}/usr/bin/${BINARY}"

cat > "${appdir}/AppRun" << 'EOF'
#!/bin/sh
# Entry point of the Headphone for All AppImage.
HERE="$(dirname "$(readlink -f "$0")")"
exec "${HERE}/usr/lib/headphone-for-all/headphone_for_all" "$@"
EOF
chmod 755 "${appdir}/AppRun"

install -m 644 "${SCRIPT_DIR}/${APP_ID}.desktop" "${appdir}/${APP_ID}.desktop"
install -m 644 "${SCRIPT_DIR}/${APP_ID}.desktop" "${appdir}/usr/share/applications/${APP_ID}.desktop"
# appimagetool validates usr/share/metainfo/<desktop id>.appdata.xml.
install -m 644 "${SCRIPT_DIR}/${APP_ID}.metainfo.xml" "${appdir}/usr/share/metainfo/${APP_ID}.appdata.xml"
cp -a "${icons}" "${appdir}/usr/share/icons/"
install -m 644 "${icons}/256x256/apps/${APP_ID}.png" "${appdir}/${APP_ID}.png"
ln -s "${APP_ID}.png" "${appdir}/.DirIcon"

# Report host libraries the bundle needs that this machine lacks, and make sure
# libpipewire was not bundled by accident.
if command -v ldd > /dev/null 2>&1; then
  missing="$(find "${prefix}" -type f \( -name '*.so*' -o -name "${BINARY}" \) -exec ldd {} + 2> /dev/null \
    | grep 'not found' | sort -u || true)"
  if [[ -n "${missing}" ]]; then
    echo "build-appimage: warning: unresolved libraries on the build machine:" >&2
    echo "${missing}" >&2
  fi
fi
if find "${prefix}" -name 'libpipewire-0.3.so*' | grep -q .; then
  die "libpipewire-0.3 is bundled; it must come from the host"
fi

tool="$(find_appimagetool)"
mkdir -p "${output}"
target="${output}/${APP_NAME}-${version}-${arch}.AppImage"
tool_args=()
if [[ -n "${APPIMAGE_UPDATE_INFO:-}" ]]; then
  tool_args+=(--updateinformation "${APPIMAGE_UPDATE_INFO}")
fi
export APPIMAGE_EXTRACT_AND_RUN=1

log "running $(basename "${tool}")"
ARCH="${arch}" VERSION="${version}" "${tool}" ${tool_args[@]+"${tool_args[@]}"} "${appdir}" "${target}"
log "wrote ${target}"
