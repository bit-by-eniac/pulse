//! Audio output and transport through an `mpv` child process, controlled over
//! mpv's JSON IPC.
//!
//! Two connections are used on purpose. One only reads events (property changes);
//! the other only writes plain-text commands, which mpv answers with nothing. That
//! way no thread ever blocks on a pipe it is also writing to, which matters for
//! Windows named pipes.

use crate::platform::{self, IpcConn};
use serde_json::Value;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug)]
pub enum PlayerEvent {
    TimePos(f64),
    Duration(f64),
    Pause(bool),
    CoreIdle(bool),
    Buffering(bool),
    /// The sound buffered ahead: `ahead` seconds, `done` when it reaches the end
    /// of the file, and how fast mpv is downloading (bit/s) while it is.
    Cache { ahead: f64, done: bool, rate: Option<f64> },
    FileLoaded,
    /// `eof`: the file played to its end. `error`: it failed to play.
    EndFile { eof: bool, error: Option<String> },
    /// The mpv process went away.
    Exited,
}

pub struct Player {
    child: Child,
    writer: IpcConn,
    endpoint: OsString,
    /// Ends mpv if the app dies without dropping this `Player`.
    _lifeline: Option<platform::Lifeline>,
}

/// FFmpeg's HTTP options for picking up a dropped connection where it broke off.
const RECONNECT: &str = "reconnect=1,reconnect_streamed=1,reconnect_delay_max=5";

/// Where mpv gets the sound from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feed {
    /// A live HLS stream, from the site.
    Live,
    /// A file, from the site.
    Site,
    /// A file, from the relay on this machine (see `relay`).
    Relay,
}

/// Quotes a string for mpv's command parser.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A started mpv with both IPC connections open.
struct Launched {
    child: Child,
    reader: IpcConn,
    writer: IpcConn,
    lifeline: Option<platform::Lifeline>,
}

impl Player {
    pub fn spawn(
        mpv: &Path,
        extra_args: &[String],
        volume: f32,
        muted: bool,
        on_event: impl Fn(PlayerEvent) + Send + 'static,
    ) -> Result<Player, String> {
        let endpoint = platform::mpv_ipc_endpoint();
        // mpv before 0.33 does not know the lifeline or cover options and exits at
        // once. Without them it still plays; it just outlives a crashed app.
        let Launched { child, reader, writer, lifeline } = launch(mpv, extra_args, volume, muted, &endpoint, true)
            .or_else(|e| if e.exited { launch(mpv, extra_args, volume, muted, &endpoint, false) } else { Err(e) })
            .map_err(|e| e.message)?;

        std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if let Some(event) = parse_event(&line) {
                        on_event(event);
                    }
                }
                on_event(PlayerEvent::Exited);
            })
            .map_err(|e| e.to_string())?;

        Ok(Player { child, writer, endpoint, _lifeline: lifeline })
    }

    fn send(&mut self, line: &str) -> std::io::Result<()> {
        writeln!(self.writer, "{line}")?;
        self.writer.flush()
    }

    /// `title` is what the system's media controls show (instead of the stream URL).
    /// `start`: where to begin, in seconds (`None`: the beginning).
    pub fn load(&mut self, url: &str, title: &str, user_agent: Option<&str>, feed: Feed, paused: bool, start: Option<f64>) -> std::io::Result<()> {
        if let Some(ua) = user_agent {
            self.send(&format!("raw set user-agent {}", quote(ua)))?;
        }
        let options = match feed {
            // Reconnecting rides out dropped connections on long videos, but it stalls
            // a live HLS stream once its first segments have played.
            Feed::Live => String::new(),
            Feed::Site => RECONNECT.to_string(),
            // The relay deals with the network. A small socket buffer, so that it
            // fetches no more than mpv has read; seeking over a gap rather than
            // reading through it.
            Feed::Relay => format!("recv_buffer_size={0},short_seek_size={0}", super::relay::SOCKET_BUFFER),
        };
        self.send(&format!("raw set stream-lavf-o {}", quote(&options)))?;
        self.send(&format!("raw set force-media-title {}", quote(title)))?;
        self.send(&format!("set pause {}", if paused { "yes" } else { "no" }))?;
        // An option rather than a loadfile argument, whose syntax changed in mpv 0.38.
        self.send(&match start {
            Some(secs) => format!("raw set start {:.3}", secs.max(0.0)),
            None => "raw set start none".to_string(),
        })?;
        self.send(&format!("raw loadfile {} replace", quote(url)))
    }

    pub fn set_pause(&mut self, paused: bool) -> std::io::Result<()> {
        self.send(&format!("set pause {}", if paused { "yes" } else { "no" }))
    }

    pub fn seek(&mut self, secs: f64) -> std::io::Result<()> {
        self.send(&format!("seek {:.3} absolute", secs.max(0.0)))
    }

    pub fn set_volume(&mut self, volume: f32) -> std::io::Result<()> {
        self.send(&format!("set volume {}", (volume.clamp(0.0, 1.0) * 100.0).round()))
    }

    /// How far ahead to download the sound, in seconds (`None`: as far as the
    /// cache holds).
    pub fn set_readahead(&mut self, secs: Option<u32>) -> std::io::Result<()> {
        self.send(&format!("set cache-secs {}", secs.unwrap_or(36_000)))
    }

    pub fn set_mute(&mut self, muted: bool) -> std::io::Result<()> {
        self.send(&format!("set mute {}", if muted { "yes" } else { "no" }))
    }

    pub fn stop(&mut self) -> std::io::Result<()> {
        self.send("stop")
    }

    /// Whether the process is still running.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.send("quit");
        // Give mpv a moment to close its audio device cleanly.
        for _ in 0..10 {
            if !self.alive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        platform::ipc_cleanup(&self.endpoint);
    }
}

