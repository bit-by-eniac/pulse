#!/usr/bin/env bash
# Builds the tools the "full" packages carry, cut down to what Pulse uses:
#   ffmpeg  decodes the picture: H.264 and VP9 from MP4, WebM and HLS (no AV1,
#           which Pulse only takes when a video has nothing else); on macOS on
#           the graphics chip where it can
#   mpv     plays the sound, with the OS media controls (Now Playing, media keys);
#           no video output, scripts or subtitles
#   qjs     QuickJS-ng, the JavaScript engine yt-dlp needs for YouTube
# yt-dlp is not included: Pulse downloads it and keeps it up to date.
#
#   scripts/build-tools.sh macos-arm64|macos-x86_64|windows-x64 [out-dir]
#
# Needs curl, a C/C++ compiler, meson, ninja, pkg-config, python3 and, for x86_64,
# nasm; on macOS also Swift (Xcode's command line tools). windows-x64 runs in MSYS2
# UCRT64, whose cppwinrt package mpv needs for the Windows media controls.
# Output: out-dir (default dist/tools-<target>) with the three programs and
# LICENSES/. Downloads are kept in target/tools/src for the next run.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET="${1:-}"
case "$TARGET" in
  macos-arm64) OS=macos; CPU=aarch64; ARCH=arm64 ;;
  macos-x86_64) OS=macos; CPU=x86_64; ARCH=x86_64 ;;
  windows-x64) OS=windows; CPU=x86_64; ARCH=x86_64 ;;
  *) echo "usage: $0 macos-arm64|macos-x86_64|windows-x64 [out-dir]" >&2; exit 2 ;;
esac
ROOT="$PWD"
SRC="$ROOT/target/tools/src"
WORK="$ROOT/target/tools/$TARGET"
# Compilers and pkg-config in MSYS2 are Windows programs: they get D:/... paths.
PREFIX="$WORK/prefix"
[[ $OS == windows ]] && PREFIX="$(cygpath -m "$PREFIX")"
OUT="${2:-dist/tools-$TARGET}"
rm -rf "$WORK"
mkdir -p "$SRC" "$PREFIX" "$OUT/LICENSES"
OUT="$(cd "$OUT" && pwd)"
JOBS=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)

