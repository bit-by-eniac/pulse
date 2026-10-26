//! The only seam between the app and the outside world of video sites.
//!
//! The UI and the playback engine talk to `dyn Provider` and the plain data types
//! below, and nothing else. They never see a URL pattern, a JSON field name or a
//! command-line flag of the backend. If YouTube (or yt-dlp) changes, only the
//! implementation in `ytdlp.rs` has to change; a different backend (Invidious,
//! Piped, ...) can be dropped in by implementing the same trait.

pub mod ytdlp;

use std::fmt;

/// One playable item.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// Stable id inside the backend (used for dedupe and thumbnails).
    pub id: String,
    /// What gets handed back to `Provider::resolve`. Also the link Share copies,
    /// so it must be the track's public page.
    pub url: String,
    pub title: String,
    pub channel: Option<String>,
    pub duration: Option<u64>,
    pub views: Option<u64>,
    pub thumbnail: Option<String>,
    pub live: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerKind {
    Playlist,
    Channel,
}

/// A playlist or channel that can be opened into a `Listing`.
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub kind: ContainerKind,
    pub id: String,
    pub url: String,
    pub title: String,
    pub owner: Option<String>,
    pub subscribers: Option<u64>,
    pub thumbnail: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SearchItem {
    Track(Track),
    Container(Container),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchKind {
    Video,
    Playlist,
    Channel,
}

/// The contents of a playlist/channel.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub title: String,
    pub owner: Option<String>,
    pub tracks: Vec<Track>,
}

/// What the user typed into the search box, as understood by the backend.
#[derive(Clone, Debug)]
pub enum Input {
    Search(String),
    /// A single video link: play it right away.
    Track(Track),
    /// A playlist link; `focus` is the video to start on, if the link named one.
    Playlist { url: String, focus: Option<String> },
    Channel { url: String },
}

#[derive(Clone, Debug)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct AudioStream {
    pub url: String,
    pub headers: Vec<Header>,
    /// Human readable, e.g. "opus 130k".
    pub label: String,
    /// Average bitrate in kbit/s (0: unknown).
    pub bitrate: u32,
    /// Size in bytes of a single file the site serves in parts (byte ranges);
    /// `None`: unknown, or not such a file.
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct VideoStream {
    pub url: String,
    pub headers: Vec<Header>,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    /// Human readable, e.g. "avc1".
    pub codec: String,
    /// Served as a playlist of short files (HLS). A decoder that sat paused on
    /// one for long cannot carry on cleanly: the site cuts off idle downloads.
    pub segmented: bool,
    /// Average bitrate in kbit/s (estimated from the height when unknown).
    pub bitrate: u32,
    /// As for `AudioStream`.
    pub size: Option<u64>,
}

/// Everything needed to actually play a track. Short-lived: stream URLs expire.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// Fresh metadata (the link the user pasted may have had none).
    pub track: Track,
    /// Ascending by bitrate, never empty: the same sound in smaller sizes for
    /// slow connections, the best one last.
    pub audio: Vec<AudioStream>,
    /// Ascending by height, then bitrate. A height can have two streams: the one
    /// that is cheapest to decode, and a smaller one in another codec.
    pub video: Vec<VideoStream>,
    /// The streams are refused before this moment (the site makes the client sit
    /// through the length of an ad first).
    pub ready_at: Option<std::time::SystemTime>,
    /// The stream links stop working at this moment; `resolve` again for new ones.
    pub expires_at: Option<std::time::SystemTime>,
    /// Roughly how fast the connection is (bit/s), when the lookup was slow
    /// enough to tell: until playback has measured it, a guide to the first
    /// stream sizes.
    pub line_hint: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// A helper program is not installed.
    Missing,
    /// No network / timeout.
    Network,
    /// The video does not exist, is private, region locked, ...
    Unavailable,
    /// The site asks for a login or a captcha; a sign-in source can get past it.
    Blocked,
    /// The backend no longer understands the site. Updating it usually fixes this.
    BackendOutdated,
    Other,
}

#[derive(Clone, Debug)]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

pub type Result<T> = std::result::Result<T, ProviderError>;

/// A health report for the settings screen.
#[derive(Clone, Debug)]
pub struct Health {
    /// Name and version of the backend, if it runs.
    pub backend: std::result::Result<String, String>,
    /// Extra advice ("install deno", ...), shown as a warning.
    pub warnings: Vec<String>,
    /// Whether `update` can do something useful.
    pub can_update: bool,
}

/// A place the backend can borrow a signed-in session from (e.g. a web browser),
/// for when the site refuses anonymous requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInSource {
    pub id: String,
    pub name: String,
    /// What the OS will ask the first time this source is used, if anything.
    pub note: Option<String>,
}

pub trait Provider: Send + Sync {
    /// Classifies what the user typed: search text or a link.
    fn parse_input(&self, text: &str) -> Input;

    fn search(&self, query: &str, kind: SearchKind, limit: usize) -> Result<Vec<SearchItem>>;

    /// Lists the tracks of a playlist or channel.
    fn open(&self, container_url: &str, limit: usize) -> Result<Listing>;

    /// Looks up the actual media streams for a track.
    fn resolve(&self, track: &Track) -> Result<Resolved>;

    fn health(&self) -> Health;

    /// Extra options for the backend (e.g. yt-dlp command-line flags).
    fn set_extra_args(&self, args: Vec<String>);

    /// Signed-in sessions available on this machine.
    fn sign_in_sources(&self) -> Vec<SignInSource>;

    /// Session to fall back to when the site blocks a request (`None`: never sign in).
    fn set_sign_in(&self, source: Option<String>);

    /// Fetches a newer backend. Returns a message for the user.
    fn update(&self) -> Result<String>;
}

pub fn thumbnail_for(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg")
}
