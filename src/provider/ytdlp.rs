//! Provider backed by the `yt-dlp` command-line tool.
//!
//! Everything that knows about YouTube URLs, yt-dlp flags or its JSON layout is in
//! this file. yt-dlp is updated by its maintainers within hours of a YouTube change,
//! so keeping it fresh (`update`) is the usual fix when playback stops working.

use super::*;
use crate::platform;
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long one yt-dlp call may take before it is killed. A lookup downloads
/// about 1 MB (mostly YouTube's player script, which yt-dlp does not keep), so
/// on a 100 kbit/s line it takes well over a minute; a dead line is caught
/// sooner by `--socket-timeout`.
const TIMEOUT: Duration = Duration::from_secs(300);
/// Signed-in calls can also wait on the OS asking the user to unlock the browser's cookies.
const SIGNED_IN_TIMEOUT: Duration = Duration::from_secs(330);
/// What a lookup downloads (920-950 KB measured; counted a little low, so the
/// first streams err small), and the time it takes apart from that (starting
/// yt-dlp, solving YouTube's challenges), for `Resolved::line_hint`.
const LOOKUP_BITS: f64 = 880.0 * 1024.0 * 8.0;
const LOOKUP_OVERHEAD: f64 = 2.5;
/// Lookups quicker than this say nothing useful about the line.
const LOOKUP_SLOW: Duration = Duration::from_secs(8);
/// QuickJS solves the challenges this much slower than deno or node (seconds).
const QUICKJS_EXTRA: f64 = 4.0;
/// Once YouTube has refused an anonymous request, sign in right away for this long
/// instead of paying for another refusal first.
const STAY_SIGNED_IN: Duration = Duration::from_secs(30 * 60);

pub struct YtDlp {
    extra_args: RwLock<Vec<String>>,
    path: RwLock<Option<PathBuf>>,
    sign_in: RwLock<Option<SignInSource>>,
    signed_in_until: Mutex<Option<Instant>>,
    /// One signed-in call at a time, so the OS asks about cookie access only once.
    sign_in_lock: Mutex<()>,
}

impl YtDlp {
    pub fn new(extra_args: Vec<String>) -> Self {
        let this = Self {
            extra_args: RwLock::new(extra_args),
            path: RwLock::new(None),
            sign_in: RwLock::new(None),
            signed_in_until: Mutex::new(None),
            sign_in_lock: Mutex::new(()),
        };
        this.relocate();
        this
    }

    fn relocate(&self) {
        let found = std::env::var_os("PULSE_YTDLP")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(managed_install)
            .or_else(|| platform::find_executable("yt-dlp"));
        *self.path.write().unwrap() = found;
    }

    fn binary(&self) -> Result<PathBuf> {
        self.path.read().unwrap().clone().ok_or_else(|| {
            ProviderError::new(ErrorKind::Missing, "yt-dlp is not installed. Open Settings to download it.")
        })
    }

    /// yt-dlp needs a JS runtime for YouTube. It only enables deno by default,
    /// so point it at node/bun when those are what the user has, or else at the
    /// QuickJS the full packages carry (tiny, but lookups take ~5 s longer).
    fn js_runtime_args() -> Vec<String> {
        if platform::find_executable("deno").is_some() {
            return Vec::new();
        }
        for (name, runtime) in [("node", "node"), ("bun", "bun"), ("qjs", "quickjs")] {
            if let Some(path) = platform::find_executable(name) {
                return vec!["--js-runtimes".into(), format!("{runtime}:{}", path.display())];
            }
        }
        Vec::new()
    }

    /// Runs yt-dlp anonymously; when YouTube refuses ("confirm you're not a bot")
    /// and the user picked a browser, runs it again with that browser's cookies.
    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let Some(source) = self.sign_in.read().unwrap().clone() else {
            return self.attempt(args, None);
        };
        let recently_refused = self.signed_in_until.lock().unwrap().is_some_and(|until| Instant::now() < until);
        if !recently_refused {
            match self.attempt(args, None) {
                Err(e) if e.kind == ErrorKind::Blocked => {}
                other => return other,
            }
        }
        let _one_at_a_time = self.sign_in_lock.lock().unwrap_or_else(|e| e.into_inner());
        let result = self.attempt(args, Some(&source));
        match &result {
            Ok(_) => *self.signed_in_until.lock().unwrap() = Some(Instant::now() + STAY_SIGNED_IN),
            Err(e) if e.kind == ErrorKind::Blocked => *self.signed_in_until.lock().unwrap() = None,
            Err(_) => {}
        }
        result
    }

    /// One call, retried once when the network hiccups.
    fn attempt(&self, args: &[&str], signed_in: Option<&SignInSource>) -> Result<Vec<u8>> {
        let started = Instant::now();
        match self.exec(args, signed_in) {
            Err(e) if e.kind == ErrorKind::Network && started.elapsed() < Duration::from_secs(20) => {
                std::thread::sleep(Duration::from_millis(1500));
                self.exec(args, signed_in)
            }
            other => other,
        }
    }

    fn exec(&self, args: &[&str], signed_in: Option<&SignInSource>) -> Result<Vec<u8>> {
        let mut cmd = Command::new(self.binary()?);
        cmd.args(["--ignore-config", "--encoding", "utf-8", "--socket-timeout", "15"]);
        match signed_in {
            // Warnings stay on: they say why cookies could not be read.
            Some(source) => cmd.args(["--cookies-from-browser", &source.id]),
            None => cmd.arg("--no-warnings"),
        };
        cmd.args(Self::js_runtime_args())
            .args(self.extra_args.read().unwrap().iter())
            .args(args)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        platform::quiet(&mut cmd);
        platform::own_process_group(&mut cmd);
        let child = cmd
            .spawn()
            .map_err(|e| ProviderError::new(ErrorKind::Missing, format!("could not start yt-dlp: {e}")))?;
        platform::end_with_app(&child);
        let timeout = if signed_in.is_some() { SIGNED_IN_TIMEOUT } else { TIMEOUT };
        let Some((status, stdout, stderr)) = wait_output(child, timeout) else {
            return Err(match signed_in {
                // Most likely the OS prompt for cookie access went unanswered.
                Some(source) => ProviderError::new(
                    ErrorKind::Blocked,
                    format!(
                        "Gave up waiting for permission to read {}\u{2019}s cookies. Answer the system prompt when it appears, or pick another browser.",
                        source.name
                    ),
                ),
                None => ProviderError::new(ErrorKind::Network, "YouTube did not answer in time. Try again."),
            });
        };
        if status.success() {
            Ok(stdout)
        } else {
            Err(classify_error(&String::from_utf8_lossy(&stderr), signed_in))
        }
    }

    fn run_json(&self, args: &[&str]) -> Result<Value> {
        let bytes = self.run(args)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::new(ErrorKind::BackendOutdated, format!("unreadable yt-dlp output: {e}")))
    }
}