# Sources, checked against these SHA-256 sums.
FFMPEG=(https://ffmpeg.org/releases/ffmpeg-8.1.tar.xz b072aed6871998cce9b36e7774033105ca29e33632be5b6347f3206898e0756a)
MPV=(https://github.com/mpv-player/mpv/archive/refs/tags/v0.41.0.tar.gz ee21092a5ee427353392360929dc64645c54479aefdb5babc5cfbb5fad626209 mpv-0.41.0.tar.gz)
LIBPLACEBO=(https://github.com/haasn/libplacebo/archive/refs/tags/v7.387.0.tar.gz f9e62d3774a9c4cfd198ec3abaa88294aaade2e31c04d5ab02bcf4abc98b1df5 libplacebo-7.387.0.tar.gz)
FAST_FLOAT=(https://github.com/fastfloat/fast_float/releases/download/v8.3.2/fast_float.h e40fc8f751e4d2d49efb583edbd81f64f5663527b84c43661e0fafd6871be3e3)
# libplacebo generates code with these and includes the Vulkan headers even without
# Vulkan (git submodules there, missing from its tarball).
VULKAN_HEADERS=(https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/v1.4.366.tar.gz 577b90c208e6fd39ec9a1460e48772f63c76f6f5afb441df2a977a0fdb3f618c Vulkan-Headers-v1.4.366.tar.gz)
JINJA=(https://files.pythonhosted.org/packages/df/bf/f7da0350254c0ed7c72f3e33cef02e048281fec7ecec5f032d4aac52226b/jinja2-3.1.6.tar.gz 0137fb05990d35f1275a587e9aee6d56da821fc83491a0fb838183be43f66d6d)
MARKUPSAFE=(https://files.pythonhosted.org/packages/38/9b/e422a865e1d5d57d0e509b4e0bf1c1a70a7f6382c29a5aa428df994c8bc8/markupsafe-3.0.4.tar.gz 2e9ad7dd851bf45fab9f75cbff4cb493fee9979e8d8c7c9c3ee119022518edd6)
LIBASS=(https://github.com/libass/libass/releases/download/0.17.5/libass-0.17.5.tar.xz 2dca25c0e0c837ddf00b52011b3f82cac1e4ddd3ad018227806b0c2288864acc)
HARFBUZZ=(https://github.com/harfbuzz/harfbuzz/releases/download/15.0.0/harfbuzz-15.0.0.tar.xz 968536e9abd782f07f7954044c0c4020199fcdd39b41d221ec42d2a3be4a44df)
FRIBIDI=(https://github.com/fribidi/fribidi/releases/download/v1.0.17/fribidi-1.0.17.tar.xz 6949dcde27d41cebad1fd741fcafc36d55a1020d2d872d4a6eb3914caabbada2)
FREETYPE=(https://download.savannah.gnu.org/releases/freetype/freetype-2.15.0.tar.xz 93cd47eb719c325107f2d3cf087d06299e944c3b48a40b55537302304dea1dd5)
FAST_FLOAT_LICENSE=(https://raw.githubusercontent.com/fastfloat/fast_float/v8.3.2/LICENSE-MIT e562f3f974ced7e69dd1db77b820b36bcf8f30377f1aa105723fba449c53c4e6 fast_float-LICENSE-MIT)
# Built here: the project's own macOS binaries need macOS 26.
QUICKJS=(https://github.com/quickjs-ng/quickjs/archive/refs/tags/v0.17.0.tar.gz 559bc4c420475e55c7ab4510adbc562f55d7524d75e8e89d79ce4bb02f5687d9 quickjs-0.17.0.tar.gz)

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
fetch() { # <url> <sha256> [file name]: downloads once into $SRC, prints the path
  local file="$SRC/${3:-$(basename "$1")}"
  if [[ ! -f "$file" || "$(sha256 "$file")" != "$2" ]]; then
    curl -fsSL --retry 3 -o "$file.part" "$1"
    [[ "$(sha256 "$file.part")" == "$2" ]] || { echo "checksum mismatch: $1" >&2; exit 1; }
    mv "$file.part" "$file"
  fi
  echo "$file"
}
unpack_to() { # <dir> <fetch args...>
  local dir="$1" archive; shift
  archive=$(fetch "$@")
  mkdir -p "$dir"
  tar -xf "$archive" -C "$dir" --strip-components=1
}
unpack() { # <fetch args...>: unpacks into $WORK/<name>, prints that directory
  local dir
  dir="$WORK/$(basename "${3:-$1}" | sed -E 's/\.tar\.(gz|xz)$//')"
  unpack_to "$dir" "$@"
  echo "$dir"
}
logged() { # <name> <command...>: quiet unless it fails
  "${@:2}" > "$WORK/$1.log" 2>&1 || { tail -40 "$WORK/$1.log" >&2; echo "failed: $1 (log: $WORK/$1.log)" >&2; exit 1; }
}

# --- toolchain ---------------------------------------------------------------------

# Only what is built here is found: nothing from Homebrew or MSYS2 slips in.
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
export PKG_CONFIG_PATH=""
FF_ARGS=()
MESON_MACHINE=()
if [[ $OS == macos ]]; then
  MIN=11.0
  export MACOSX_DEPLOYMENT_TARGET=$MIN
  FLAGS="-arch $ARCH -mmacosx-version-min=$MIN"
  FF_ARGS+=(--cc="clang $FLAGS" --cxx="clang++ $FLAGS" --objcc="clang $FLAGS" --extra-ldflags="$FLAGS"
    --enable-pthreads --enable-securetransport)
  if [[ "$(uname -m)" != "$ARCH" ]]; then
    FF_ARGS+=(--enable-cross-compile --arch="$CPU" --target-os=darwin)
    # QuickJS's build declares generators it could run; Rosetta can.
    WRAPPER=""
    arch "-$ARCH" /usr/bin/true 2>/dev/null && WRAPPER="exe_wrapper = ['arch', '-$ARCH']"
    cat > "$WORK/cross.ini" <<EOF
[binaries]
c = ['clang', '-arch', '$ARCH', '-mmacosx-version-min=$MIN']
cpp = ['clang++', '-arch', '$ARCH', '-mmacosx-version-min=$MIN']
objc = ['clang', '-arch', '$ARCH', '-mmacosx-version-min=$MIN']
objcpp = ['clang++', '-arch', '$ARCH', '-mmacosx-version-min=$MIN']
ar = 'ar'
strip = 'strip'
pkg-config = 'pkg-config'
$WRAPPER

[host_machine]
system = 'darwin'
cpu_family = '$CPU'
cpu = '$ARCH'
endian = 'little'
EOF
    MESON_MACHINE=(--cross-file "$WORK/cross.ini")
  else
    export CC="clang $FLAGS" CXX="clang++ $FLAGS" OBJC="clang $FLAGS" OBJCXX="clang++ $FLAGS"
  fi
else
  # Everything linked in, so the programs need only what comes with Windows.
  FF_ARGS+=(--enable-w32threads --enable-schannel --extra-ldflags=-static --pkg-config-flags=--static)
  export LDFLAGS="-static"
fi
[[ $CPU == x86_64 ]] && ! command -v nasm >/dev/null && { echo "nasm is needed for x86_64" >&2; exit 1; }

meson_build() { # <source dir> <options...>: static libraries into $PREFIX; OPT: optimization
  local dir="$1" name; shift
  name=$(basename "$dir")
  # (The odd expansion: bash 3.2, macOS's, calls an empty array unset.)
  logged "$name-setup" meson setup "$dir/build" "$dir" ${MESON_MACHINE[@]+"${MESON_MACHINE[@]}"} --prefix="$PREFIX" --libdir=lib \
    --default-library=static -Dprefer_static=true --auto-features=disabled \
    -Ddebug=false -Doptimization="${OPT:-s}" -Db_ndebug=true -Dpkg_config_path="$PKG_CONFIG_LIBDIR" "$@"
  logged "$name-build" ninja -C "$dir/build" install
}

# ffmpeg with only the parts Pulse or mpv touch: `common` for both, then one for
# the picture (the ffmpeg program) and one for the sound (libraries for mpv).
# shellcheck disable=SC2054 # the commas are part of the options
FF_COMMON=(--disable-everything --disable-autodetect --disable-doc --disable-debug
  --disable-ffplay --disable-ffprobe --disable-avdevice --disable-shared --enable-static
  --enable-protocol=file,pipe,http,https,tcp,tls,hls,crypto --enable-demuxer=mov,matroska,hls,mpegts)
ffmpeg_build() { # <dir> <configure args...>
  local dir="$1"; shift
  (cd "$dir" && logged "$(basename "$dir")-configure" ./configure "${FF_COMMON[@]}" "${FF_ARGS[@]}" "$@" &&
    logged "$(basename "$dir")-build" make -j"$JOBS")
}

echo "== $TARGET: ffmpeg (picture)"
ff=$(unpack "${FFMPEG[@]}")
mv "$ff" "$ff-picture"
# On macOS the graphics chip decodes and scales (VideoToolbox) where it can.
PICTURE=()
# shellcheck disable=SC2054 # the commas are part of the options
[[ $OS == macos ]] && PICTURE=(--enable-videotoolbox --enable-hwaccel=h264_videotoolbox,vp9_videotoolbox
  --enable-filter=scale_vt,hwdownload)
ffmpeg_build "$ff-picture" --disable-swresample \
  --enable-decoder=h264,vp9 --enable-parser=h264,vp9 --enable-bsf=vp9_superframe_split \
  --enable-filter=fps,scale,format,null --enable-encoder=rawvideo --enable-muxer=rawvideo \
  ${PICTURE[@]+"${PICTURE[@]}"}
EXE=""; [[ $OS == windows ]] && EXE=.exe
cp "$ff-picture/ffmpeg$EXE" "$OUT/ffmpeg$EXE"

echo "== $TARGET: ffmpeg (sound, for mpv)"
ff=$(unpack "${FFMPEG[@]}")
# The PNG decoder is for the Now Playing artwork Pulse hands mpv.
ffmpeg_build "$ff" --prefix="$PREFIX" --disable-programs --enable-zlib \
  --enable-decoder=aac,opus,vorbis,mp3float,flac,png --enable-parser=aac,opus,vorbis,mpegaudio,flac,png \
  --enable-demuxer=aac,mp3,ogg,flac,image2,image_png_pipe \
  --enable-filter=aformat,anull,aresample,abuffer,abuffersink,format,null,scale,buffer,buffersink
logged ffmpeg-install make -C "$ff" install

echo "== $TARGET: mpv's libraries"
meson_build "$(unpack "${FREETYPE[@]}")" -Dharfbuzz=disabled -Dzlib=none -Dtests=disabled
meson_build "$(unpack "${FRIBIDI[@]}")" -Ddocs=false -Dbin=false -Dtests=false
meson_build "$(unpack "${HARFBUZZ[@]}")" -Dfreetype=enabled -Draster=disabled -Dvector=disabled \
  -Dsubset=disabled -Dtests=disabled -Dutilities=disabled -Ddocs=disabled -Dintrospection=disabled
# No font provider: with no video there are no subtitles to draw.
meson_build "$(unpack "${LIBASS[@]}")" -Drequire-system-font-provider=false
placebo=$(unpack "${LIBPLACEBO[@]}")
mkdir -p "$placebo/3rdparty/fast_float/include/fast_float"
cp "$(fetch "${FAST_FLOAT[@]}")" "$placebo/3rdparty/fast_float/include/fast_float/"
unpack_to "$placebo/3rdparty/jinja" "${JINJA[@]}"
unpack_to "$placebo/3rdparty/markupsafe" "${MARKUPSAFE[@]}"
unpack_to "$placebo/3rdparty/Vulkan-Headers" "${VULKAN_HEADERS[@]}"
meson_build "$placebo" -Ddemos=false -Dtests=false -Dbench=false -Dfuzz=false

echo "== $TARGET: mpv"
mpv=$(unpack "${MPV[@]}")
MPV_ARGS=(-Dlibmpv=false -Dcplayer=true -Dbuild-date=false -Dgl=disabled -Dlua=disabled -Dmanpage-build=disabled)
if [[ $OS == macos ]]; then
  # AVFoundation: mpv's fallback when Core Audio refuses a device (AirPods on macOS 27).
  MPV_ARGS+=(-Dcocoa=enabled -Dswift-build=enabled -Dmacos-media-player=enabled -Dcoreaudio=enabled -Davfoundation=enabled)
  [[ "$(uname -m)" != "$ARCH" ]] && MPV_ARGS+=(-Dswift-flags="-target $ARCH-apple-macos$MIN")
else
  MPV_ARGS+=(-Dwasapi=enabled -Dwin32-smtc=enabled -Dwin32-threads=enabled)
fi
meson_build "$mpv" "${MPV_ARGS[@]}"
cp "$PREFIX/bin/mpv$EXE" "$OUT/mpv$EXE"

echo "== $TARGET: QuickJS"
# Built for speed, not size: yt-dlp runs it on every lookup.
qjs=$(unpack "${QUICKJS[@]}")
OPT=2 meson_build "$qjs"
cp "$PREFIX/bin/qjs$EXE" "$OUT/qjs$EXE"

strip "$OUT/ffmpeg$EXE" "$OUT/mpv$EXE" "$OUT/qjs$EXE"
[[ $OS == macos ]] && for f in ffmpeg mpv qjs; do codesign --force --sign - "$OUT/$f"; done

# Licences: mpv (GPL) and the LGPL libraries are free software; their source is at
# the links in SOURCES.txt, built with this script.
L="$OUT/LICENSES"
cp "$mpv/LICENSE.GPL" "$L/mpv-GPL.txt"; cp "$mpv/Copyright" "$L/mpv-Copyright.txt"
cp "$ff/COPYING.LGPLv2.1" "$L/ffmpeg-LGPL.txt"
cp "$placebo/LICENSE" "$L/libplacebo-LGPL.txt"
cp "$WORK/libass-0.17.5/COPYING" "$L/libass.txt"
cp "$WORK/harfbuzz-15.0.0/COPYING" "$L/harfbuzz.txt"
cp "$WORK/fribidi-1.0.17/COPYING" "$L/fribidi-LGPL.txt"
cp "$WORK/freetype-2.15.0/LICENSE.TXT" "$L/freetype.txt"; cp "$WORK/freetype-2.15.0/docs/FTL.TXT" "$L/freetype-FTL.txt"
cp "$(fetch "${FAST_FLOAT_LICENSE[@]}")" "$L/fast_float-MIT.txt"
cp "$qjs/LICENSE" "$L/quickjs-MIT.txt"
cat > "$L/SOURCES.txt" <<EOF
Built with scripts/build-tools.sh of Pulse from these sources:
${FFMPEG[0]}
${MPV[0]}
${LIBPLACEBO[0]}
${FAST_FLOAT[0]} (MIT)
${LIBASS[0]}
${HARFBUZZ[0]}
${FRIBIDI[0]}
${FREETYPE[0]}
${QUICKJS[0]} (MIT)
EOF
ls -l "$OUT" | tail -n +2
