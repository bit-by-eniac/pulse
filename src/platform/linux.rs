//! Linux, and other desktop unixes that follow the freedesktop conventions.

pub use super::unix::*;
use super::Script;
use std::path::PathBuf;

#[cfg(target_arch = "aarch64")]
pub const YTDLP_RELEASE: (&str, &str) = ("yt-dlp_linux_aarch64.zip", "yt-dlp_linux_aarch64");
#[cfg(not(target_arch = "aarch64"))]
pub const YTDLP_RELEASE: (&str, &str) = ("yt-dlp_linux.zip", "yt-dlp_linux");
pub const INSTALL_HINT: &str = "sudo apt install mpv ffmpeg   (or your distro's equivalent)";
pub const COMMAND_KEY: &str = "Ctrl";
pub const COOKIE_PERMISSION_HINT: &str = "Check that your user can read the browser\u{2019}s profile folder.";

/// Where mpv's socket goes: the per-user runtime folder when there is one.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Distro packages, snaps, Homebrew on Linux, and per-user installs.
pub fn extra_tool_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/usr/local/bin", "/usr/bin", "/snap/bin", "/home/linuxbrew/.linuxbrew/bin"].iter().map(PathBuf::from).collect();
    if let Some(home) = dirs::home_dir() {
        dirs.extend([".local/bin", ".deno/bin", ".bun/bin"].map(|d| home.join(d)));
    }
    dirs
}

pub fn fallback_fonts(script: Script) -> &'static [&'static str] {
    match script {
        Script::Cjk => &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ],
        Script::Arabic => &["/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
        Script::Hebrew => &["/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
        Script::Thai => &["/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf", "/usr/share/fonts/truetype/tlwg/Loma.ttf"],
        Script::Devanagari => &[
            "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf",
            "/usr/share/fonts/truetype/lohit-devanagari/Lohit-Devanagari.ttf",
        ],
    }
}

/// HTTPS through rustls with built-in root certificates: Linux has no single system
/// TLS library to link against (OpenSSL versions differ between distros).
pub fn tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::default()
}

/// mpv has no media-controls integration on Linux, so it needs no artwork.
pub fn now_playing_cover() -> Option<PathBuf> {
    None
}

/// The window icon for X11 (Wayland takes it from the desktop file).
pub fn window_icon_png() -> Option<&'static [u8]> {
    Some(include_bytes!("../../assets/icon-128.png"))
}

/// Profile folders of browsers whose cookies yt-dlp can read.
pub fn browser_dirs() -> Vec<(&'static str, &'static str, PathBuf)> {
    let (Some(config), Some(home)) = (dirs::config_dir(), dirs::home_dir()) else { return Vec::new() };
    vec![
        ("firefox", "Firefox", home.join(".mozilla/firefox")),
        ("firefox", "Firefox", home.join("snap/firefox/common/.mozilla/firefox")),
        ("firefox", "Firefox", home.join(".var/app/org.mozilla.firefox/.mozilla/firefox")),
        ("chrome", "Chrome", config.join("google-chrome")),
        ("chromium", "Chromium", config.join("chromium")),
        ("brave", "Brave", config.join("BraveSoftware/Brave-Browser")),
        ("edge", "Edge", config.join("microsoft-edge")),
        ("vivaldi", "Vivaldi", config.join("vivaldi")),
        ("opera", "Opera", config.join("opera")),
    ]
}

pub fn browser_access_note(browser: &str, name: &str) -> Option<String> {
    match browser {
        "firefox" => None,
        _ => Some(format!(
            "{name} keeps its cookies encrypted with your desktop keyring (GNOME Keyring or KWallet). If the keyring is locked, it will ask for your password."
        )),
    }
}
