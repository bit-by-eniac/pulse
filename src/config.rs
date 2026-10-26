//! Small JSON settings file in the OS config dir.

use crate::platform;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Mode {
    Audio,
    Video,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    pub volume: f32,
    pub muted: bool,
    pub mode: Mode,
    /// `None` = automatic (matches the size of the picture).
    pub quality: Option<u32>,
    pub repeat: Repeat,
    pub shuffle: bool,
    pub window_size: Option<[f32; 2]>,
    /// Extra arguments for yt-dlp, e.g. `["--cookies-from-browser", "firefox"]`.
    pub ytdlp_args: Vec<String>,
    /// Browser whose YouTube sign-in may be used when YouTube blocks a request
    /// (`None`: never). Only ever set by the user.
    pub sign_in: Option<String>,
    /// Extra arguments for mpv, e.g. `["--audio-device=coreaudio/..."]`.
    pub mpv_args: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            volume: 0.8,
            muted: false,
            mode: Mode::Video,
            quality: None,
            repeat: Repeat::Off,
            shuffle: false,
            window_size: None,
            ytdlp_args: Vec::new(),
            sign_in: None,
            mpv_args: Vec::new(),
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        platform::config_dir().join("config.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}
