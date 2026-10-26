#!/usr/bin/env bash
# Builds Pulse.app, with mpv, ffmpeg and QuickJS inside, and a .dmg on macOS.
#
#   scripts/build-macos.sh [local|arm64|x86_64|all]
#
#   local    this machine's architecture (default)
#   arm64    Apple silicon
#   x86_64   Intel
#   all      both: one .dmg each, half the size of a universal one
#
# The tools come from dist/tools-macos-<arch>: run scripts/build-tools.sh
# macos-arm64 (or macos-x86_64) first.
# Output: dist/Pulse-<version>-macos-apple-silicon.dmg, ...-macos-intel.dmg
# Optional signing: MACOS_SIGN_IDENTITY="Developer ID Application: ..." (ad-hoc otherwise)
set -euo pipefail
cd "$(dirname "$0")/.."

[[ "$(uname)" == "Darwin" ]] || { echo "This script must run on macOS." >&2; exit 1; }
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET_ARG="${1:-local}"
SIGN="${MACOS_SIGN_IDENTITY:--}"
TOOLS=(mpv ffmpeg qjs)
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

make_bundle() { # <label> <binary> <tools dir>
  local label="$1" bin="$2" tools="$3"
  local stage="dist/stage-$label" app="dist/stage-$label/Pulse.app"
  rm -rf "$stage"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$bin" "$app/Contents/MacOS/pulse"
  cp assets/icon.icns "$app/Contents/Resources/icon.icns"
  sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
  # Next to Pulse, where it looks for them first.
  for f in "${TOOLS[@]}"; do
    cp "$tools/$f" "$app/Contents/MacOS/$f"
    codesign --force --sign "$SIGN" "$app/Contents/MacOS/$f"
  done
  cp -R "$tools/LICENSES" "$app/Contents/Resources/LICENSES"
  codesign --force --deep --sign "$SIGN" "$app"
  local dmg="dist/Pulse-$VERSION-macos-$label.dmg"
  rm -f "$dmg"
  if dmgbuild_cmd >/dev/null; then
    # Styled window: background with an arrow, icon positions, volume icon. The volume
    # icon stops at 256 px: 54 KB instead of the 330 KB of the full icon.icns.
    local set="$stage/volume.iconset"
    mkdir -p "$set"
    for s in 16 32 128; do
      cp "assets/icon-$s.png" "$set/icon_${s}x${s}.png"
      cp "assets/icon-$((s * 2)).png" "$set/icon_${s}x${s}@2x.png"
    done
    iconutil -c icns "$set" -o "$stage/volume.icns"
    $(dmgbuild_cmd) -s packaging/macos/dmg-settings.py -D app="$app" -D volume_icon="$stage/volume.icns" \
      "Pulse" "$dmg" >/dev/null
  else
    echo "note: dmgbuild not found (install uv or pipx), making a plain .dmg" >&2
    ln -s /Applications "$stage/Applications"
    hdiutil create -quiet -volname "Pulse" -srcfolder "$stage" -ov -format UDZO "$dmg"
  fi
  rm -rf "$stage"
  echo "-> $dmg"
}

tools_for() { # <arch>: the directory scripts/build-tools.sh made
  local dir="dist/tools-macos-$1"
  for f in "${TOOLS[@]}"; do
    [[ -f "$dir/$f" ]] || { echo "missing $dir/$f: run scripts/build-tools.sh macos-$1 first" >&2; exit 1; }
  done
  echo "$dir"
}

package() { # <arch> <rust target> <label>
  local tools
  tools=$(tools_for "$1")
  build_one "$2"
  make_bundle "$3" "target/$2/release/pulse" "$tools"
}
arm64() { package arm64 aarch64-apple-darwin apple-silicon; }
x86_64() { package x86_64 x86_64-apple-darwin intel; }

case "$TARGET_ARG" in
  local)
    if [[ "$(uname -m)" == "arm64" ]]; then arm64; else x86_64; fi ;;
  arm64) arm64 ;;
  x86_64) x86_64 ;;
  all) arm64; x86_64 ;;
  *) echo "usage: $0 [local|arm64|x86_64|all]" >&2; exit 2 ;;
esac
