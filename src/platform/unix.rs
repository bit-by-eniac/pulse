//! What macOS and Linux have in common: unix sockets, file modes, process groups.
//! `macos.rs` and `linux.rs` re-export this and add what differs.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;

pub const EXE_SUFFIX: &str = "";

pub type IpcConn = UnixStream;

pub fn quiet(_cmd: &mut Command) {}

pub fn limit_send_buffer(stream: &std::net::TcpStream, bytes: i32) {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid socket and a pointer to an int of the given size.
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&bytes as *const i32).cast(),
            std::mem::size_of::<i32>() as libc::socklen_t,
        );
    }
}

pub fn make_executable(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

pub fn ipc_endpoint() -> OsString {
    // Socket paths are limited to ~100 bytes, so keep the name short.
    super::imp::runtime_dir().join(format!("pulse-mpv-{}.sock", std::process::id())).into_os_string()
}

pub fn ipc_connect(endpoint: &OsString) -> io::Result<IpcConn> {
    UnixStream::connect(endpoint)
}

pub fn ipc_cleanup(endpoint: &OsString) {
    let _ = std::fs::remove_file(endpoint);
}

/// Our end of a connection that mpv quits on losing.
pub struct Lifeline {
    ours: Option<UnixStream>,
    theirs: Option<UnixStream>,
}

impl Lifeline {
    /// Call once mpv runs: closes our copy of mpv's end, so only mpv holds it.
    pub fn started(&mut self, _child: &std::process::Child) {
        self.theirs = None;
        if let Some(ours) = self.ours.as_mut() {
            // mpv sends every client some events; this one never reads them.
            use std::io::Write;
            let _ = writeln!(ours, r#"{{"command":["disable_event","all"]}}"#);
        }
    }
}

/// Hands mpv one end of a socket pair as an IPC client. mpv quits when that
/// connection closes, which the OS does when the app exits in any way, a crash
/// or `kill` included, so audio never plays on without the app.
pub fn mpv_lifeline(cmd: &mut Command) -> Lifeline {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let Ok((ours, theirs)) = UnixStream::pair() else { return Lifeline { ours: None, theirs: None } };
    let fd = theirs.as_raw_fd();
    cmd.arg(format!("--input-ipc-client=fd://{fd}"));
    // Rust opens sockets close-on-exec; let this one through to mpv.
    // SAFETY: only fcntl, which is async-signal-safe, runs between fork and exec.
    unsafe {
        cmd.pre_exec(move || match libc::fcntl(fd, libc::F_SETFD, 0) {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    Lifeline { ours: Some(ours), theirs: Some(theirs) }
}

/// Helpers end with the app by themselves here: ffmpeg on its closed output pipe,
/// mpv through its lifeline, yt-dlp when its short job is done.
pub fn end_with_app(_child: &std::process::Child) {}

pub fn own_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

pub fn kill_tree(child: &mut std::process::Child) {
    // The child leads its own process group, so a negative pid reaches all of it.
    let _ = Command::new("kill")
        .args(["-s", "KILL", "--", &format!("-{}", child.id())])
        .stderr(std::process::Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// After a crash or `kill`, an mpv we started can outlive the app. Its socket
/// name carries the owner's pid, so players whose owner is gone can be stopped.
pub fn reap_stale_players() {
    let Ok(entries) = std::fs::read_dir(super::imp::runtime_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = name.strip_prefix("pulse-mpv-").and_then(|r| r.strip_suffix(".sock")) else { continue };
        let alive = Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(true);
        if !alive {
            let _ = Command::new("pkill").args(["-f", &name]).status();
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