impl Provider for YtDlp {
    fn parse_input(&self, text: &str) -> Input {
        parse_input(text)
    }

    fn search(&self, query: &str, kind: SearchKind, limit: usize) -> Result<Vec<SearchItem>> {
        let n = limit.to_string();
        let range = format!("1:{limit}");
        let json = match kind {
            SearchKind::Video => self.run_json(&["--flat-playlist", "-J", &format!("ytsearch{n}:{query}")])?,
            SearchKind::Playlist | SearchKind::Channel => {
                // `sp` is YouTube's search-filter parameter (playlists / channels).
                let sp = if kind == SearchKind::Playlist { "EgIQAw%3D%3D" } else { "EgIQAg%3D%3D" };
                let url = format!("https://www.youtube.com/results?search_query={}&sp={sp}", urlencode(query));
                self.run_json(&["--flat-playlist", "-J", "-I", &range, &url])?
            }
        };
        Ok(entries(&json)
            .iter()
            .filter_map(|e| match kind {
                SearchKind::Video => track_from_entry(e).map(SearchItem::Track),
                _ => container_from_entry(e).map(SearchItem::Container),
            })
            .collect())
    }

    fn open(&self, container_url: &str, limit: usize) -> Result<Listing> {
        let url = normalize_container_url(container_url);
        let range = format!("1:{limit}");
        let json = self.run_json(&["--flat-playlist", "-J", "-I", &range, &url])?;
        let tracks: Vec<Track> = entries(&json).iter().filter_map(track_from_entry).collect();
        let mut title = s(&json, "title").unwrap_or_default();
        // Channel tabs are titled "Name - Videos".
        if let Some(stripped) = title.strip_suffix(" - Videos") {
            title = stripped.to_string();
        }
        Ok(Listing {
            title,
            owner: s(&json, "channel").or_else(|| s(&json, "uploader")),
            tracks,
        })
    }

    fn resolve(&self, track: &Track) -> Result<Resolved> {
        let started = Instant::now();
        let json = self.run_json(&["-J", "--no-playlist", &track.url])?;
        let mut resolved = parse_resolved(&json, track)?;
        let quickjs = Self::js_runtime_args().iter().any(|a| a.starts_with("quickjs:"));
        let took = started.elapsed().as_secs_f64() - if quickjs { QUICKJS_EXTRA } else { 0.0 };
        if took > LOOKUP_SLOW.as_secs_f64() {
            resolved.line_hint = Some(LOOKUP_BITS / (took - LOOKUP_OVERHEAD).max(0.5));
        }
        Ok(resolved)
    }

    fn health(&self) -> Health {
        let backend = self
            .binary()
            .map_err(|e| e.message)
            .and_then(|bin| {
                let mut cmd = Command::new(bin);
                cmd.arg("--version").stdin(Stdio::null());
                platform::quiet(&mut cmd);
                let out = cmd.output().map_err(|e| e.to_string())?;
                Ok(format!("yt-dlp {}", String::from_utf8_lossy(&out.stdout).trim()))
            });
        let mut warnings = Vec::new();
        let legacy = platform::managed_bin_dir().join(platform::exe_name("yt-dlp"));
        if self.path.read().unwrap().as_ref() == Some(&legacy) {
            warnings.push("This yt-dlp build takes seconds to start on every request. Update yt-dlp to switch to the fast one.".to_string());
        }
        if ["deno", "node", "bun", "qjs"].iter().all(|n| platform::find_executable(n).is_none()) {
            warnings.push(
                "YouTube needs a JavaScript runtime for yt-dlp. Install deno (deno.com) or node, or only low quality streams may work."
                    .to_string(),
            );
        }
        Health { backend, warnings, can_update: true }
    }

    fn set_extra_args(&self, args: Vec<String>) {
        *self.extra_args.write().unwrap() = args;
    }

    fn sign_in_sources(&self) -> Vec<SignInSource> {
        platform::browsers()
            .into_iter()
            .map(|b| SignInSource { id: b.id.to_string(), name: b.name.to_string(), note: platform::browser_access_note(b.id, b.name) })
            .collect()
    }

    fn set_sign_in(&self, source: Option<String>) {
        let source = source.map(|id| {
            let name = platform::browsers().into_iter().find(|b| b.id == id).map(|b| b.name.to_string());
            SignInSource { name: name.unwrap_or_else(|| id.clone()), id, note: None }
        });
        *self.sign_in.write().unwrap() = source;
        *self.signed_in_until.lock().unwrap() = None;
    }

