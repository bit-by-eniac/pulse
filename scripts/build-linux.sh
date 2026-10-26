#!/usr/bin/env bash
# Builds Pulse on Linux: a .tar.gz and (if cargo-deb is installed) a .deb.
#
#   scripts/build-linux.sh
#
# Output: dist/Pulse-<version>-linux-<arch>.tar.gz, dist/pulse-player_<version>_<arch>.deb
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
mkdir -p dist
# Keep this machine's paths (home folder, checkout) out of the binary: Rust embeds
# source paths in panic messages. Encoded form, so paths with spaces survive.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~"$'\x1f'"--remap-path-prefix=$PWD=."
cargo build --release

# QuickJS (MIT, ~2.5 MB): the JavaScript engine yt-dlp needs for YouTube, used when
# no deno or node is installed. mpv and ffmpeg come from the distro.
QJS_VERSION=v0.17.0
case "$ARCH" in
  x86_64) QJS=(qjs-linux-x86_64 0bfc02511a9f549c28b53880d988fc7cd5d361e90c5e8afdfcd7dc6774ceace5) ;;
  aarch64) QJS=(qjs-linux-aarch64 3372133484edf50a69f3c67903af41206d22a061e930e3cfb63269272ef56d2e) ;;
  *) echo "QuickJS is fetched for x86_64 and aarch64 only, not $ARCH" >&2; exit 1 ;;
esac
mkdir -p target/tools
curl -fsSL --retry 3 -o target/tools/qjs "https://github.com/quickjs-ng/quickjs/releases/download/$QJS_VERSION/${QJS[0]}"
echo "${QJS[1]}  target/tools/qjs" | sha256sum -c --quiet
chmod +x target/tools/qjs
curl -fsSL --retry 3 -o target/tools/quickjs-LICENSE "https://raw.githubusercontent.com/quickjs-ng/quickjs/$QJS_VERSION/LICENSE"
echo "96f73f9d2a16c21a36b418f06073be26e7d6d5e7c1bc99756b21a4f2c74ef171  target/tools/quickjs-LICENSE" | sha256sum -c --quiet

stage="dist/stage-linux/pulse-$VERSION"
rm -rf dist/stage-linux
mkdir -p "$stage/share/applications" "$stage/share/icons/hicolor/256x256/apps" "$stage/tools"
cp target/release/pulse "$stage/pulse"
cp target/tools/qjs "$stage/tools/qjs"
cp target/tools/quickjs-LICENSE "$stage/tools/quickjs-LICENSE"
cp packaging/linux/pulse.desktop "$stage/share/applications/"
cp assets/icon-256.png "$stage/share/icons/hicolor/256x256/apps/pulse.png"
cp README.md "$stage/"
tar -C dist/stage-linux -czf "dist/Pulse-$VERSION-linux-$ARCH.tar.gz" "pulse-$VERSION"
rm -rf dist/stage-linux
echo "-> dist/Pulse-$VERSION-linux-$ARCH.tar.gz"

if command -v cargo-deb >/dev/null 2>&1 || cargo deb --version >/dev/null 2>&1; then
  cargo deb --no-build --output dist/
  echo "-> $(ls dist/*.deb | tail -1)"
else
  echo "(skipping .deb: install with 'cargo install cargo-deb')"
fi