struct LaunchError {
    message: String,
    /// mpv quit on its own right after starting (e.g. it rejected an option).
    exited: bool,
}

/// `modern`: also use the options of mpv 0.33+ (lifeline, Now Playing cover).
fn launch(mpv: &Path, extra_args: &[String], volume: f32, muted: bool, endpoint: &OsString, modern: bool) -> Result<Launched, LaunchError> {
    let fail = |message: String, exited: bool| LaunchError { message, exited };
    platform::ipc_cleanup(endpoint);
    let mut cmd = Command::new(mpv);
    cmd.args([
        "--idle=yes",
        "--terminal=no",
        "--no-config",
        "--no-video",
        "--sid=no",
        "--force-window=no",
        "--audio-display=no",
        "--keep-open=no",
        "--input-default-bindings=no",
        "--cache=yes",
        // About 5 minutes of audio ahead and 1.5 behind (mpv's bookkeeping per
        // packet about doubles the bytes).
        "--demuxer-max-bytes=12MiB",
        "--demuxer-max-back-bytes=4MiB",
        "--audio-client-name=Pulse",
        "--title=Pulse",
    ])
    .arg(format!("--volume={}", (volume * 100.0).round()))
    .arg(format!("--mute={}", if muted { "yes" } else { "no" }))
    .arg({
        let mut arg = OsString::from("--input-ipc-server=");
        arg.push(endpoint);
        arg
    });
    if has_scripts(mpv) {
        cmd.args(SCRIPT_OPTIONS);
    }
    if let Some(cover) = platform::now_playing_cover().filter(|_| modern) {
        let mut arg = OsString::from("--cover-art-files-append=");
        arg.push(cover);
        cmd.arg(arg);
    }
    let mut lifeline = modern.then(|| platform::mpv_lifeline(&mut cmd));
    cmd.args(extra_args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    platform::quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| fail(format!("could not start mpv: {e}"), false))?;
    if let Some(lifeline) = lifeline.as_mut() {
        lifeline.started(&child);
    }

    // mpv creates the socket a moment after it starts.
    let connect = |child: &mut Child| -> Result<IpcConn, LaunchError> {
        for _ in 0..60 {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(fail(format!("mpv exited right away ({status})"), true));
            }
            if let Ok(conn) = platform::ipc_connect(endpoint) {
                return Ok(conn);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(fail("mpv did not start in time".to_string(), false))
    };
    let io = |e: std::io::Error| fail(e.to_string(), false);
    let connected = connect(&mut child).and_then(|mut reader| {
        let props = [(1, "time-pos"), (2, "duration"), (3, "pause"), (4, "core-idle"), (5, "paused-for-cache"), (6, "demuxer-cache-state")];
        for (id, name) in props {
            writeln!(reader, r#"{{"command":["observe_property",{id},"{name}"]}}"#).map_err(io)?;
        }
        let mut writer = connect(&mut child)?;
        // The writer never reads, so keep events from piling up on it.
        writeln!(writer, r#"{{"command":["disable_event","all"]}}"#).map_err(io)?;
        Ok((reader, writer))
    });
    match connected {
        Ok((reader, writer)) => Ok(Launched { child, reader, writer, lifeline }),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            platform::ipc_cleanup(endpoint);
            Err(e)
        }
    }
}

/// Switch off the scripts mpv runs by itself: the user's, the youtube-dl hook and
/// the on-screen controller.
const SCRIPT_OPTIONS: [&str; 3] = ["--load-scripts=no", "--ytdl=no", "--osc=no"];

/// Whether this mpv takes `SCRIPT_OPTIONS`. Without scripting (the -full packages'
/// mpv) they do not exist, and mpv exits at an unknown option. Tried once per mpv;
/// only the exit status counts, as Windows' mpv.exe may print nowhere.
fn has_scripts(mpv: &Path) -> bool {
    static KNOWN: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);
    let mut known = KNOWN.lock().unwrap();
    if known.as_ref().is_none_or(|(path, _)| path != mpv) {
        let mut cmd = Command::new(mpv);
        cmd.arg("--no-config").args(SCRIPT_OPTIONS).arg("--version");
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        platform::quiet(&mut cmd);
        // In doubt (it did not run), as before: every mpv used to have them.
        let scripts = cmd.status().map_or(true, |s| s.success());
        *known = Some((mpv.to_path_buf(), scripts));
    }
    known.as_ref().is_none_or(|(_, scripts)| *scripts)
}

fn parse_event(line: &str) -> Option<PlayerEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v.get("event")?.as_str()? {
        "property-change" => {
            let data = v.get("data");
            match v.get("name")?.as_str()? {
                "time-pos" => data?.as_f64().map(PlayerEvent::TimePos),
                "duration" => data?.as_f64().map(PlayerEvent::Duration),
                "pause" => data?.as_bool().map(PlayerEvent::Pause),
                "core-idle" => data?.as_bool().map(PlayerEvent::CoreIdle),
                "paused-for-cache" => data?.as_bool().map(PlayerEvent::Buffering),
                "demuxer-cache-state" => {
                    let data = data?;
                    let flag = |key: &str| data.get(key).and_then(Value::as_bool).unwrap_or(false);
                    Some(PlayerEvent::Cache {
                        ahead: data.get("cache-duration").and_then(Value::as_f64).unwrap_or(0.0),
                        done: flag("eof"),
                        rate: (!flag("idle")).then(|| data.get("raw-input-rate").and_then(Value::as_f64).unwrap_or(0.0) * 8.0),
                    })
                }
                _ => None,
            }
        }
        "file-loaded" => Some(PlayerEvent::FileLoaded),
        "end-file" => {
            let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
            Some(PlayerEvent::EndFile {
                eof: reason == "eof",
                error: (reason == "error").then(|| {
                    v.get("file_error").and_then(Value::as_str).unwrap_or("playback failed").to_string()
                }),
            })
        }
        _ => None,
    }
}