    fn update(&self) -> Result<String> {
        let (asset, exe) = platform::ytdlp_release();
        let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
        let net = |e: String| ProviderError::new(ErrorKind::Network, format!("download failed: {e}"));
        let fail = |e: String| ProviderError::new(ErrorKind::Other, format!("could not install yt-dlp: {e}"));

        // Each update unpacks into a new folder, so a yt-dlp that is running right
        // now (Windows locks its files) never has to be overwritten.
        let dir = platform::managed_bin_dir();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let target = dir.join(format!("yt-dlp-{stamp}"));
        let part = dir.join(format!("yt-dlp-{stamp}.part"));
        let archive = dir.join(format!("yt-dlp-{stamp}.zip.part"));
        std::fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
        let _ = std::fs::remove_dir_all(&part);

        // Straight to disk: the archive is tens of MB, which should not pass through RAM.
        let download = || -> Result<()> {
            let response = platform::http_agent(Some(Duration::from_secs(600))).get(&url).call().map_err(|e| net(e.to_string()))?;
            let mut file = std::fs::File::create(&archive).map_err(|e| fail(e.to_string()))?;
            std::io::copy(&mut response.into_body().into_reader().take(200 * 1024 * 1024), &mut file).map_err(|e| net(e.to_string()))?;
            let file = std::fs::File::open(&archive).map_err(|e| fail(e.to_string()))?;
            zip::ZipArchive::new(std::io::BufReader::new(file))
                .and_then(|mut zip| zip.extract(&part))
                .map_err(|e| fail(e.to_string()))
        };
        let unpacked = download();
        let _ = std::fs::remove_file(&archive);
        if let Err(e) = unpacked {
            let _ = std::fs::remove_dir_all(&part);
            return Err(e);
        }
        platform::make_executable(&part.join(exe)).map_err(|e| fail(e.to_string()))?;
        std::fs::rename(&part, &target).map_err(|e| fail(e.to_string()))?;
        self.relocate();
        // The first run also lets macOS finish scanning the new files.
        match self.health().backend {
            Ok(version) => {
                remove_old_installs(&target);
                Ok(format!("Updated: {version}"))
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&target);
                self.relocate();
                Err(fail(format!("it does not run: {e}")))
            }
        }
    }
}

/// The newest yt-dlp unpacked by `update`, in `<managed bin>/yt-dlp-<stamp>/`.
fn managed_install() -> Option<PathBuf> {
    let exe = platform::ytdlp_release().1;
    let mut installs: Vec<(u64, PathBuf)> = std::fs::read_dir(platform::managed_bin_dir())
        .ok()?
        .flatten()
        .filter_map(|e| {
            let stamp = e.file_name().to_str()?.strip_prefix("yt-dlp-")?.parse().ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    installs.sort_unstable_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    installs.into_iter().map(|(_, dir)| dir.join(exe)).find(|p| p.is_file())
}

/// Best effort: files still in use (Windows) are left for the next update.
fn remove_old_installs(keep: &std::path::Path) {
    let dir = platform::managed_bin_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path == keep || !name.starts_with("yt-dlp") {
            continue;
        }
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

// --- input parsing -----------------------------------------------------------

struct Link {
    host: String,
    path: String,
    query: Vec<(String, String)>,
}

impl Link {
    fn param(&self, key: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

fn parse_link(text: &str) -> Option<Link> {
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .or_else(|| {
            let lower = text.to_ascii_lowercase();
            (lower.starts_with("youtube.com/")
                || lower.starts_with("www.youtube.com/")
                || lower.starts_with("music.youtube.com/")
                || lower.starts_with("m.youtube.com/")
                || lower.starts_with("youtu.be/"))
            .then_some(text)
        })?;
    let rest = rest.split('#').next().unwrap_or(rest);
    let (host_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = host_path.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((host_path, "/".into()));
    let host = host.to_ascii_lowercase();
    let host = ["www.", "m.", "music."].iter().fold(host.as_str(), |h, p| h.strip_prefix(p).unwrap_or(h)).to_string();
    let query = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Some(Link { host, path, query })
}

fn video_track(id: &str) -> Track {
    Track {
        id: id.to_string(),
        url: format!("https://www.youtube.com/watch?v={id}"),
        title: format!("youtube.com/watch?v={id}"),
        channel: None,
        duration: None,
        views: None,
        thumbnail: Some(thumbnail_for(id)),
        live: false,
    }
}

fn parse_input(text: &str) -> Input {
    let text = text.trim();
    if let Some(handle) = text.strip_prefix('@') {
        if !handle.is_empty() && !handle.contains(char::is_whitespace) {
            return Input::Channel { url: format!("https://www.youtube.com/@{handle}/videos") };
        }
    }
    let Some(link) = parse_link(text) else {
        return Input::Search(text.to_string());
    };
    let is_youtube = matches!(link.host.as_str(), "youtube.com" | "youtu.be" | "youtube-nocookie.com");
    if !is_youtube {
        // Any other site yt-dlp understands: treat it as one playable link.
        let mut track = video_track("");
        track.id = format!("url-{:x}", fnv(text));
        track.url = text.to_string();
        track.title = text.to_string();
        track.thumbnail = None;
        return Input::Track(track);
    }
    let first_segment = link.path.trim_matches('/').split('/').next().unwrap_or("").to_string();
    let second_segment = link.path.trim_matches('/').split('/').nth(1).unwrap_or("").to_string();
    // Auto-generated "mix" lists (RD...) are endless; play only the video then.
    let list = link.param("list").filter(|l| !l.starts_with("RD") && !l.starts_with("UL"));

    if link.host == "youtu.be" && !first_segment.is_empty() {
        return match list {
            Some(l) => Input::Playlist { url: playlist_url(l), focus: Some(first_segment) },
            None => Input::Track(video_track(&first_segment)),
        };
    }
    match first_segment.as_str() {
        "watch" => match (link.param("v"), list) {
            (Some(v), Some(l)) => Input::Playlist { url: playlist_url(l), focus: Some(v.to_string()) },
            (Some(v), None) => Input::Track(video_track(v)),
            (None, Some(l)) => Input::Playlist { url: playlist_url(l), focus: None },
            _ => Input::Search(text.to_string()),
        },
        "playlist" => match list.or(link.param("list")) {
            Some(l) => Input::Playlist { url: playlist_url(l), focus: None },
            None => Input::Search(text.to_string()),
        },
        "shorts" | "live" | "embed" | "v" if !second_segment.is_empty() => Input::Track(video_track(&second_segment)),
        s if s.starts_with('@') => Input::Channel { url: normalize_container_url(&format!("https://www.youtube.com{}", link.path)) },
        "channel" | "c" | "user" => Input::Channel { url: normalize_container_url(&format!("https://www.youtube.com{}", link.path)) },
        _ => Input::Search(text.to_string()),
    }
}

fn playlist_url(list_id: &str) -> String {
    format!("https://www.youtube.com/playlist?list={list_id}")
}

/// Channel pages are tabbed; we want the uploads list.
fn normalize_container_url(url: &str) -> String {
    let Some(link) = parse_link(url) else { return url.to_string() };
    let path = link.path.trim_end_matches('/');
    let is_channel = path.starts_with("/@") || ["/channel/", "/c/", "/user/"].iter().any(|p| path.starts_with(p));
    let has_tab = ["/videos", "/streams", "/shorts", "/playlists", "/featured", "/releases"].iter().any(|t| path.ends_with(t));
    if is_channel && !has_tab {
        format!("https://www.youtube.com{path}/videos")
    } else {
        url.to_string()
    }
}

fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

// --- JSON helpers --------------------------------------------------------------

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|x| !x.is_empty()).map(str::to_string)
}

fn n(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)))
}

fn fl(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn entries(json: &Value) -> Vec<Value> {
    json.get("entries").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn track_from_entry(e: &Value) -> Option<Track> {
    let id = s(e, "id")?;
    let title = s(e, "title")?;
    // Placeholders for removed videos in playlists.
    if matches!(title.as_str(), "[Private video]" | "[Deleted video]" | "[Unavailable]") {
        return None;
    }
    // Playlists/channels can show up in channel tabs; they are not tracks.
    if e.get("ie_key").and_then(Value::as_str) == Some("YoutubeTab") {
        return None;
    }
    let url = s(e, "url")
        .filter(|u| u.starts_with("http"))
        .or_else(|| s(e, "webpage_url"))
        .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));
    Some(Track {
        thumbnail: Some(thumbnail_for(&id)),
        url,
        id,
        title,
        channel: s(e, "channel").or_else(|| s(e, "uploader")),
        duration: n(e, "duration"),
        views: n(e, "view_count"),
        live: s(e, "live_status").as_deref() == Some("is_live"),
    })
}

fn container_from_entry(e: &Value) -> Option<Container> {
    let id = s(e, "id")?;
    let url = s(e, "url")?;
    let title = s(e, "title")?;
    let kind = if url.contains("/playlist?list=") { ContainerKind::Playlist } else { ContainerKind::Channel };
    let thumbnail = (kind == ContainerKind::Playlist)
        .then(|| {
            e.get("thumbnails")
                .and_then(Value::as_array)
                .and_then(|t| t.iter().filter_map(|t| s(t, "url")).find(|u| u.contains("ytimg.com")))
        })
        .flatten();
    Some(Container {
        kind,
        id,
        url,
        title,
        owner: s(e, "channel").or_else(|| s(e, "uploader")),
        subscribers: n(e, "channel_follower_count"),
        thumbnail,
    })
}

// --- resolving streams -----------------------------------------------------------

fn headers_of(format: &Value) -> Vec<Header> {
    format
        .get("http_headers")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| Header { name: k.clone(), value: v.to_string() }))
                .collect()
        })
        .unwrap_or_default()
}

