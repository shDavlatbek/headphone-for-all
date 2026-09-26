#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Packs the macOS release app into a drag-to-Applications disk image, and
# optionally signs and notarizes it. Runs on macOS only.
#
#   packaging/macos/build-dmg.sh [--app PATH.app] [--version X.Y.Z] [--output DIR]
#                                [--sign "Developer ID Application: …"] [--notarize]
#
# Prerequisite: `flutter build macos --release` (run from app/), which produces
# app/build/macos/Build/Products/Release/<name>.app (a universal binary).
# The DMG is written to packaging/dist/Headphone_for_All-<version>-macos.dmg
# unless --output says otherwise. `create-dmg` (brew install create-dmg) gives
# a styled window; without it plain `hdiutil` is used.
#
# Signing (--sign IDENTITY or $MACOS_SIGN_IDENTITY): re-signs nested code and
# the app with the hardened runtime (required for notarization) and the
# entitlements in app/macos/Runner/Release.entitlements, then signs the DMG.
#
# Notarization (--notarize, needs signing) submits the DMG with
# `xcrun notarytool` and staples the ticket. Credentials come from the
# environment, never from this repository (CI secrets):
#   APPLE_NOTARY_PROFILE                      a `notarytool store-credentials` keychain profile, or
#   APPLE_API_KEY_PATH, APPLE_API_KEY_ID,
#   APPLE_API_ISSUER                          an App Store Connect API key (.p8), or
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   an Apple ID with an app-specific password.

set -euo pipefail

readonly APP_NAME="Headphone for All"
readonly FILE_NAME="Headphone_for_All"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
readonly SCRIPT_DIR REPO_ROOT

die() {
  echo "build-dmg: error: $*" >&2
  exit 1
}

log() {
  echo "build-dmg: $*"
}

app=""
version=""
output="${REPO_ROOT}/packaging/dist"
identity="${MACOS_SIGN_IDENTITY:-}"
notarize=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) app="${2:?--app needs a path}"; shift 2 ;;
    --version) version="${2:?--version needs a value}"; shift 2 ;;
    --output) output="${2:?--output needs a directory}"; shift 2 ;;
    --sign) identity="${2:?--sign needs an identity}"; shift 2 ;;
    --notarize) notarize=true; shift ;;
    -h | --help) sed -n '4,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] || die "this script needs macOS (hdiutil, codesign, notarytool)"

if [[ -z "${app}" ]]; then
  products="${REPO_ROOT}/app/build/macos/Build/Products/Release"
  shopt -s nullglob
  candidates=("${products}"/*.app)
  shopt -u nullglob
  [[ ${#candidates[@]} -eq 1 ]] || die "expected one .app in ${products}; run 'flutter build macos --release' or pass --app"
  app="${candidates[0]}"
fi
[[ -d "${app}/Contents/MacOS" ]] || die "${app} is not an application bundle"
if [[ -z "${version}" ]]; then
  version="$(sed -n 's/^version:[[:space:]]*\([^+[:space:]]*\).*/\1/p' "${REPO_ROOT}/app/pubspec.yaml" | head -n 1)"
  [[ -n "${version}" ]] || die "cannot read the version from app/pubspec.yaml; pass --version"
fi
if [[ "${notarize}" == true && -z "${identity}" ]]; then
  die "--notarize needs a signing identity (--sign or MACOS_SIGN_IDENTITY)"
fi

sign_app() {
  local entitlements="${REPO_ROOT}/app/macos/Runner/Release.entitlements"
  local args=(--force --timestamp --options runtime --sign "${identity}")
  log "signing ${app} as ${identity}"
  # Inside-out: nested frameworks and dylibs first, then the app itself.
  while IFS= read -r -d '' nested; do
    codesign "${args[@]}" "${nested}"
  done < <(find "${app}/Contents" \( -name '*.framework' -o -name '*.dylib' \) -print0 | sort -rz)
  if [[ -f "${entitlements}" ]]; then
    codesign "${args[@]}" --entitlements "${entitlements}" "${app}"
  else
    codesign "${args[@]}" "${app}"
  fi
  codesign --verify --deep --strict --verbose=2 "${app}"
}

notary_auth() {
  if [[ -n "${APPLE_NOTARY_PROFILE:-}" ]]; then
    printf '%s\0' --keychain-profile "${APPLE_NOTARY_PROFILE}"
  elif [[ -n "${APPLE_API_KEY_PATH:-}" && -n "${APPLE_API_KEY_ID:-}" && -n "${APPLE_API_ISSUER:-}" ]]; then
    printf '%s\0' --key "${APPLE_API_KEY_PATH}" --key-id "${APPLE_API_KEY_ID}" --issuer "${APPLE_API_ISSUER}"
  elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
    printf '%s\0' --apple-id "${APPLE_ID}" --team-id "${APPLE_TEAM_ID}" --password "${APPLE_APP_PASSWORD}"
  else
    die "no notarization credentials (see the header of this script)"
  fi
}

if [[ -n "${identity}" ]]; then
  sign_app
fi

mkdir -p "${output}"
dmg="${output}/${FILE_NAME}-${version}-macos.dmg"
rm -f "${dmg}"
staging="$(mktemp -d)"
trap 'rm -rf "${staging}"' EXIT
app_name="$(basename "${app}")"
# ditto keeps signatures, extended attributes and symlinks intact.
ditto "${app}" "${staging}/${app_name}"

if command -v create-dmg > /dev/null 2>&1; then
  log "creating ${dmg} with create-dmg"
  extra=()
  # Headless CI runners cannot drive Finder over AppleScript to lay out the
  # window; the image is still valid without that layout.
  if [[ -n "${CI:-}" ]]; then
    extra+=(--skip-jenkins)
  fi
  create-dmg --volname "${APP_NAME}" --window-size 540 360 --icon-size 112 \
    --icon "${app_name}" 140 170 --hide-extension "${app_name}" \
    --app-drop-link 400 170 --no-internet-enable ${extra[@]+"${extra[@]}"} \
    "${dmg}" "${staging}/"
else
  log "creating ${dmg} with hdiutil"
  ln -s /Applications "${staging}/Applications"
  # `hdiutil create` fails now and then with "Resource busy" while XProtect or
  # Spotlight still scan the staging folder (common on hosted CI runners,
  # actions/runner-images#7522); retry with a growing pause.
  attempt=1
  until hdiutil create -volname "${APP_NAME}" -srcfolder "${staging}" -fs HFS+ \
    -format UDZO -imagekey zlib-level=9 -ov "${dmg}"; do
    [[ ${attempt} -lt ${HDIUTIL_ATTEMPTS:-5} ]] || die "hdiutil create failed ${attempt} times"
    log "hdiutil create failed (attempt ${attempt}); retrying in $((attempt * 5)) s"
    rm -f "${dmg}"
    sleep $((attempt * 5))
    attempt=$((attempt + 1))
  done
fi

if [[ -n "${identity}" ]]; then
  codesign --force --timestamp --sign "${identity}" "${dmg}"
fi

if [[ "${notarize}" == true ]]; then
  auth=()
  while IFS= read -r -d '' part; do auth+=("${part}"); done < <(notary_auth)
  log "submitting ${dmg} for notarization (this can take minutes)"
  xcrun notarytool submit "${dmg}" "${auth[@]}" --wait
  xcrun stapler staple "${dmg}"
  spctl --assess --type open --context context:primary-signature --verbose=2 "${dmg}"
fi

log "wrote ${dmg}"
