//! Everything that differs between operating systems lives in this module.
//!
//! The rest of the crate never uses `cfg(target_os = ...)`. Each OS gets one file
//! (`macos.rs`, `linux.rs`, `windows.rs`) that provides the same small surface,
//! so a build carries only its own OS's paths, messages and TLS. macOS and Linux
//! share `unix.rs`. This file holds the logic built on top of them.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(all(unix, not(target_os = "macos")))]
mod linux;

#[cfg(all(unix, not(target_os = "macos")))]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(windows)]
use windows as imp;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

pub const APP_NAME: &str = "Pulse";

/// Writable per-user directory for downloaded helpers (e.g. an updated yt-dlp).
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join(APP_NAME)
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join(APP_NAME)
}

/// Directory where the app keeps tools it downloaded itself.
pub fn managed_bin_dir() -> PathBuf {
    data_dir().join("bin")
}

/// File name of an executable on this OS (`mpv` -> `mpv.exe` on Windows).
pub fn exe_name(base: &str) -> String {
    format!("{base}{}", imp::EXE_SUFFIX)
}

/// Looks for `base` in: the managed dir, next to our own executable, `PATH`,
/// and the well-known install locations of this OS. GUI apps launched from
/// Finder / the Start menu often have a much shorter `PATH` than a terminal.
pub fn find_executable(base: &str) -> Option<PathBuf> {
    let file = exe_name(base);
    let mut dirs: Vec<PathBuf> = vec![managed_bin_dir()];
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(dir.clone());
        dirs.push(dir.join("tools"));
        // macOS bundle: Pulse.app/Contents/MacOS/pulse -> Contents/Resources
        if let Some(contents) = dir.parent() {
            dirs.push(contents.join("Resources"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(imp::extra_tool_dirs());
    dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Stops a console window from flashing up when we spawn helper processes.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    imp::quiet(cmd);
    cmd
}

/// Marks a downloaded file as runnable (no-op on Windows).
pub fn make_executable(path: &Path) -> io::Result<()> {
    imp::make_executable(path)
}

/// The yt-dlp release asset for this OS/arch and the executable inside it. It is the
/// zipped "onedir" build: the single-file build unpacks itself on every run, which
/// adds seconds to each search and track start.
pub fn ytdlp_release() -> (&'static str, &'static str) {
    imp::YTDLP_RELEASE
}

/// How the user installs mpv/ffmpeg on this OS.
pub fn install_hint() -> &'static str {
    imp::INSTALL_HINT
}

/// Name of the key that egui's `command` modifier means here ("Cmd" or "Ctrl").
pub fn command_key() -> &'static str {
    imp::COMMAND_KEY
}

/// An HTTPS client. macOS and Windows use the system's TLS; Linux uses rustls.
pub fn http_agent(timeout: Option<std::time::Duration>) -> ureq::Agent {
    ureq::Agent::config_builder().tls_config(imp::tls_config()).timeout_global(timeout).build().into()
}

/// For streaming: connecting and the start of each answer must take less than
/// `answer`; how long a body takes is up to each request.
pub fn stream_agent(answer: std::time::Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(imp::tls_config())
        .timeout_connect(Some(answer))
        .timeout_recv_response(Some(answer))
        .build()
        .into()
}

/// The app icon as PNG, where the OS needs it set on the window.
pub fn window_icon_png() -> Option<&'static [u8]> {
    imp::window_icon_png()
}

/// A small image for the system's media controls, if mpv shows it there.
pub fn now_playing_cover() -> Option<PathBuf> {
    imp::now_playing_cover()
}

/// Writes `bytes` to `name` in the data folder unless it is already there.
#[allow(dead_code)] // not every OS needs one
fn cached_file(name: &str, bytes: &[u8]) -> Option<PathBuf> {
    let path = data_dir().join(name);
    if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        std::fs::create_dir_all(data_dir()).ok()?;
        let part = path.with_extension("part");
        std::fs::write(&part, bytes).ok()?;
        std::fs::rename(&part, &path).ok()?;
    }
    Some(path)
}