fn language_preference(f: &Value) -> i64 {
    f.get("language_preference").and_then(Value::as_i64).unwrap_or(-1)
}

fn bitrate(f: &Value) -> Option<f64> {
    fl(f, "tbr").or_else(|| fl(f, "abr")).or_else(|| fl(f, "vbr")).filter(|r| *r > 0.0)
}

/// The "stable volume" remix of a track, which squashes its dynamics.
fn is_drc(f: &Value) -> bool {
    s(f, "format_id").is_some_and(|id| id.ends_with("-drc")) || f.get("has_drc").and_then(Value::as_bool) == Some(true)
}

/// Keeps the formats that pass `test`, unless none would be left.
fn keep_if(formats: Vec<&Value>, test: impl Fn(&Value) -> bool) -> Vec<&Value> {
    if formats.iter().any(|f| test(f)) {
        formats.into_iter().filter(|f| test(f)).collect()
    } else {
        formats
    }
}

/// The sound in each size there is, smallest first: the original language, in
/// stereo (surround is mixed down anyway and costs three times as much), as
/// recorded rather than volume-levelled, from single files. Of two streams of
/// about the same size, opus (better per bit) stays.
fn audio_ladder(formats: &[&Value]) -> Vec<AudioStream> {
    let audio: Vec<&Value> = formats.iter().copied().filter(|f| is_audio_only(f)).collect();
    let best_language = audio.iter().map(|f| language_preference(f)).max().unwrap_or(-1);
    let audio = audio.into_iter().filter(|f| language_preference(f) == best_language).collect();
    let audio = keep_if(audio, |f| n(f, "audio_channels").is_none_or(|c| c <= 2));
    let audio = keep_if(audio, |f| !is_drc(f));
    let mut audio = keep_if(audio, |f| !segmented(f));
    if audio.iter().all(|f| bitrate(f).is_none()) {
        // Sizes unknown (live HLS): the last listed is the best.
        audio.drain(..audio.len().saturating_sub(1));
    }
    audio.sort_by(|a, b| bitrate(a).unwrap_or(0.0).total_cmp(&bitrate(b).unwrap_or(0.0)));
    let mut ladder: Vec<&Value> = Vec::new();
    for f in audio {
        match ladder.last() {
            Some(prev) if bitrate(f).unwrap_or(0.0) < bitrate(prev).unwrap_or(0.0) * 1.15 => {
                if s(f, "acodec").is_some_and(|c| c.starts_with("opus")) {
                    *ladder.last_mut().unwrap() = f;
                }
            }
            _ => ladder.push(f),
        }
    }
    ladder.into_iter().map(audio_stream).collect()
}

fn audio_stream(fm: &Value) -> AudioStream {
    let rate = bitrate(fm);
    AudioStream {
        url: s(fm, "url").unwrap(),
        headers: headers_of(fm),
        label: format!(
            "{} {}",
            s(fm, "acodec").map(|c| c.split('.').next().unwrap_or("").to_string()).unwrap_or_default(),
            rate.map(|a| format!("{}k", a.round() as u64)).unwrap_or_default()
        )
        .trim()
        .to_string(),
        bitrate: rate.map(|r| r.round() as u32).unwrap_or(0),
        size: file_size(fm),
    }
}

