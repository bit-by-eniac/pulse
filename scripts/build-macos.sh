#!/usr/bin/env bash
# Builds Pulse.app and a .dmg on macOS.
#
#   scripts/build-macos.sh [local|arm64|x86_64|universal|all]
#
#   local      this machine's architecture (default; fastest)
#   arm64      Apple Silicon
#   x86_64     Intel
#   universal  one app that runs on both
#   all        arm64 + universal
#
# Output: dist/Pulse-<version>-macos-<arch>.dmg
# Optional signing: MACOS_SIGN_IDENTITY="Developer ID Application: ..." (ad-hoc otherwise)
set -euo pipefail
cd "$(dirname "$0")/.."

[[ "$(uname)" == "Darwin" ]] || { echo "This script must run on macOS." >&2; exit 1; }
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET_ARG="${1:-local}"
mkdir -p dist
# Keep this machine's paths (home folder, checkout) out of the binary: Rust embeds
# source paths in panic messages. Encoded form, so paths with spaces survive.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~"$'\x1f'"--remap-path-prefix=$PWD=."

build_one() { # <rust-target>
  rustup target add "$1" >/dev/null 2>&1 || true
  cargo build --release --target "$1"
}

# dmgbuild (Python) lays out the .dmg window without driving Finder. Pinned, and
# fetched on the fly by uvx or pipx when it is not installed.
DMGBUILD_VERSION=1.6.7
dmgbuild_cmd() {
  if command -v dmgbuild >/dev/null; then echo dmgbuild
  elif command -v uvx >/dev/null; then echo "uvx dmgbuild==$DMGBUILD_VERSION"
  elif command -v pipx >/dev/null; then echo "pipx run --spec dmgbuild==$DMGBUILD_VERSION dmgbuild"
  else return 1; fi
}

make_bundle() { # <arch-label> <binary>
  local label="$1" bin="$2" app="dist/stage-$1/Pulse.app"
  rm -rf "dist/stage-$label"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$bin" "$app/Contents/MacOS/pulse"
  cp assets/icon.icns "$app/Contents/Resources/icon.icns"
  sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
  codesign --force --deep --sign "${MACOS_SIGN_IDENTITY:--}" "$app"
  local dmg="dist/Pulse-$VERSION-macos-$label.dmg"
  rm -f "$dmg"
  if dmgbuild_cmd >/dev/null; then
    # Styled window: background with an arrow, icon positions, volume icon. The volume
    # icon stops at 256 px: 54 KB instead of the 330 KB of the full icon.icns.
    local set="dist/stage-$label/volume.iconset"
    mkdir -p "$set"
    for s in 16 32 128; do
      cp "assets/icon-$s.png" "$set/icon_${s}x${s}.png"
      cp "assets/icon-$((s * 2)).png" "$set/icon_${s}x${s}@2x.png"
    done
    iconutil -c icns "$set" -o "dist/stage-$label/volume.icns"
    $(dmgbuild_cmd) -s packaging/macos/dmg-settings.py -D app="$app" -D volume_icon="dist/stage-$label/volume.icns" \
      "Pulse" "$dmg" >/dev/null
  else
    echo "note: dmgbuild not found (install uv or pipx), making a plain .dmg" >&2
    ln -s /Applications "dist/stage-$label/Applications"
    hdiutil create -quiet -volname "Pulse" -srcfolder "dist/stage-$label" -ov -format UDZO "$dmg"
  fi
  rm -rf "dist/stage-$label"
  echo "-> $dmg"
}

arm64()     { build_one aarch64-apple-darwin; make_bundle arm64  target/aarch64-apple-darwin/release/pulse; }
x86_64()    { build_one x86_64-apple-darwin;  make_bundle x86_64 target/x86_64-apple-darwin/release/pulse; }
universal() {
  build_one aarch64-apple-darwin; build_one x86_64-apple-darwin
  lipo -create -output dist/pulse-universal \
    target/aarch64-apple-darwin/release/pulse target/x86_64-apple-darwin/release/pulse
  make_bundle universal dist/pulse-universal
  rm -f dist/pulse-universal
}

case "$TARGET_ARG" in
  local)
    if [[ "$(uname -m)" == "arm64" ]]; then arm64; else x86_64; fi ;;
  arm64) arm64 ;;
  x86_64) x86_64 ;;
  universal) universal ;;
  all) arm64; universal ;;
  *) echo "usage: $0 [local|arm64|x86_64|universal|all]" >&2; exit 2 ;;
esac
