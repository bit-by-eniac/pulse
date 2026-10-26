# Pulse

A tiny YouTube audio/video player. Rust, one ~3.5 MB executable, no browser.

- Search videos, playlists and channels, or paste any YouTube link (also `pulse <link>` from a terminal)
- Video or audio-only, switchable any time; quality selector (Auto, 144p to 4K)
- Queue, playlist playback, shuffle, repeat, gapless-ish (next track is looked up in advance)
- Share copies a video's link (on hover over a result, or under the player)
- Background playback: audio runs in its own process and keeps going when the window is minimised
- Adaptive layout, down to a tiny window that shows only the playback controls
- Fullscreen, keyboard shortcuts (Settings lists them)

## What it needs at runtime

Pulse does not reimplement video decoding or YouTube. It drives three well-known tools:

| Tool | Used for | Install |
| --- | --- | --- |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | searching and finding the streams | downloaded by the app on first run (Settings) |
| [mpv](https://mpv.io) | audio playback, seeking, volume | `brew install mpv` / `winget install mpv` / `apt install mpv` |
| [ffmpeg](https://ffmpeg.org) | decoding video frames (video mode only) | `brew install ffmpeg` / `winget install ffmpeg` / `apt install ffmpeg` |

yt-dlp also needs a JavaScript runtime for YouTube nowadays: install [deno](https://deno.com) (or node).
The app checks all of this and tells you what is missing. Tools are found in the app data folder, next
to the executable (also in a `tools/` folder), on `PATH`, and in the usual Homebrew/winget/scoop locations.

## When YouTube breaks

Almost always the fix is a newer yt-dlp: **Settings → Update yt-dlp**. If the problem is deeper,
only [`src/provider/ytdlp.rs`](src/provider/ytdlp.rs) knows about YouTube.

### "Sign in to confirm you're not a bot"

YouTube sometimes refuses anonymous requests, more often from some networks, and for age-restricted
videos always. A newer yt-dlp does not help with this; being signed in does. Browser sign-in is **off by default**.
When YouTube refuses, the app shows **Sign in…**, which explains what will happen and lets you pick a
browser that has a YouTube login (or keep it off). Once on:

- Requests still go out anonymously first. Only when YouTube refuses does yt-dlp repeat the request with
  that browser's cookies (`--cookies-from-browser`). After a refusal it signs in straight away for 30 minutes.
- Cookies are only sent to YouTube. Pulse doesn't store or copy them.
- What the OS asks the first time:
  - **macOS:** Chrome, Brave, Edge and other Chromium browsers trigger a Keychain password prompt from
    `security` (macOS's keychain tool, run by yt-dlp) for "<Browser> Safe Storage". *Always Allow* stops it
    from asking again. Safari needs Full Disk Access. Firefox normally asks nothing.
  - **Windows:** Firefox works best. Chrome and Edge often lock their cookies away from other programs.
    Close the browser if the app says its cookies are locked.
  - **Linux:** Chromium browsers may ask to unlock the desktop keyring. Firefox asks nothing.
- If the browser's cookies can't be read, the message says why and offers another browser. If YouTube
  refuses even when signed in, that video is skipped and the playlist continues.
- YouTube can occasionally limit accounts it sees used with download tools. A second account in that browser
  avoids any risk to your main one. Turn it off any time in **Settings → YouTube sign-in**.

Check it from a terminal: `cargo run --release --example backend -- resolve chrome <video link>`.

## Architecture

```
src/provider/   trait Provider + plain data types; ytdlp.rs is the only YouTube-aware code
src/engine/     queue + playback thread: mpv (audio, JSON IPC), ffmpeg (RGBA frames), clock sync
src/platform/   everything OS-specific: macos.rs, linux.rs, windows.rs (unix.rs is shared by the
                first two). Paths, IPC socket vs named pipe, fonts, TLS, how helpers end with the app

src/app/        egui UI: layout tiers, controls, browse, queue, settings
```

- The UI only talks to `dyn Provider` and sends `Cmd`s to the engine; it never sees a URL or a flag.
- The engine runs on its own thread, so the queue keeps advancing while the window is hidden.
- Video frames are paced against mpv's audio clock; the decoder only runs while the picture is visible
  and at the size it is shown at, so RAM and CPU stay low. Audio-only mode starts no decoder at all.
  After a resize (or quality change) a second decoder starts slightly ahead of playback, and the old one keeps
  showing until the new one is in step, so the picture never stalls (`cargo run --release --example resize` measures it).
- The app downloads the sound and the picture itself, in bounded pieces, and hands them to mpv and ffmpeg
  through a small HTTP server on localhost (`engine/relay.rs`), because YouTube slows an open-ended download
  to about twice the stream's bitrate. That also measures the line, keeps the sound well ahead of the picture
  and stops video downloads while the sound runs low. On Auto, `engine/abr.rs` picks the stream sizes from that
  measurement: smaller ones before the buffer runs out, bigger ones once the line has had room for them a while.
  Live streams (HLS) bypass the relay.
- `scripts/netsim.py` simulates a slow or changing line; `cargo run --release --example network -- 10M@40,500k@40,0@10,10M@40`
  plays a video through it and reports start-up time, audio stalls, frozen picture and the qualities used.
- Swapping the backend (Invidious, Piped, ...) means implementing `Provider` and changing one line in `main.rs`.
- A build carries only its own OS's code. HTTPS goes through the system's TLS on macOS and Windows and
  through rustls on Linux. CI runs clippy on all three.
- Helpers never outlive the app, even after a crash or `kill`: mpv holds a connection to the app and quits
  when it closes, ffmpeg exits when its output pipe closes, and on Windows all helpers sit in a job that
  Windows ends with the app.

## Build

```sh
cargo run --release            # run
scripts/build-macos.sh local   # Pulse.app + .dmg for this Mac (arm64|x86_64|universal|all also work)
scripts/build-linux.sh         # .tar.gz (+ .deb with cargo-deb)
powershell scripts\build-windows.ps1
```

Outputs land in `dist/`. Releases: Actions → **Release** → Run workflow (or push a `v*` tag). It builds
macOS (arm64 + universal dmg), Windows x64 (zip), Linux x86_64 + aarch64 (tar.gz + deb) and attaches them
to a GitHub release (draft by default).

macOS builds are ad-hoc signed, not notarized. After downloading, if macOS says the app is damaged:
`xattr -dr com.apple.quarantine /Applications/Pulse.app`. Set `MACOS_SIGN_IDENTITY` to sign properly.

The .dmg window (background, icon layout, volume icon) is written by [dmgbuild](https://github.com/dmgbuild/dmgbuild),
which the script runs through `uvx` or `pipx`; without either it falls back to a plain .dmg. The background
comes from `scripts/make-dmg-background.py`. It is light because Finder always draws the icon labels in black over a
background picture, even in dark mode.

The icon is generated by `scripts/make-icons.py` (needs Pillow; results are committed).
`assets/fonts/` holds egui's monospace font (Hack) cut down to Latin, punctuation and arrows; the
command is in `src/app/theme.rs`.

The build scripts remap this machine's paths (`--remap-path-prefix`), so release binaries do not
contain your home folder.

## Settings file

`config.json` in the OS config folder (`~/Library/Application Support/Pulse`, `%APPDATA%\Pulse`,
`~/.config/Pulse`). Besides what the UI saves, `ytdlp_args` (e.g. `["--proxy","socks5://127.0.0.1:1080"]`)
and `mpv_args` (e.g. a specific audio device) are passed through. `sign_in` is the browser picked in the
sign-in dialog (`null` = off).