/// googlevideo links to single files state their size (`clen=`) and serve any
/// byte range of them.
fn file_size(f: &Value) -> Option<u64> {
    let url = s(f, "url")?;
    let (_, rest) = url.split_once("?clen=").or_else(|| url.split_once("&clen="))?;
    rest.split('&').next()?.parse().ok().filter(|_| !segmented(f))
}

/// A rough bitrate for a video stream that does not say (kbit/s).
fn guess_bitrate(height: u32) -> u32 {
    match height {
        0..=144 => 100,
        145..=240 => 250,
        241..=360 => 500,
        361..=480 => 900,
        481..=720 => 1800,
        721..=1080 => 3500,
        1081..=1440 => 7000,
        _ => 15000,
    }
}

/// HLS: a playlist of short files. YouTube cuts off a download that has not
/// been read for about 15 seconds, which every pause longer than that does.
/// mpv and ffmpeg pick a single file up again where it broke off, but they skip
/// the rest of a cut segment, which freezes the picture for seconds.
fn segmented(f: &Value) -> bool {
    f.get("protocol").and_then(Value::as_str).is_some_and(|p| p.starts_with("m3u8"))
}

/// When a stream link stops working: googlevideo links carry it as `expire=`
/// (or `/expire/<secs>/` in HLS playlist links), in Unix seconds.
fn link_expiry(url: &str) -> Option<u64> {
    let (_, rest) = url.split_once("?expire=").or_else(|| url.split_once("&expire=")).or_else(|| url.split_once("/expire/"))?;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).map_or(rest, |end| &rest[..end]);
    digits.parse().ok()
}

fn codec_rank(codec: &str) -> u8 {
    // Cheapest to decode in software first.
    if codec.starts_with("avc1") {
        0
    } else if codec.starts_with("vp9") || codec.starts_with("vp09") {
        1
    } else if codec.starts_with("av01") {
        2
    } else {
        3
    }
}

fn has_codec(format: &Value, key: &str) -> bool {
    format.get(key).and_then(Value::as_str).is_some_and(|c| c != "none")
}

/// Sound without picture. yt-dlp leaves the audio codec unset for some streams
/// (live HLS audio), so "no video, audio not ruled out" counts too.
fn is_audio_only(format: &Value) -> bool {
    let codec = |key: &str| format.get(key).and_then(Value::as_str);
    codec("vcodec") == Some("none") && codec("acodec") != Some("none")
}

fn parse_resolved(json: &Value, requested: &Track) -> Result<Resolved> {
    let live = json.get("is_live").and_then(Value::as_bool).unwrap_or(false);
    let id = s(json, "id").unwrap_or_else(|| requested.id.clone());
    let track = Track {
        thumbnail: requested.thumbnail.clone().or_else(|| s(json, "thumbnail")),
        id,
        url: requested.url.clone(),
        title: s(json, "title").unwrap_or_else(|| requested.title.clone()),
        channel: s(json, "channel").or_else(|| s(json, "uploader")).or_else(|| requested.channel.clone()),
        duration: n(json, "duration").or(requested.duration),
        views: n(json, "view_count").or(requested.views),
        live,
    };

    let formats: Vec<&Value> = json
        .get("formats")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let usable: Vec<&Value> = formats
        .into_iter()
        .filter(|f| s(f, "url").is_some())
        .filter(|f| f.get("vcodec").and_then(Value::as_str) != Some("images"))
        .filter(|f| {
            // Skip DASH fragment lists and other things mpv/ffmpeg can't open from one URL.
            matches!(f.get("protocol").and_then(Value::as_str), Some("https" | "http" | "m3u8" | "m3u8_native") | None)
        })
        .collect();

    // Audio: the audio-only streams; otherwise the smallest muxed one.
    let mut audio = audio_ladder(&usable);
    if audio.is_empty() {
        if let Some(fm) = usable.iter().filter(|f| has_codec(f, "acodec")).min_by_key(|f| n(f, "height").unwrap_or(u64::MAX)) {
            audio.push(audio_stream(fm));
        } else if let Some(url) = s(json, "url") {
            // A direct media link without a format list.
            audio.push(AudioStream { url, headers: headers_of(json), label: String::new(), bitrate: 0, size: None });
        } else {
            return Err(ProviderError::new(ErrorKind::Unavailable, "no playable audio stream found"));
        }
    }

    // Video: video-only streams when they exist, single files over HLS, no more
    // than the 30 frames per second the app shows, and SDR. Per height, the cheapest
    // codec to decode (highest bitrate of it), plus the smallest stream in another
    // codec when that saves a good part: on a slow connection a taller picture in
    // VP9 beats a shorter one in H.264. AV1 only when there is nothing else (it is
    // the heaviest to decode, and not every ffmpeg can).
    let any_video_only = usable.iter().any(|f| has_codec(f, "vcodec") && !has_codec(f, "acodec"));
    let mut by_height: std::collections::BTreeMap<u32, Vec<&Value>> = std::collections::BTreeMap::new();
    for fm in usable.iter().filter(|f| has_codec(f, "vcodec") && (!any_video_only || !has_codec(f, "acodec"))) {
        if let Some(height) = n(fm, "height") {
            by_height.entry(height as u32).or_default().push(fm);
        }
    }
    let vcodec = |f: &Value| f.get("vcodec").and_then(Value::as_str).unwrap_or("").to_string();
    let mut video: Vec<VideoStream> = Vec::new();
    for (height, formats) in by_height {
        let formats = keep_if(formats, |f| !segmented(f));
        let formats = keep_if(formats, |f| fl(f, "fps").is_none_or(|fps| fps <= 31.0));
        // HDR would need tone mapping to look right on the way to RGB.
        let formats = keep_if(formats, |f| s(f, "dynamic_range").is_none_or(|d| d == "SDR"));
        let rate = |f: &Value| bitrate(f).unwrap_or(0.0);
        let Some(cheap) = formats.iter().copied().min_by(|a, b| {
            (codec_rank(&vcodec(a)), -rate(a)).partial_cmp(&(codec_rank(&vcodec(b)), -rate(b))).unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            continue;
        };
        let small = formats
            .iter()
            .copied()
            .filter(|f| codec_rank(&vcodec(f)) <= 1 && bitrate(f).is_some())
            .min_by(|a, b| rate(a).total_cmp(&rate(b)))
            .filter(|f| bitrate(cheap).is_some_and(|c| rate(f) < c * 0.8));
        for fm in small.into_iter().chain([cheap]) {
            video.push(VideoStream {
                url: s(fm, "url").unwrap(),
                headers: headers_of(fm),
                width: n(fm, "width").map(|w| w as u32).unwrap_or(height * 16 / 9),
                height,
                fps: fl(fm, "fps").unwrap_or(30.0) as f32,
                codec: s(fm, "vcodec").map(|c| c.split('.').next().unwrap_or("").to_string()).unwrap_or_default(),
                segmented: segmented(fm),
                bitrate: bitrate(fm).map(|r| r.round() as u32).unwrap_or_else(|| guess_bitrate(height)),
                size: file_size(fm),
            });
        }
    }

    let ready_at = usable
        .iter()
        .filter_map(|f| n(f, "available_at"))
        .max()
        .map(|secs| std::time::UNIX_EPOCH + Duration::from_secs(secs));
    let expires_at = audio
        .iter()
        .map(|a| &a.url)
        .chain(video.iter().map(|v| &v.url))
        .filter_map(|url| link_expiry(url))
        .min()
        .map(|secs| std::time::UNIX_EPOCH + Duration::from_secs(secs));
    Ok(Resolved { track, audio, video, ready_at, expires_at, line_hint: None })
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// Like `Child::wait_with_output`, but gives up (and kills the process tree) after `timeout`.
fn wait_output(mut child: Child, timeout: Duration) -> Option<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some((status, stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default())),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                platform::kill_tree(&mut child);
                // The reader threads end once the pipes close; don't wait on them.
                return None;
            }
        }
    }
}

