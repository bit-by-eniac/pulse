pub use super::unix::*;
use super::Script;
use std::path::PathBuf;
use std::sync::Mutex;

pub const YTDLP_RELEASE: (&str, &str) = ("yt-dlp_macos.zip", "yt-dlp_macos");
pub const INSTALL_HINT: &str = "brew install mpv ffmpeg";
pub const COMMAND_KEY: &str = "Cmd";
pub const COOKIE_PERMISSION_HINT: &str =
    "Allow it when macOS asks, or turn on Full Disk Access for Pulse in System Settings \u{203a} Privacy & Security.";

/// Where mpv's socket goes: the per-user temporary folder.
pub fn runtime_dir() -> PathBuf {
    std::env::temp_dir()
}

/// Homebrew (Apple silicon and Intel), MacPorts, and per-user installs.
pub fn extra_tool_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"].iter().map(PathBuf::from).collect();
    if let Some(home) = dirs::home_dir() {
        dirs.extend([".local/bin", ".deno/bin", ".bun/bin"].map(|d| home.join(d)));
    }
    dirs
}

pub fn fallback_fonts(script: Script) -> &'static [&'static str] {
    match script {
        // Arial Unicode covers Chinese, Japanese and Korean in one face.
        Script::Cjk => &[
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/Library/Fonts/Arial Unicode.ttf",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ],
        Script::Arabic => &["/System/Library/Fonts/GeezaPro.ttc"],
        Script::Hebrew => &["/System/Library/Fonts/ArialHB.ttc"],
        Script::Thai => &["/System/Library/Fonts/ThonburiUI.ttc", "/System/Library/Fonts/Supplemental/Ayuthaya.ttf"],
        Script::Devanagari => &["/System/Library/Fonts/Kohinoor.ttc", "/System/Library/Fonts/Supplemental/DevanagariMT.ttc"],
    }
}

/// HTTPS through macOS's own TLS and certificate store, so the app carries neither.
pub fn tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

/// mpv shows the playing track in Control Center's Now Playing and re-encodes the
/// artwork on every update. Without artwork it uses its own large icon, and those
/// re-encodes leave hundreds of MB behind, so it gets a small one.
pub fn now_playing_cover() -> Option<PathBuf> {
    super::cached_file("now-playing.png", include_bytes!("../../assets/icon-64.png"))
}

/// VideoToolbox, for the codecs the Mac decodes in hardware: H.264 on every Mac
/// Pulse runs on, VP9 on Apple silicon. Asked once per codec.
pub fn gpu_video(codec: &str) -> Option<super::GpuVideo> {
    let fourcc = match codec {
        c if c.starts_with("avc1") => *b"avc1",
        c if c.starts_with("vp9") || c.starts_with("vp09") => *b"vp09",
        _ => return None,
    };
    static KNOWN: Mutex<Vec<([u8; 4], bool)>> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap();
    let hardware = match known.iter().find(|(c, _)| *c == fourcc) {
        Some(&(_, hardware)) => hardware,
        None => {
            let kind = u32::from_be_bytes(fourcc);
            // SAFETY: plain C functions taking a codec's four-character code. VP9's
            // decoder is a supplemental one, there only once registered.
            let hardware = unsafe {
                VTRegisterSupplementalVideoDecoderIfAvailable(kind);
                VTIsHardwareDecodeSupported(kind) != 0
            };
            known.push((fourcc, hardware));
            hardware
        }
    };
    hardware.then_some(super::GpuVideo { hwaccel: "videotoolbox", frames: "videotoolbox_vld", scale: "scale_vt" })
}

pub fn keep_awake(on: bool) {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    thread_local! {
        static ACTIVITY: std::cell::RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { std::cell::RefCell::new(None) };
    }
    ACTIVITY.with_borrow_mut(|activity| {
        let info = NSProcessInfo::processInfo();
        match activity.take() {
            None if on => {
                let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
                    | NSActivityOptions::LatencyCritical
                    | NSActivityOptions::IdleDisplaySleepDisabled;
                *activity = Some(info.beginActivityWithOptions_reason(options, &NSString::from_str("Playing video")));
            }
            // SAFETY: the object `beginActivity` returned.
            Some(started) if !on => unsafe { info.endActivity(&started) },
            kept => *activity = kept,
        }
    });
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTIsHardwareDecodeSupported(codec_type: u32) -> u8;
    fn VTRegisterSupplementalVideoDecoderIfAvailable(codec_type: u32);
}

/// None: the Dock shows the app bundle's icon, which is sharper than any set at runtime.
pub fn window_icon_png() -> Option<&'static [u8]> {
    None
}

/// Profile folders of browsers whose cookies yt-dlp can read.
pub fn browser_dirs() -> Vec<(&'static str, &'static str, PathBuf)> {
    let (Some(support), Some(home)) = (dirs::data_dir(), dirs::home_dir()) else { return Vec::new() };
    vec![
        ("chrome", "Chrome", support.join("Google/Chrome")),
        ("firefox", "Firefox", support.join("Firefox")),
        ("brave", "Brave", support.join("BraveSoftware/Brave-Browser")),
        ("edge", "Edge", support.join("Microsoft Edge")),
        ("vivaldi", "Vivaldi", support.join("Vivaldi")),
        ("opera", "Opera", support.join("com.operasoftware.Opera")),
        ("chromium", "Chromium", support.join("Chromium")),
        // Last: reading it needs Full Disk Access.
        ("safari", "Safari", home.join("Library/Containers/com.apple.Safari")),
    ]
}

pub fn browser_access_note(browser: &str, name: &str) -> Option<String> {
    match browser {
        "firefox" => None,
        "safari" => Some(format!(
            "macOS only lets apps read Safari\u{2019}s cookies with Full Disk Access. Turn it on for {} in System Settings \u{203a} Privacy & Security \u{203a} Full Disk Access, then try again.",
            super::APP_NAME
        )),
        _ => {
            let key = if browser == "edge" { "Microsoft Edge".to_string() } else { name.to_string() };
            Some(format!(
                "macOS will ask for your Mac password so that \u{201c}security\u{201d} (macOS\u{2019}s keychain tool, run by yt-dlp) can use \u{201c}{key} Safe Storage\u{201d}, the key {name} encrypts its cookies with. Choose Always Allow so it won\u{2019}t ask each time, or Allow to be asked again next time."
            ))
        }
    }
}
