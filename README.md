<p align="center"><img src="assets/icon-256.png" width="128" alt="Pulse icon"></p>

<h1 align="center">Pulse</h1>

<p align="center">A tiny YouTube player. About 10 MB with everything it needs, no browser.</p>

- Search, or paste any YouTube link
- Video or audio only; the quality follows your connection
- Queue, playlists, shuffle, repeat
- Keeps playing when the window is minimised

## Install

Download Pulse from [Releases](../../releases/latest). Nothing else to install: Pulse fetches yt-dlp by itself.

- **macOS:** `…-macos-apple-silicon.dmg` for M1 and later, `…-macos-intel.dmg` for older Macs.
- **Windows:** `…-windows-x64.zip`. Unzip it anywhere and run `Pulse.exe`.
- **Linux:** `sudo apt install ./pulse-player_*.deb` (it brings mpv and ffmpeg). The `.tar.gz` needs them installed.

macOS says the app is damaged? It isn't notarized: run `xattr -dr com.apple.quarantine /Applications/Pulse.app`.

## When YouTube acts up

- Videos stop loading: **Settings → Update yt-dlp**.
- "Sign in to confirm you're not a bot": Pulse can use a browser where you're signed in to YouTube.
  Off by default; [what it does](docs/how-it-works.md#sign-in).

## Build

```sh
cargo run --release
```

`scripts/build-macos.sh`, `scripts/build-linux.sh` and `scripts/build-windows.ps1` put installers in `dist/`.
A push to `main` releases the version in `Cargo.toml` if it has no release yet.

More: [how it works](docs/how-it-works.md).