fn classify_error(stderr: &str, signed_in: Option<&SignInSource>) -> ProviderError {
    let error = classify_plain(stderr);
    let lower = stderr.to_ascii_lowercase();
    match signed_in {
        None if error.kind == ErrorKind::Blocked => ProviderError::new(
            ErrorKind::Blocked,
            if lower.contains("your age") {
                "This video is age-restricted. Signing in through your browser lets you watch it."
            } else {
                "YouTube wants to confirm you\u{2019}re not a bot. Signing in through your browser gets past this."
            },
        ),
        Some(source) if matches!(error.kind, ErrorKind::Blocked | ErrorKind::Other) => {
            if let Some(problem) = cookie_problem(&lower, source) {
                ProviderError::new(ErrorKind::Blocked, problem)
            } else if error.kind == ErrorKind::Blocked {
                // Signing in did not help; nothing more to try for this video.
                ProviderError::new(
                    ErrorKind::Unavailable,
                    format!(
                        "YouTube refused this video even with your {} sign-in. Check that you\u{2019}re signed in to YouTube in {0}, or try again later.",
                        source.name
                    ),
                )
            } else {
                error
            }
        }
        _ => error,
    }
}

/// Why yt-dlp could not use the browser's cookies, if that is what went wrong.
fn cookie_problem(lower: &str, source: &SignInSource) -> Option<String> {
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    let name = &source.name;
    if has(&["could not copy", "database is locked"]) {
        Some(format!("{name} is holding its cookies locked. Close {name} and try again, or pick another browser."))
    } else if has(&["operation not permitted", "permission denied", "full disk access"]) {
        Some(format!(
            "{} is not allowed to read {name}\u{2019}s cookies. {} Or pick another browser.",
            platform::APP_NAME,
            platform::cookie_permission_hint()
        ))
    } else if has(&["decrypt", "no key found", "keyring", "keychain", "app-bound", "dpapi", "unsupported browser"])
        || lower.contains(&format!("could not find {} cookies", source.id))
        || lower.contains("cookies database")
    {
        Some(format!(
            "Could not read {name}\u{2019}s cookies. Allow access when your system asks, or pick another browser{}.",
            if source.id == "firefox" { "" } else { " (Firefox works everywhere)" }
        ))
    } else {
        None
    }
}

