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

stage="dist/stage-linux/pulse-$VERSION"
rm -rf dist/stage-linux
mkdir -p "$stage/share/applications" "$stage/share/icons/hicolor/256x256/apps"
cp target/release/pulse "$stage/pulse"
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