/// A private IPC endpoint name for talking to one mpv process.
pub fn mpv_ipc_endpoint() -> OsString {
    imp::ipc_endpoint()
}

/// Keeps at most about `bytes` queued in the OS for a local connection, so the
/// writer knows how far the reader has really got.
pub fn limit_send_buffer(stream: &std::net::TcpStream, bytes: i32) {
    imp::limit_send_buffer(stream, bytes)
}

/// Connection to mpv's JSON IPC server (unix socket / named pipe).
pub type IpcConn = imp::IpcConn;

pub fn ipc_connect(endpoint: &OsString) -> io::Result<IpcConn> {
    imp::ipc_connect(endpoint)
}

/// Removes the leftover endpoint (socket file) after mpv exits.
pub fn ipc_cleanup(endpoint: &OsString) {
    imp::ipc_cleanup(endpoint)
}

/// Writing systems that the bundled fonts do not cover.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Script {
    Cjk,
    Arabic,
    Hebrew,
    Thai,
    Devanagari,
}

/// System fonts that can render `script`, best first.
pub fn fallback_fonts(script: Script) -> Vec<PathBuf> {
    imp::fallback_fonts(script).iter().map(PathBuf::from).collect()
}

/// A web browser whose saved logins other programs can reuse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Browser {
    /// Lower-case name, as tools like yt-dlp spell it ("chrome", "firefox", ...).
    pub id: &'static str,
    pub name: &'static str,
}

/// Browsers that have a user profile on this machine, the most dependable first.
pub fn browsers() -> Vec<Browser> {
    let mut found: Vec<Browser> = Vec::new();
    for (id, name, dir) in imp::browser_dirs() {
        if !found.iter().any(|b| b.id == id) && has_cookie_store(&dir) {
            found.push(Browser { id, name });
        }
    }
    found
}

/// Whether a browser folder holds a cookie database (uninstalled browsers often
/// leave an empty folder behind). Folders the OS won't let us list count as yes.
fn has_cookie_store(dir: &Path) -> bool {
    const NAMES: [&str; 3] = ["Cookies", "cookies.sqlite", "Cookies.binarycookies"];
    let mut stack = vec![(dir.to_path_buf(), 0)];
    let mut budget = 4000;
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                if depth == 0 && e.kind() == io::ErrorKind::PermissionDenied {
                    return true;
                }
                continue;
            }
        };
        for entry in entries.flatten() {
            budget -= 1;
            if budget == 0 {
                return false;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 4 {
                    stack.push((path, depth + 1));
                }
            } else if NAMES.iter().any(|n| entry.file_name() == *n) {
                return true;
            }
        }
    }
    false
}

/// Extra advice when the OS refused to let us read a browser's cookies.
pub fn cookie_permission_hint() -> &'static str {
    imp::COOKIE_PERMISSION_HINT
}

/// What the OS will ask, or what the user must do, when another program reads
/// this browser's cookies. `None` when nothing will be asked.
pub fn browser_access_note(browser: &str, name: &str) -> Option<String> {
    imp::browser_access_note(browser, name)
}

/// Makes mpv end with the app, even when the app crashes or is killed. Call before
/// spawning, call `started` on the result once it runs, and keep it while mpv runs.
pub fn mpv_lifeline(cmd: &mut Command) -> Lifeline {
    imp::mpv_lifeline(cmd)
}

pub type Lifeline = imp::Lifeline;

/// Makes a helper process end with the app where the OS does not do that by itself.
pub fn end_with_app(child: &Child) {
    imp::end_with_app(child)
}

/// Starts `cmd` as the head of its own process tree, so `kill_tree` can stop all of it
/// (some tools re-launch themselves as a child process).
pub fn own_process_group(cmd: &mut Command) {
    imp::own_process_group(cmd)
}

/// Stops a child started with `own_process_group`, including anything it started.
pub fn kill_tree(child: &mut Child) {
    imp::kill_tree(child)
}

/// Stops mpv processes left behind by a crashed earlier run.
pub fn reap_stale_players() {
    imp::reap_stale_players()
}