fn classify_plain(stderr: &str) -> ProviderError {
    let text = stderr.trim();
    let last = text
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .or_else(|| text.lines().rev().find(|l| !l.starts_with('[') && !l.starts_with("WARNING")))
        .unwrap_or("yt-dlp failed");
    let message = last.trim_start_matches("ERROR:").trim().to_string();
    let lower = text.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    // The block check comes first: its message also mentions "cookies".
    let kind = if has(&["sign in to confirm", "confirm you're not a bot", "confirm you\u{2019}re not a bot"]) {
        ErrorKind::Blocked
    } else if has(&["getaddrinfo", "urlopen error", "timed out", "temporary failure in name resolution", "network is unreachable", "connection reset", "unable to download webpage"]) && !has(&["http error 4"]) {
        ErrorKind::Network
    } else if has(&["login required", "use --cookies"]) {
        ErrorKind::Blocked
    } else if has(&["video unavailable", "private video", "this video is not available", "has been removed", "not available in your country", "members-only", "live stream recording is not available", "premieres in"]) {
        ErrorKind::Unavailable
    } else if has(&["unable to extract", "nsig", "http error 403", "requested format is not available", "unsupported url", "signature", "player response", "challenge", "js runtime", "unexpected"]) {
        ErrorKind::BackendOutdated
    } else {
        ErrorKind::Other
    };
    ProviderError::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_input() {
        assert!(matches!(parse_input("lofi beats"), Input::Search(_)));
        assert!(matches!(parse_input("https://youtu.be/dQw4w9WgXcQ?t=4"), Input::Track(t) if t.id == "dQw4w9WgXcQ"));
        assert!(matches!(parse_input("https://www.youtube.com/watch?v=abc&list=PL123"), Input::Playlist { focus: Some(f), .. } if f == "abc"));
        assert!(matches!(parse_input("https://www.youtube.com/watch?v=abc&list=RDabc"), Input::Track(_)));
        assert!(matches!(parse_input("https://music.youtube.com/playlist?list=PL9"), Input::Playlist { .. }));
        assert!(matches!(parse_input("https://www.youtube.com/@LofiGirl"), Input::Channel { url } if url.ends_with("/@LofiGirl/videos")));
        assert!(matches!(parse_input("@LofiGirl"), Input::Channel { .. }));
        assert!(matches!(parse_input("https://www.youtube.com/shorts/xyz"), Input::Track(t) if t.id == "xyz"));
        assert!(matches!(parse_input("youtube.com/watch?v=q"), Input::Track(_)));
    }

    #[test]
    fn classifies_blocks() {
        let bot = "ERROR: [youtube] dk1OY5z08_Y: Sign in to confirm you\u{2019}re not a bot. This helps protect our community. Use --cookies-from-browser or --cookies for the authentication.";
        let chrome = SignInSource { id: "chrome".into(), name: "Chrome".into(), note: None };
        let safari = SignInSource { id: "safari".into(), name: "Safari".into(), note: None };

        let anon = classify_error(bot, None);
        assert_eq!(anon.kind, ErrorKind::Blocked);
        assert!(anon.message.starts_with("YouTube wants to confirm"));
        assert!(classify_error("ERROR: [youtube] x: Sign in to confirm your age.", None).message.contains("age-restricted"));

        // Signed in and still refused: give up on this video (lets playlists move on).
        assert_eq!(classify_error(bot, Some(&chrome)).kind, ErrorKind::Unavailable);

        // Signed in but the cookies could not be read: say why, offer another browser.
        let denied = format!("WARNING: cannot decrypt v10 cookies: no key found\n{bot}");
        let e = classify_error(&denied, Some(&chrome));
        assert_eq!(e.kind, ErrorKind::Blocked);
        assert!(e.message.starts_with("Could not read Chrome"));
        let locked = "ERROR: Could not copy Chrome cookie database. See  https://github.com/yt-dlp/yt-dlp/issues/7271";
        assert!(classify_error(locked, Some(&chrome)).message.contains("Close Chrome"));
        let fda = "ERROR: [Errno 1] Operation not permitted: '/Users/x/Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies'";
        assert!(classify_error(fda, Some(&safari)).message.contains("not allowed to read Safari"));

        // Other failures keep their kind when signed in.
        assert_eq!(classify_error("ERROR: [youtube] x: Video unavailable", Some(&chrome)).kind, ErrorKind::Unavailable);
        assert_eq!(classify_error("ERROR: unable to download webpage: timed out", None).kind, ErrorKind::Network);
    }

    #[test]
    fn resolves_live_streams() {
        // Live HLS as YouTube serves it now: separate audio without a known codec.
        let json: Value = serde_json::from_str(
            r#"{"id":"x","title":"Live","is_live":true,"formats":[
                {"format_id":"233","protocol":"m3u8_native","url":"https://a/233","acodec":null,"vcodec":"none","format_note":"Default, low"},
                {"format_id":"234","protocol":"m3u8_native","url":"https://a/234","acodec":null,"vcodec":"none","format_note":"Default, high"},
                {"format_id":"232","protocol":"m3u8_native","url":"https://a/232","acodec":"none","vcodec":"avc1.4D401F","height":720,"width":1280},
                {"format_id":"270","protocol":"m3u8_native","url":"https://a/270","acodec":"none","vcodec":"avc1.640028","height":1080,"width":1920}]}"#,
        )
        .unwrap();
        let track = parse_input("https://youtu.be/dQw4w9WgXcQ");
        let Input::Track(track) = track else { panic!() };
        let r = parse_resolved(&json, &track).unwrap();
        assert!(r.track.live);
        assert_eq!(r.audio.iter().map(|a| a.url.as_str()).collect::<Vec<_>>(), ["https://a/234"]);
        assert_eq!(r.video.iter().map(|v| v.height).collect::<Vec<_>>(), [720, 1080]);
        assert!(r.video.iter().all(|v| v.segmented));
        // Live HLS lists no bitrates; the guesses still rank the heights.
        assert!(r.video[0].bitrate < r.video[1].bitrate);
    }

    #[test]
    fn builds_stream_ladders() {
        // As YouTube lists Big Buck Bunny: stereo and 5.1 sound, "stable volume"
        // remixes, 30 and 60 fps pictures.
        let json: Value = serde_json::from_str(
            r#"{"id":"x","title":"Video","duration":635,"formats":[
                {"format_id":"249-drc","protocol":"https","url":"https://r/249d","acodec":"opus","vcodec":"none","abr":49.8,"tbr":49.8,"audio_channels":2},
                {"format_id":"139","protocol":"https","url":"https://r/139","acodec":"mp4a.40.5","vcodec":"none","abr":48.8,"tbr":48.8,"audio_channels":2},
                {"format_id":"249","protocol":"https","url":"https://r/249","acodec":"opus","vcodec":"none","abr":49.6,"tbr":49.6,"audio_channels":2},
                {"format_id":"250","protocol":"https","url":"https://r/250","acodec":"opus","vcodec":"none","abr":65.4,"tbr":65.4,"audio_channels":2},
                {"format_id":"140","protocol":"https","url":"https://r/140","acodec":"mp4a.40.2","vcodec":"none","abr":129.5,"tbr":129.5,"audio_channels":2},
                {"format_id":"251","protocol":"https","url":"https://r/251","acodec":"opus","vcodec":"none","abr":128.6,"tbr":128.6,"audio_channels":2},
                {"format_id":"258","protocol":"https","url":"https://r/258","acodec":"mp4a.40.2","vcodec":"none","abr":387.9,"tbr":387.9,"audio_channels":6},
                {"format_id":"160","protocol":"https","url":"https://r/160","acodec":"none","vcodec":"avc1.4D400C","height":144,"width":256,"fps":30,"tbr":54.5},
                {"format_id":"278","protocol":"https","url":"https://r/278","acodec":"none","vcodec":"vp9","height":144,"width":256,"fps":30,"tbr":66.7},
                {"format_id":"136","protocol":"https","url":"https://r/136","acodec":"none","vcodec":"avc1.4D401F","height":720,"width":1280,"fps":30,"tbr":1100.5},
                {"format_id":"247","protocol":"https","url":"https://r/247","acodec":"none","vcodec":"vp9","height":720,"width":1280,"fps":30,"tbr":709.5},
                {"format_id":"298","protocol":"https","url":"https://r/298","acodec":"none","vcodec":"avc1.4D4020","height":720,"width":1280,"fps":60,"tbr":1897.7},
                {"format_id":"398","protocol":"https","url":"https://r/398","acodec":"none","vcodec":"av01.0.08M.08","height":720,"width":1280,"fps":60,"tbr":898.7},
                {"format_id":"299","protocol":"https","url":"https://r/299","acodec":"none","vcodec":"avc1.64002A","height":1080,"width":1920,"fps":60,"tbr":3247.8},
                {"format_id":"303","protocol":"https","url":"https://r/303","acodec":"none","vcodec":"vp9","height":1080,"width":1920,"fps":60,"tbr":2127.3}]}"#,
        )
        .unwrap();
        let Input::Track(track) = parse_input("https://youtu.be/dQw4w9WgXcQ") else { panic!() };
        let r = parse_resolved(&json, &track).unwrap();
        let audio: Vec<_> = r.audio.iter().map(|a| (a.url.rsplit('/').next().unwrap(), a.bitrate)).collect();
        assert_eq!(audio, [("249", 50), ("250", 65), ("251", 129)]);
        let video: Vec<_> = r.video.iter().map(|v| (v.height, v.url.rsplit('/').next().unwrap())).collect();
        // 144p: H.264 is the smaller one there. 720p: 30 fps only. 1080p: 60 fps is all there is.
        assert_eq!(video, [(144, "160"), (720, "247"), (720, "136"), (1080, "303"), (1080, "299")]);
    }

    #[test]
    fn prefers_single_files() {
        // A video as YouTube serves it now: each stream both as HLS and as one file.
        let json: Value = serde_json::from_str(
            r#"{"id":"x","title":"Video","duration":3613,"formats":[
                {"format_id":"234","protocol":"m3u8_native","url":"https://m/hls_playlist/expire/1792771722/itag/234/index.m3u8","acodec":"mp4a.40.2","vcodec":"none","tbr":200},
                {"format_id":"251","protocol":"https","url":"https://r/videoplayback?expire=1792771700&itag=251","acodec":"opus","vcodec":"none","abr":147},
                {"format_id":"270","protocol":"m3u8_native","url":"https://m/hls_playlist/expire/1792771722/itag/270/index.m3u8","acodec":"none","vcodec":"avc1.640028","height":1080,"width":1920,"tbr":5925},
                {"format_id":"137","protocol":"https","url":"https://r/videoplayback?expire=1792771722&itag=137","acodec":"none","vcodec":"avc1.640028","height":1080,"width":1920,"tbr":3481},
                {"format_id":"248","protocol":"https","url":"https://r/videoplayback?expire=1792771722&itag=248","acodec":"none","vcodec":"vp9","height":1080,"width":1920,"tbr":1934}]}"#,
        )
        .unwrap();
        let Input::Track(track) = parse_input("https://youtu.be/dQw4w9WgXcQ") else { panic!() };
        let r = parse_resolved(&json, &track).unwrap();
        assert!(r.audio.iter().all(|a| !a.url.contains("hls_playlist")));
        assert!(r.audio.last().unwrap().url.ends_with("itag=251"));
        // H.264, and VP9 at little more than half its size.
        let video: Vec<_> = r.video.iter().map(|v| (v.height, v.codec.as_str(), v.segmented)).collect();
        assert_eq!(video, [(1080, "vp9", false), (1080, "avc1", false)]);
        assert!(r.video[1].url.ends_with("itag=137"));
        assert_eq!(r.expires_at, Some(std::time::UNIX_EPOCH + Duration::from_secs(1792771700)));
    }

    #[test]
    fn prefers_sdr() {
        let json: Value = serde_json::from_str(
            r#"{"id":"x","title":"Video","formats":[
                {"format_id":"251","protocol":"https","url":"https://r/251","acodec":"opus","vcodec":"none","abr":147},
                {"format_id":"271","protocol":"https","url":"https://r/271","acodec":"none","vcodec":"vp9","height":1440,"width":2560,"tbr":9000,"dynamic_range":"SDR"},
                {"format_id":"337","protocol":"https","url":"https://r/337","acodec":"none","vcodec":"vp09.02.51.10.01.09.16.09.00","height":1440,"width":2560,"tbr":12000,"dynamic_range":"HDR10"}]}"#,
        )
        .unwrap();
        let Input::Track(track) = parse_input("https://youtu.be/dQw4w9WgXcQ") else { panic!() };
        let urls: Vec<_> = parse_resolved(&json, &track).unwrap().video.into_iter().map(|v| v.url).collect();
        assert_eq!(urls, ["https://r/271"]);
    }

    #[test]
    fn reads_file_sizes() {
        let size = |url: &str, protocol: &str| file_size(&serde_json::json!({ "url": url, "protocol": protocol }));
        assert_eq!(size("https://r/videoplayback?expire=1&clen=87293859&dur=634.5", "https"), Some(87293859));
        assert_eq!(size("https://r/videoplayback?clen=12", "https"), Some(12));
        assert_eq!(size("https://r/videoplayback?expire=1", "https"), None);
        assert_eq!(size("https://m/index.m3u8?clen=5", "m3u8_native"), None);
    }

    #[test]
    fn reads_link_expiry() {
        assert_eq!(link_expiry("https://r/videoplayback?expire=1792771722&ei=x"), Some(1792771722));
        assert_eq!(link_expiry("https://r/videoplayback?ei=x&expire=1792771722"), Some(1792771722));
        assert_eq!(link_expiry("https://m/api/manifest/hls_playlist/expire/1792769665/ei/x/index.m3u8"), Some(1792769665));
        assert_eq!(link_expiry("https://r/videoplayback?noexpire=5"), None);
        assert_eq!(link_expiry("https://example.com/a.mp4"), None);
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }
}
