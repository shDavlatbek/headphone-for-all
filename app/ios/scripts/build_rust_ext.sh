#!/bin/sh
# build_rust_ext.sh - builds the Rust sender (core/hfa-ffi, C ABI only) as a static library for
# the HfaBroadcast broadcast upload extension.
#
# Runs as the first build phase ("Build Rust sender (hfa-ffi)") of the HfaBroadcast target, so
# it reads Xcode's build environment:
#   PLATFORM_NAME   iphoneos | iphonesimulator
#   ARCHS           e.g. "arm64" or "arm64 x86_64"
#   CONFIGURATION   Debug -> cargo dev profile; Release / Profile -> --release
#   BUILT_PRODUCTS_DIR, PROJECT_DIR, PROJECT_TEMP_DIR
# and writes $BUILT_PRODUCTS_DIR/libhfa_ext.a (lipo'd when several ARCHS are built), which the
# extension links with -lhfa_ext.
#
# Why a separate library and not the Flutter app's copy: the app links hfa-ffi through
# cargokit (rust_builder pod, `-force_load .../libhfa_ffi.a`, with the flutter_rust_bridge API).
# The extension is a separate Mach-O binary that only needs the C ABI, so it gets its own build
# (`--no-default-features --features bundled-opus`: no flutter_rust_bridge, no Dart shims), in its
# own cargo target directory, under its own name (libhfa_ext.a). No binary ever links both
# archives, so duplicate symbols are impossible.
#
# Overrides (environment):
#   HFA_EXT_RUST_PROFILE=release|debug   force a cargo profile
#   HFA_EXT_CARGO_TARGET_DIR=<dir>       cargo target directory
#                                        (default: $PROJECT_TEMP_DIR/hfa_ext_cargo)
#
# PATH: sources ~/.cargo/env, drops Xcode's developer directories and appends ~/.cargo/bin,
# /opt/homebrew/bin and /usr/local/bin (an Xcode GUI build has a minimal PATH); needs cargo and
# cmake (bundled libopus).
#
# Manual use outside Xcode (on a Mac), e.g. to check that the library builds:
#   PLATFORM_NAME=iphoneos ARCHS=arm64 CONFIGURATION=Release BUILT_PRODUCTS_DIR=/tmp/out \
#     PROJECT_DIR="$PWD/app/ios" PROJECT_TEMP_DIR=/tmp/hfa-ext sh app/ios/scripts/build_rust_ext.sh

set -eu

fail() {
  # "error:" lines are shown as build errors by Xcode.
  echo "error: build_rust_ext.sh: $*" >&2
  exit 1
}

: "${PLATFORM_NAME:?must run inside an Xcode build phase (PLATFORM_NAME unset)}"
: "${ARCHS:?ARCHS unset}"
: "${BUILT_PRODUCTS_DIR:?BUILT_PRODUCTS_DIR unset}"
: "${PROJECT_DIR:?PROJECT_DIR unset}"
CONFIGURATION="${CONFIGURATION:-Release}"

# Xcode runs build phases with a minimal PATH: pick up rustup's cargo.
if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
# Same as cargokit (build_pod.sh): drop Xcode's developer tool directories from PATH so that
# host build scripts use the regular /usr/bin toolchain shims.
PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -v 'Contents/Developer/' | tr '\n' ':')
PATH=${PATH%:}
# A build started from the Xcode GUI does not see the shell's PATH either: add rustup's and
# Homebrew's directories (Apple Silicon, Intel), where cargo and cmake usually live.
for dir in "$HOME/.cargo/bin" /opt/homebrew/bin /usr/local/bin; do
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) if [ -d "$dir" ]; then PATH="$PATH:$dir"; fi ;;
  esac
done
export PATH

command -v cargo >/dev/null 2>&1 || fail "cargo not found: install Rust with rustup (https://rustup.rs)"
# The bundled libopus (opusic-sys) is built with CMake.
command -v cmake >/dev/null 2>&1 || fail "cmake not found: install it (brew install cmake)"

MANIFEST="$PROJECT_DIR/../../core/Cargo.toml"
[ -f "$MANIFEST" ] || fail "Rust workspace not found at $MANIFEST"

TARGET_DIR="${HFA_EXT_CARGO_TARGET_DIR:-${PROJECT_TEMP_DIR:-$PROJECT_DIR/../build/ios}/hfa_ext_cargo}"

case "${HFA_EXT_RUST_PROFILE:-}" in
  release) PROFILE=release ;;
  debug) PROFILE=debug ;;
  "")
    if [ "$CONFIGURATION" = "Debug" ]; then PROFILE=debug; else PROFILE=release; fi
    ;;
  *) fail "HFA_EXT_RUST_PROFILE must be release or debug" ;;
esac
if [ "$PROFILE" = "release" ]; then PROFILE_FLAG="--release"; else PROFILE_FLAG=""; fi

# Maps an Xcode arch on the current platform to a Rust target triple.
rust_target() {
  case "$PLATFORM_NAME:$1" in
    iphoneos:arm64 | iphoneos:arm64e) echo "aarch64-apple-ios" ;;
    iphonesimulator:arm64) echo "aarch64-apple-ios-sim" ;;
    iphonesimulator:x86_64) echo "x86_64-apple-ios" ;;
    *) return 1 ;;
  esac
}

TRIPLES=""
for arch in $ARCHS; do
  triple=$(rust_target "$arch") || fail "unsupported platform/arch: $PLATFORM_NAME/$arch"
  case " $TRIPLES " in
    *" $triple "*) ;;
    *) TRIPLES="$TRIPLES $triple" ;;
  esac
done

# The built archives, collected in "$@" (paths may contain spaces).
set --
for triple in $TRIPLES; do
  if command -v rustup >/dev/null 2>&1; then
    if ! rustup target list --installed | grep -qx "$triple"; then
      echo "note: installing Rust target $triple"
      rustup target add "$triple"
    fi
  fi
  echo "note: building hfa-ffi ($PROFILE) for $triple"
  # Only the staticlib crate type: the cdylib / rlib of hfa-ffi are not needed here. --locked:
  # like CI, never rewrite core/Cargo.lock from an Xcode build.
  # shellcheck disable=SC2086 # PROFILE_FLAG is empty or a single word.
  cargo rustc \
    --manifest-path "$MANIFEST" \
    --locked \
    -p hfa-ffi \
    --lib \
    --crate-type staticlib \
    --no-default-features \
    --features bundled-opus \
    --target "$triple" \
    --target-dir "$TARGET_DIR" \
    $PROFILE_FLAG
  lib="$TARGET_DIR/$triple/$PROFILE/libhfa_ffi.a"
  [ -f "$lib" ] || fail "cargo did not produce $lib"
  set -- "$@" "$lib"
done

mkdir -p "$BUILT_PRODUCTS_DIR"
OUT="$BUILT_PRODUCTS_DIR/libhfa_ext.a"
if [ "$#" -eq 1 ]; then
  cp -f "$1" "$OUT"
else
  lipo -create "$@" -output "$OUT"
fi
echo "note: wrote $OUT"
