use super::Script;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const EXE_SUFFIX: &str = ".exe";
#[cfg(target_arch = "aarch64")]
pub const YTDLP_RELEASE: (&str, &str) = ("yt-dlp_win_arm64.zip", "yt-dlp_arm64.exe");
#[cfg(not(target_arch = "aarch64"))]
pub const YTDLP_RELEASE: (&str, &str) = ("yt-dlp_win.zip", "yt-dlp.exe");
pub const COOKIE_PERMISSION_HINT: &str = "Close the browser and try again.";
pub const COMMAND_KEY: &str = "Ctrl";
pub const INSTALL_HINT: &str = "winget install mpv ffmpeg   (or: scoop install mpv ffmpeg)";

/// mpv serves JSON IPC on a named pipe; a pipe opens like a plain file.
pub type IpcConn = File;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn extra_tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("scoop").join("shims"));
        dirs.push(home.join(".deno").join("bin"));
    }
    if let Some(data) = std::env::var_os("ProgramData") {
        dirs.push(PathBuf::from(data).join("chocolatey").join("bin"));
    }
    dirs
}

pub fn quiet(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

pub fn limit_send_buffer(stream: &std::net::TcpStream, bytes: i32) {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{setsockopt, SOL_SOCKET, SO_SNDBUF};
    // SAFETY: a valid socket and a pointer to an int of the given size.
    unsafe {
        setsockopt(stream.as_raw_socket() as _, SOL_SOCKET, SO_SNDBUF, (&bytes as *const i32).cast(), std::mem::size_of::<i32>() as i32);
    }
}

pub fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub fn ipc_endpoint() -> OsString {
    format!(r"\\.\pipe\pulse-mpv-{}", std::process::id()).into()
}

pub fn ipc_connect(endpoint: &OsString) -> io::Result<IpcConn> {
    OpenOptions::new().read(true).write(true).open(endpoint)
}

pub fn ipc_cleanup(_endpoint: &OsString) {}

pub fn fallback_fonts(script: Script) -> &'static [&'static str] {
    match script {
        Script::Cjk => &[
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\YuGothM.ttc",
            r"C:\Windows\Fonts\malgun.ttf",
            r"C:\Windows\Fonts\msjh.ttc",
        ],
        Script::Arabic => &[r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\arial.ttf"],
        Script::Hebrew => &[r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\arial.ttf"],
        Script::Thai => &[r"C:\Windows\Fonts\LeelawUI.ttf", r"C:\Windows\Fonts\tahoma.ttf"],
        Script::Devanagari => &[r"C:\Windows\Fonts\Nirmala.ttc", r"C:\Windows\Fonts\mangal.ttf"],
    }
}

pub fn reap_stale_players() {}

/// HTTPS through Windows' own TLS (SChannel) and certificate store.
pub fn tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

/// Keeps the display on. The setting belongs to the calling thread, the UI's.
pub fn keep_awake(on: bool) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED};
    // SAFETY: a plain call taking flags.
    unsafe { SetThreadExecutionState(if on { ES_CONTINUOUS | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS }) };
}

/// Software decoding for now: the bundled ffmpeg has no D3D11 decoding.
pub fn gpu_video(_codec: &str) -> Option<super::GpuVideo> {
    None
}

/// mpv shows the playing track in Windows' media overlay; give it Pulse's icon
/// rather than its own large one, which it re-encodes on every update.
pub fn now_playing_cover() -> Option<PathBuf> {
    super::cached_file("now-playing.png", include_bytes!("../../assets/icon-64.png"))
}

pub fn browser_access_note(browser: &str, name: &str) -> Option<String> {
    match browser {
        "firefox" => None,
        "chrome" | "edge" => Some(format!(
            "{name} locks its cookies so that other programs often can\u{2019}t read them. Close {name} first; if it still fails, use Firefox."
        )),
        _ => Some(format!("Close {name} first: Windows keeps its cookie file locked while it runs.")),
    }
}

/// Nothing to hold on Windows: mpv joins the app's job instead (`end_with_app`).
pub struct Lifeline;

impl Lifeline {
    pub fn started(&mut self, child: &std::process::Child) {
        end_with_app(child);
    }
}

pub fn mpv_lifeline(_cmd: &mut Command) -> Lifeline {
    Lifeline
}

/// Puts a helper in a job that Windows closes when the app exits in any way, a
/// crash included, and that kills everything in it (and what they started) then.
pub fn end_with_app(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::*;
    // The handle stays open for the app's lifetime; closing it is the point.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| {
        // SAFETY: plain Win32 calls with a zeroed, correctly sized info struct.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of_val(&info) as u32;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, size);
            job as usize
        }
    });
    if job != 0 {
        // SAFETY: both handles are valid; failure only means the helper is not tied.
        unsafe {
            AssignProcessToJobObject(job as HANDLE, child.as_raw_handle() as HANDLE);
        }
    }
}

pub fn own_process_group(_cmd: &mut Command) {}

pub fn kill_tree(child: &mut std::process::Child) {
    let mut cmd = Command::new("taskkill");
    cmd.args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    quiet(&mut cmd);
    let _ = cmd.status();
    let _ = child.kill();
    let _ = child.wait();
}

/// The window and taskbar icon (Windows does not take it from the .exe by itself).
pub fn window_icon_png() -> Option<&'static [u8]> {
    Some(include_bytes!("../../assets/icon-128.png"))
}

/// Profile folders of browsers whose cookies yt-dlp can read. Firefox first:
/// Chrome and Edge encrypt cookies so that other programs often cannot read them.
pub fn browser_dirs() -> Vec<(&'static str, &'static str, PathBuf)> {
    let (Some(roaming), Some(local)) = (dirs::data_dir(), dirs::data_local_dir()) else { return Vec::new() };
    vec![
        ("firefox", "Firefox", roaming.join(r"Mozilla\Firefox")),
        ("brave", "Brave", local.join(r"BraveSoftware\Brave-Browser\User Data")),
        ("vivaldi", "Vivaldi", local.join(r"Vivaldi\User Data")),
        ("opera", "Opera", roaming.join(r"Opera Software\Opera Stable")),
        ("chrome", "Chrome", local.join(r"Google\Chrome\User Data")),
        ("edge", "Edge", local.join(r"Microsoft\Edge\User Data")),
        ("chromium", "Chromium", local.join(r"Chromium\User Data")),
    ]
}
