//! The playback engine: queue, transport and the glue between the provider
//! (what to play), mpv (audio) and ffmpeg (video frames).
//!
//! It runs on its own thread and owns all playback state, so music keeps
//! advancing through the queue while the window is minimised or not redrawing.
//! The UI only sends `Cmd`s and reads `PlaybackState`.

mod abr;
mod clock;
mod mpv;
mod relay;
mod video;

pub use video::{Frame, FrameSlot};

use crate::config::{Config, Mode, Repeat};
use crate::platform;
use crate::provider::{ErrorKind, Provider, ProviderError, Resolved, Track};
use clock::Clock;
use mpv::{Player, PlayerEvent};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use video::VideoSession;

/// Called from engine threads to ask the UI for a repaint.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Nothing queued.
    Idle,
    Loading,
    Playing,
    Paused,
    Buffering,
    /// Reached the end of the queue.
    Stopped,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub id: u64,
    pub message: String,
    pub kind: ErrorKind,
}

/// A snapshot of everything the UI needs to draw the player.
#[derive(Clone, Debug)]
pub struct PlaybackState {
    pub queue: Arc<Vec<Track>>,
    pub index: Option<usize>,
    pub status: Status,
    pub duration: Option<f64>,
    pub live: bool,
    pub volume: f32,
    pub muted: bool,
    pub mode: Mode,
    /// The user's choice; `None` = automatic.
    pub quality: Option<u32>,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// Heights the current track offers for video.
    pub heights: Vec<u32>,
    /// Height of the stream being decoded right now.
    pub active_height: Option<u32>,
    pub audio_label: String,
    pub notice: Option<Notice>,
    pub net: NetStats,
}

/// How the connection is doing (updated about twice a second).
#[derive(Clone, Debug, Default)]
pub struct NetStats {
    /// Measured speed, bit/s.
    pub estimate: Option<f64>,
    /// Bitrate of the sound playing, kbit/s.
    pub audio_kbps: u32,
    /// Seconds of sound downloaded ahead; `None` once all of it is in.
    pub audio_ahead: Option<f64>,
    /// Seconds of picture fetched ahead (`None`: unknown, or all of it is in).
    pub video_ahead: Option<f64>,
    /// How far the picture is behind the sound, seconds.
    pub video_behind: Option<f64>,
    /// Video downloads wait so the sound can catch up.
    pub video_held: bool,
}

impl PlaybackState {
    pub fn current(&self) -> Option<&Track> {
        self.index.and_then(|i| self.queue.get(i))
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.status, Status::Playing | Status::Buffering | Status::Loading)
    }
}

pub enum Cmd {
    /// Replace the queue and start at `index`.
    PlayQueue { tracks: Vec<Track>, index: usize },
    PlayIndex(usize),
    Enqueue(Vec<Track>),
    RemoveFromQueue(usize),
    ClearQueue,
    TogglePause,
    Next,
    Prev,
    SeekTo(f64),
    SeekBy(f64),
    SetVolume(f32),
    SetMuted(bool),
    SetMode(Mode),
    SetQuality(Option<u32>),
    SetRepeat(Repeat),
    SetShuffle(bool),
    /// Where the picture is shown, in physical pixels; `None` when it is not visible.
    VideoTarget(Option<(u32, u32)>),
    DismissNotice,
    Shutdown,
}

enum Msg {
    Cmd(Cmd),
    Player(PlayerEvent),
    /// `took`: how long the lookup took (0 when it was done ahead).
    Resolved { gen: u64, result: Result<Resolved, ProviderError>, took: Duration },
    Prefetched { id: String, result: Result<Resolved, ProviderError>, took: Duration },
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Msg>,
    state: Arc<Mutex<PlaybackState>>,
    clock: Arc<Clock>,
    frames: Arc<FrameSlot>,
    exited: Arc<std::sync::atomic::AtomicBool>,
}

impl EngineHandle {
    /// Stops playback and waits (briefly) until mpv and ffmpeg are gone.
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
        for _ in 0..60 {
            if self.exited.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(Msg::Cmd(cmd));
    }

    pub fn state(&self) -> PlaybackState {
        self.state.lock().unwrap().clone()
    }

    /// Current position in seconds (interpolated between mpv updates).
    pub fn position(&self) -> f64 {
        self.clock.now()
    }

    pub fn frames(&self) -> &Arc<FrameSlot> {
        &self.frames
    }
}

/// Programs the engine shells out to.
#[derive(Clone, Debug, Default)]
pub struct Tools {
    pub mpv: Option<std::path::PathBuf>,
    pub ffmpeg: Option<std::path::PathBuf>,
}

impl Tools {
    pub fn discover() -> Self {
        Self { mpv: platform::find_executable("mpv"), ffmpeg: platform::find_executable("ffmpeg") }
    }
}

pub fn start(provider: Arc<dyn Provider>, config: &Config, waker: Waker) -> EngineHandle {
    let (tx, rx) = channel();
    let state = Arc::new(Mutex::new(PlaybackState {
        queue: Arc::new(Vec::new()),
        index: None,
        status: Status::Idle,
        duration: None,
        live: false,
        volume: config.volume,
        muted: config.muted,
        mode: config.mode,
        quality: config.quality,
        repeat: config.repeat,
        shuffle: config.shuffle,
        heights: Vec::new(),
        active_height: None,
        audio_label: String::new(),
        notice: None,
        net: NetStats::default(),
    }));
    let clock = Arc::new(Clock::new());
    let frames = Arc::new(FrameSlot::default());
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = EngineHandle { tx: tx.clone(), state: state.clone(), clock: clock.clone(), frames: frames.clone(), exited: exited.clone() };

    let mut engine = Engine {
        rx,
        tx,
        shared: state,
        clock,
        frames,
        provider,
        waker,
        exited,
        mpv_args: config.mpv_args.clone(),
        queue: Vec::new(),
        order: Vec::new(),
        pos: 0,
        index: None,
        volume: config.volume,
        muted: config.muted,
        mode: config.mode,
        quality: config.quality,
        repeat: config.repeat,
        shuffle: config.shuffle,
        player: None,
        gen: 0,
        current: None,
        loaded: false,
        paused: false,
        paused_since: None,
        holding: None,
        core_idle: true,
        buffering: false,
        stopped: false,
        duration: None,
        refreshed: None,
        refresh_at: None,
        failures: 0,
        prefetch: HashMap::new(),
        video: None,
        video_next: None,
        video_session: 0,
        video_startup: Duration::from_secs(1),
        video_retry: None,
        video_failures: 0,
        video_failed: None,
        gpu_refused: Vec::new(),
        target: None,
        target_changed: Instant::now(),
        hidden_since: None,
        restart_video: false,
        notice: None,
        notice_id: 0,
        meter: Arc::new(abr::Meter::default()),
        relay: None,
        relay_failed: false,
        sound: None,
        hold: Arc::new(AtomicBool::new(false)),
        audio_index: 0,
        audio_ceiling: None,
        audio: AudioCache::default(),
        audio_trouble: None,
        audio_from: Instant::now(),
        audio_reload: false,
        prefetched: false,
        readahead: None,
        lookup_took: Duration::ZERO,
        switched: None,
        want: None,
        last_up: None,
        up_failures: 0,
        resynced: None,
        late_restarts: 0,
        fell_behind: None,
        head_start: false,
        ceiling: None,
        height_cap: None,
        lagging: None,
        stats_at: Instant::now(),
        rng: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1,
        tools: Tools::discover(),
    };
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || engine.run())
        .expect("spawn engine thread");
    handle
}

struct Engine {
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    shared: Arc<Mutex<PlaybackState>>,
    clock: Arc<Clock>,
    frames: Arc<FrameSlot>,
    provider: Arc<dyn Provider>,
    waker: Waker,
    exited: Arc<std::sync::atomic::AtomicBool>,
    mpv_args: Vec<String>,

    queue: Vec<Track>,
    /// Play order (indices into `queue`); identity unless shuffled.
    order: Vec<usize>,
    pos: usize,
    index: Option<usize>,
    volume: f32,
    muted: bool,
    mode: Mode,
    quality: Option<u32>,
    repeat: Repeat,
    shuffle: bool,

    player: Option<Player>,
    /// Bumped for every track start, so stale lookups are ignored.
    gen: u64,
    current: Option<Resolved>,
    loaded: bool,
    paused: bool,
    /// When playback was paused, while it is.
    paused_since: Option<Instant>,
    /// Resuming after a long pause: mpv stays paused until a fresh decoder shows
    /// the picture (since when).
    holding: Option<Instant>,
    core_idle: bool,
    buffering: bool,
    stopped: bool,
    duration: Option<f64>,
    /// The last time a track got fresh stream links: its id and when.
    refreshed: Option<(String, Instant)>,
    /// Where to carry on once fresh links for the current track arrive.
    refresh_at: Option<f64>,
    failures: u32,
    prefetch: HashMap<String, (Instant, Resolved)>,

    /// The decoder whose frames are on screen.
    video: Option<VideoSession>,
    /// Its replacement at a new size or quality, while it gets ready.
    video_next: Option<VideoSession>,
    /// Number of the last decoder started.
    video_session: u64,
    /// How long the last replacement took to deliver its first frame.
    video_startup: Duration,
    /// When the last replacement failed, and how many failed in a row.
    video_retry: Option<Instant>,
    video_failures: u32,
    /// Track id whose video failed, so we do not retry in a loop.
    video_failed: Option<String>,
    /// Codecs the graphics chip would not decode after all: software from then on.
    gpu_refused: Vec<String>,
    target: Option<(u32, u32)>,
    target_changed: Instant,
    hidden_since: Option<Instant>,
    restart_video: bool,

    notice: Option<Notice>,
    notice_id: u64,

    /// Connection speed, from the video the relay fetches (and the sound).
    meter: Arc<abr::Meter>,
    /// Fetches single-file video for ffmpeg; started when first needed.
    relay: Option<relay::Relay>,
    relay_failed: bool,
    /// The sound's download, when the relay fetches it for mpv.
    sound: Option<Arc<relay::Source>>,
    /// Set while the sound runs low: video downloads wait.
    hold: Arc<AtomicBool>,
    /// Which of the current track's sound streams plays.
    audio_index: usize,
    /// Highest sound bitrate (kbit/s) the line has kept up with, once one did not.
    audio_ceiling: Option<u32>,
    audio: AudioCache,
    /// Since when mpv has been downloading slower than the sound plays.
    audio_trouble: Option<Instant>,
    /// When mpv last began downloading at a new place (a load or a seek).
    audio_from: Instant,
    /// mpv is loading another size of the same sound: the picture stays.
    audio_reload: bool,
    /// The next track has been looked up (or will not be).
    prefetched: bool,
    /// How far ahead mpv downloads the sound, in seconds (`None`: as far as its
    /// cache holds); see `update_readahead`.
    readahead: Option<u32>,
    /// How long the last lookup took.
    lookup_took: Duration,
    /// When the last decoder replacement started.
    switched: Option<Instant>,
    /// The stream automatic quality asks for, and since when.
    want: Option<(String, Instant)>,
    /// When a switch to a better stream last took over, and how many of those
    /// had to be undone soon after (each doubles the wait before the next try).
    last_up: Option<Instant>,
    up_failures: u32,
    /// When a decoder that fell behind was last restarted ahead of playback, how
    /// far ahead the next one starts, and when the picture last fell behind.
    resynced: Option<Instant>,
    late_restarts: u32,
    fell_behind: Option<Instant>,
    /// The last replacement came out late: the next one starts further ahead (see `lead`).
    head_start: bool,
    /// Most bit/s a picture kept up with (see `update_ceiling`), and since when.
    ceiling: Option<(f64, Instant)>,
    /// Tallest stream to decode after one the computer could not keep up with
    /// (Auto only), and since when.
    height_cap: Option<(u32, Instant)>,
    /// Since when the picture has been falling behind with its data there.
    lagging: Option<Instant>,
    stats_at: Instant,
    rng: u64,
    tools: Tools,
}

/// What mpv last said about the sound it has buffered.
#[derive(Default)]
struct AudioCache {
    ahead: f64,
    done: bool,
    /// Download rate (bit/s) while it downloads.
    rate: Option<f64>,
    /// A report came for the file playing now.
    known: bool,
}

const PREFETCH_TTL: Duration = Duration::from_secs(60 * 60);
const HIDDEN_GRACE: Duration = Duration::from_secs(15);
/// Wait this long after the last size change before decoding at the new size
/// (a fullscreen animation resizes the window many times).
const RESIZE_SETTLE: Duration = Duration::from_millis(350);
/// Give up on a replacement decoder that has not caught up by then.
const NEXT_VIDEO_TIMEOUT: Duration = Duration::from_secs(12);
/// Wait this long after a replacement failed before trying another, and give up
/// after this many in a row.
const VIDEO_RETRY: Duration = Duration::from_secs(3);
const VIDEO_ATTEMPTS: u32 = 3;
/// The site cuts off a download that has not been read for about 15 s; an HLS
/// decoder paused longer than this has lost the rest of its segment.
const STALE_PAUSE: Duration = Duration::from_secs(12);
/// Longest the sound waits for the picture when resuming after a long pause.
const HOLD_MAX: Duration = Duration::from_secs(5);
/// Fresh stream links for a track at most this often, so a link that keeps
/// failing cannot loop.
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
/// Links that expire within this are treated as expired.
const LINK_MARGIN: Duration = Duration::from_secs(60);
/// A replacement decoder still waiting for its data after this has met a dead
/// line; it is dropped and tried again later.
const NEXT_VIDEO_GIVE_UP: Duration = Duration::from_secs(90);
/// Video downloads wait while less than `HOLD_BELOW` seconds of sound are
/// buffered, until there are `HOLD_UNTIL`: the sound must not run dry.
const HOLD_BELOW: f64 = 6.0;
const HOLD_UNTIL: f64 = 12.0;
/// Switch to a smaller stream when less than this much picture is buffered (or
/// than the smaller one takes to get ready) and the line cannot carry the current one.
const DOWN_AT: f64 = 8.0;
/// Try a bigger stream only with this much picture buffered, a while after the
/// last switch, and once the line has had room for it for `UP_STABLE`.
const UP_AT: f64 = 10.0;
const UP_WAIT: Duration = Duration::from_secs(6);
const UP_STABLE: Duration = Duration::from_secs(3);
/// A switch up undone within this counts as failed.
const UP_UNDONE: Duration = Duration::from_secs(20);
/// A decoder this far behind the sound, for want of data, is restarted at the
/// playback point (at most every `RESYNC_EVERY`): catching up would need all
/// the data in between.
const RESYNC_BEHIND: f64 = 4.0;
const RESYNC_EVERY: Duration = Duration::from_secs(15);
/// Where the line has little to spare, or a restart came out late, the next
/// begins far enough ahead of playback for its data to be in by then: the
/// stream's header and `RESYNC_PLAY` seconds of it, at the share of the line
/// video gets. That estimate doubles while restarts
/// still come out late, and goes back once the picture has kept up for
/// `RESYNC_RESET`. A line slower than the smallest stream so shows the picture
/// in stretches rather than not at all. The header is what ffmpeg reads from
/// the start of the file before it jumps to its start point: the relay's
/// first piece.
const RESYNC_HEADER_BITS: f64 = 32.0 * 1024.0 * 8.0;
const RESYNC_PLAY: f64 = 15.0;
/// Media seconds in one fragment of a stream, about: a decoder can show nothing
/// before the one that holds its start point is in.
const FRAGMENT: f64 = 5.0;
const MAX_RESYNC_LEAD: f64 = 60.0;
const RESYNC_RESET: Duration = Duration::from_secs(60);
/// Sound that keeps downloading slower than it plays for this long, with less
/// than `AUDIO_LOW` seconds buffered, gets a smaller stream.
const AUDIO_TROUBLE: Duration = Duration::from_secs(3);
const AUDIO_LOW: f64 = 20.0;
/// After a load or a seek, mpv's download rate is mostly the wait for the first
/// bytes for this long.
const AUDIO_SETTLE: Duration = Duration::from_secs(5);
/// Look the next track up only once this much sound is buffered (or all of it,
/// or the end is near): on a slow line the lookup competes with playback.
const PREFETCH_AHEAD: f64 = 30.0;
/// While video plays, the sound is downloaded `AUDIO_OVER_VIDEO` seconds further
/// ahead than the picture, and at least `MIN_READAHEAD` (see `update_readahead`).
const AUDIO_OVER_VIDEO: f64 = 15.0;
const MIN_READAHEAD: u32 = 20;
/// mpv's cache this far below its mark gets more from the relay.
const SOUND_REFILL: f64 = 2.0;
/// For HLS, whose buffer is not seen: below this (bit/s) the sound is downloaded
/// only `SLOW_LINE_READAHEAD` seconds ahead.
const SLOW_LINE: f64 = 2e6;
const SLOW_LINE_READAHEAD: u32 = 60;
/// With this much sound in hand, a picture short of data may have the whole line.
const AUDIO_SPARE: f64 = 120.0;
/// How long a lowered ceiling (see `update_ceiling`) holds before a try without.
const CEILING_FOR: Duration = Duration::from_secs(60);

impl Engine {
    fn run(&mut self) {
        loop {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Cmd(Cmd::Shutdown)) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => self.tick(),
            }
            self.meter.check_stall();
            self.update_readahead();
            self.update_hold();
            self.maybe_prefetch();
            self.reconcile_video();
            self.release_hold();
            if self.stats_at.elapsed() >= Duration::from_millis(500) {
                self.publish_stats();
            }
        }
        self.stop_video();
        self.player = None;
        self.exited.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Cmd(cmd) => self.command(cmd),
            Msg::Player(ev) => self.player_event(ev),
            Msg::Resolved { gen, result, took } => {
                if !took.is_zero() {
                    self.lookup_took = took;
                }
                if gen == self.gen {
                    self.on_resolved(result);
                }
            }
            Msg::Prefetched { id, result, took } => {
                self.lookup_took = took;
                if let Ok(resolved) = result {
                    if self.prefetch.len() >= 3 {
                        self.prefetch.clear();
                    }
                    self.prefetch.insert(id, (Instant::now(), resolved));
                }
            }
        }
    }

    fn tick(&mut self) {
        if let Some(player) = self.player.as_mut() {
            if !player.alive() {
                self.player = None;
                self.fail(ErrorKind::Other, "The audio engine (mpv) stopped unexpectedly.".into());
            }
        }
    }

    // --- commands ---------------------------------------------------------------

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::PlayQueue { tracks, index } => {
                if tracks.is_empty() {
                    return;
                }
                self.index = Some(index.min(tracks.len() - 1));
                self.queue = tracks;
                self.rebuild_order();
                self.failures = 0;
                self.play_pos(self.pos);
            }
            Cmd::PlayIndex(i) => {
                if let Some(p) = self.order.iter().position(|&x| x == i) {
                    self.failures = 0;
                    self.play_pos(p);
                }
            }
            Cmd::Enqueue(tracks) => {
                let was_empty = self.queue.is_empty();
                for t in tracks {
                    if !self.queue.iter().any(|q| q.id == t.id) {
                        self.queue.push(t);
                        self.order.push(self.queue.len() - 1);
                    }
                }
                if was_empty && !self.queue.is_empty() {
                    self.index = Some(0);
                    self.rebuild_order();
                    self.play_pos(self.pos);
                } else {
                    self.publish();
                }
            }
            Cmd::RemoveFromQueue(i) => self.remove(i),
            Cmd::ClearQueue => {
                self.gen += 1;
                self.queue.clear();
                self.order.clear();
                self.index = None;
                self.pos = 0;
                self.reset_playback();
                self.publish();
            }
            Cmd::TogglePause => {
                if self.stopped && !self.queue.is_empty() {
                    self.play_pos(self.pos);
                } else if self.index.is_some() {
                    if self.paused {
                        self.resume();
                    } else {
                        self.pause();
                    }
                }
            }
            Cmd::Next => self.next(false),
            Cmd::Prev => self.prev(),
            Cmd::SeekTo(t) => self.seek(t),
            Cmd::SeekBy(d) => self.seek(self.clock.now() + d),
            Cmd::SetVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
                if self.volume > 0.0 {
                    self.muted = false;
                }
                let (v, m) = (self.volume, self.muted);
                self.with_player(|p, v| p.set_volume(v), v);
                self.with_player(|p, m| p.set_mute(m), m);
                self.publish();
            }
            Cmd::SetMuted(m) => {
                self.muted = m;
                self.with_player(|p, m| p.set_mute(m), m);
                self.publish();
            }
            Cmd::SetMode(m) => {
                self.mode = m;
                self.video_failed = None;
                self.publish();
            }
            Cmd::SetQuality(q) => {
                self.quality = q;
                self.publish();
            }
            Cmd::SetRepeat(r) => {
                self.repeat = r;
                self.publish();
            }
            Cmd::SetShuffle(s) => {
                self.shuffle = s;
                self.rebuild_order();
                self.publish();
            }
            Cmd::VideoTarget(t) => {
                if t != self.target {
                    self.target = t;
                    self.target_changed = Instant::now();
                }
            }
            Cmd::DismissNotice => {
                self.notice = None;
                self.publish();
            }
            Cmd::Shutdown => {}
        }
    }

    fn with_player<T: Copy>(&mut self, f: impl Fn(&mut Player, T) -> std::io::Result<()>, arg: T) {
        if let Some(player) = self.player.as_mut() {
            if f(player, arg).is_err() {
                self.player = None;
                self.fail(ErrorKind::Other, "Lost contact with the audio engine (mpv).".into());
            }
        }
    }

    fn seek(&mut self, to: f64) {
        if self.current.is_none() || self.live() {
            return;
        }
        let max = self.duration.unwrap_or(f64::MAX);
        let to = to.clamp(0.0, (max - 0.5).max(0.0));
        // Until mpv says, count on the sound it had past the new place, and on
        // nothing behind (it may still have some).
        let skip = to - self.clock.now();
        if (0.0..=self.audio.ahead).contains(&skip) || self.audio.done && skip >= 0.0 {
            self.audio.ahead = (self.audio.ahead - skip).max(0.0);
        } else {
            self.audio.ahead = 0.0;
            self.audio.done = false;
        }
        self.audio_from = Instant::now();
        self.clock.set(to);
        if self.refresh_at.is_some() {
            // Fresh links are on their way; start there once they arrive.
            self.refresh_at = Some(to);
            return;
        }
        // A seek opens new connections, which expired links would get refused.
        if self.links_expired() && self.refresh(to) {
            return;
        }
        self.restart_video = true;
        // The sound stops for a moment anyway: a good time for a better stream.
        let better = self.pick_audio();
        if better > self.audio_index && self.loaded {
            self.switch_audio(better, to);
            return;
        }
        self.with_player(|p, t| p.seek(t), to);
    }

    fn pause(&mut self) {
        self.paused = true;
        self.paused_since = Some(Instant::now());
        self.holding = None;
        self.with_player(|p, paused| p.set_pause(paused), true);
        self.update_clock();
        self.publish();
    }

    /// Plays on after a pause, which mpv may already have done by itself (media keys).
    fn resume(&mut self) {
        let long = self.paused_since.take().is_some_and(|t| t.elapsed() > STALE_PAUSE);
        self.paused = false;
        if self.links_expired() && self.refresh(self.clock.now()) {
            return;
        }
        if long && self.video_segmented() {
            if self.live() {
                // The paused downloads are cut off and a live stream has moved on:
                // rejoin it live, sound and picture together.
                self.loaded = false;
                self.load_current(None);
            } else {
                // The paused decoder would skip the rest of its cut-off segment: start a
                // fresh one at the same spot and hold the sound until it has the picture.
                self.stop_video();
                self.holding = Some(Instant::now());
                self.with_player(|p, paused| p.set_pause(paused), true);
            }
            self.update_clock();
            self.publish();
            return;
        }
        self.with_player(|p, paused| p.set_pause(paused), false);
        self.update_clock();
        self.publish();
    }

    /// Lets the sound go once the picture is back (or it took too long).
    fn release_hold(&mut self) {
        let Some(since) = self.holding else { return };
        let ready = self.video.as_ref().is_none_or(|v| v.frames_shown() > 0);
        if ready || since.elapsed() > HOLD_MAX {
            self.holding = None;
            self.with_player(|p, paused| p.set_pause(paused), false);
            self.publish();
        }
    }

    fn live(&self) -> bool {
        self.current.as_ref().is_some_and(|r| r.track.live)
    }

    /// Whether the stream links have stopped working, or are about to.
    fn links_expired(&self) -> bool {
        let soon = std::time::SystemTime::now() + LINK_MARGIN;
        self.current.as_ref().and_then(|r| r.expires_at).is_some_and(|at| at < soon)
    }

    /// Whether the picture on screen comes from an HLS stream.
    fn video_segmented(&self) -> bool {
        let (Some(r), Some(v)) = (&self.current, &self.video) else { return false };
        r.video.iter().any(|s| s.url == v.stream_url && s.segmented)
    }

    /// Within the last seconds of the track, where the video stream may just end.
    fn near_end(&self) -> bool {
        !self.live() && self.duration.is_none_or(|d| self.clock.now() > d - 3.0)
    }

    // --- queue --------------------------------------------------------------------

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// Recomputes the play order, keeping the current track current.
    fn rebuild_order(&mut self) {
        let n = self.queue.len();
        self.order = (0..n).collect();
        if self.shuffle {
            for i in (1..n).rev() {
                let j = (self.rand() % (i as u64 + 1)) as usize;
                self.order.swap(i, j);
            }
            if let Some(cur) = self.index {
                if let Some(p) = self.order.iter().position(|&x| x == cur) {
                    self.order.swap(0, p);
                }
            }
            self.pos = 0;
        } else {
            self.pos = self.index.unwrap_or(0).min(n.saturating_sub(1));
        }
    }

    fn remove(&mut self, i: usize) {
        if i >= self.queue.len() {
            return;
        }
        let removing_current = self.index == Some(i);
        self.queue.remove(i);
        self.order.retain(|&x| x != i);
        for x in &mut self.order {
            if *x > i {
                *x -= 1;
            }
        }
        if self.queue.is_empty() {
            self.command(Cmd::ClearQueue);
            return;
        }
        if removing_current {
            self.pos = self.pos.min(self.order.len() - 1);
            self.play_pos(self.pos);
        } else {
            if let Some(cur) = self.index {
                self.index = Some(if cur > i { cur - 1 } else { cur });
            }
            if let Some(p) = self.index.and_then(|c| self.order.iter().position(|&x| x == c)) {
                self.pos = p;
            }
            self.publish();
        }
    }

    fn next(&mut self, auto: bool) {
        let n = self.order.len();
        if n == 0 {
            return;
        }
        if auto && self.repeat == Repeat::One {
            self.play_pos(self.pos);
        } else if self.pos + 1 < n {
            self.play_pos(self.pos + 1);
        } else if self.repeat == Repeat::All {
            if self.shuffle {
                self.index = None;
                self.rebuild_order();
            }
            self.play_pos(0);
        } else if auto {
            self.finish();
        }
    }

    fn prev(&mut self) {
        if self.order.is_empty() {
            return;
        }
        if self.clock.now() > 3.0 || self.pos == 0 && self.repeat != Repeat::All {
            self.seek(0.0);
        } else if self.pos == 0 {
            self.play_pos(self.order.len() - 1);
        } else {
            self.play_pos(self.pos - 1);
        }
    }

    /// The queue ran out.
    fn finish(&mut self) {
        self.stopped = true;
        self.loaded = false;
        self.sound = None;
        self.stop_video();
        self.frames.release();
        self.clock.set_running(false);
        self.publish();
    }

    // --- playing a track ------------------------------------------------------------

    fn reset_playback(&mut self) {
        self.current = None;
        self.loaded = false;
        self.stopped = false;
        self.duration = None;
        self.buffering = false;
        self.paused_since = None;
        self.holding = None;
        self.refresh_at = None;
        self.video_retry = None;
        self.video_failures = 0;
        self.stop_video();
        self.video_failed = None;
        self.restart_video = false;
        self.audio = AudioCache::default();
        self.audio_trouble = None;
        self.audio_reload = false;
        self.hold.store(false, Ordering::Relaxed);
        self.want = None;
        self.switched = None;
        self.resynced = None;
        self.lagging = None;
        self.frames.clear();
        self.clock.set(0.0);
        self.clock.set_running(false);
        if let Some(p) = self.player.as_mut() {
            let _ = p.stop();
        }
        self.sound = None;
    }

    fn play_pos(&mut self, pos: usize) {
        let Some(&index) = self.order.get(pos) else { return };
        let Some(track) = self.queue.get(index).cloned() else { return };
        self.pos = pos;
        self.index = Some(index);
        self.paused = false;
        self.gen += 1;
        self.reset_playback();
        self.duration = track.duration.map(|d| d as f64);
        self.notice = None;
        self.publish();

        if let Some((at, resolved)) = self.prefetch.remove(&track.id) {
            if at.elapsed() < PREFETCH_TTL {
                if ready_in(&resolved).is_zero() {
                    self.on_resolved(Ok(resolved));
                } else {
                    let (tx, gen) = (self.tx.clone(), self.gen);
                    std::thread::spawn(move || {
                        std::thread::sleep(ready_in(&resolved));
                        let _ = tx.send(Msg::Resolved { gen, result: Ok(resolved), took: Duration::ZERO });
                    });
                }
                return;
            }
        }
        self.spawn_resolve(track, self.gen);
    }

    fn spawn_resolve(&self, track: Track, gen: u64) {
        let (provider, tx) = (self.provider.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = provider.resolve(&track);
            let took = started.elapsed();
            // Handing mpv a stream before it is served only gets refusals.
            if let Ok(resolved) = &result {
                std::thread::sleep(ready_in(resolved));
            }
            let _ = tx.send(Msg::Resolved { gen, result, took });
        });
    }

    fn ensure_player(&mut self) -> Result<(), String> {
        if self.player.is_some() {
            return Ok(());
        }
        platform::reap_stale_players();
        // Looked up again each time so installing mpv while the app runs just works.
        self.tools = Tools::discover();
        let mpv = self.tools.mpv.clone().ok_or_else(|| {
            format!("mpv is not installed. Install it with: {}", platform::install_hint())
        })?;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let player = Player::spawn(&mpv, &self.mpv_args, self.volume, self.muted, move |ev| {
            let _ = tx.send(Msg::Player(ev));
            waker();
        })?;
        self.player = Some(player);
        // A new mpv downloads as far ahead as its cache holds.
        self.readahead = None;
        Ok(())
    }

    fn on_resolved(&mut self, result: Result<Resolved, ProviderError>) {
        let resolved = match result {
            Ok(r) => r,
            Err(e) => {
                self.track_failed(e.kind, e.message);
                return;
            }
        };
        if let Err(msg) = self.ensure_player() {
            self.fail(ErrorKind::Missing, msg);
            return;
        }
        // Fill in metadata that a pasted link did not have.
        if let Some(slot) = self.index.and_then(|i| self.queue.get_mut(i)) {
            let keep_thumb = slot.thumbnail.clone();
            *slot = resolved.track.clone();
            slot.thumbnail = keep_thumb.or_else(|| resolved.track.thumbnail.clone());
        }
        self.duration = resolved.track.duration.map(|d| d as f64).or(self.duration);
        self.current = Some(resolved);
        self.audio_index = self.pick_audio();
        // Looked up once the sound is safely buffered (see `maybe_prefetch`).
        self.prefetched = false;
        let start = self.refresh_at.take();
        self.load_current(start);
        self.publish();
    }

    /// Hands the current track's sound to mpv, from `start` (`None`: from the
    /// beginning, or the live edge). The picture follows once it plays.
    fn load_current(&mut self, start: Option<f64>) {
        let Some(r) = &self.current else { return };
        let audio = r.audio[self.audio_index.min(r.audio.len() - 1)].clone();
        let ua = audio.headers.iter().find(|h| h.name.eq_ignore_ascii_case("user-agent")).map(|h| h.value.clone());
        let (title, live, paused) = (r.track.title.clone(), r.track.live, self.paused);
        // A single file comes through the relay, which paces and measures the
        // download; mpv reads anything else (live HLS) itself.
        self.sound = None;
        if let Some(size) = audio.size.filter(|_| !live) {
            self.sound = self.ensure_relay().map(|relay| relay.open(&audio.url, &audio.headers, audio.bitrate, size, true));
        }
        let (url, feed) = match &self.sound {
            Some(source) => (source.url().to_string(), mpv::Feed::Relay),
            None => (audio.url.clone(), if live { mpv::Feed::Live } else { mpv::Feed::Site }),
        };
        self.audio = AudioCache::default();
        self.audio_trouble = None;
        self.audio_from = Instant::now();
        self.with_player(|p, _: ()| p.load(&url, &title, ua.as_deref(), feed, paused, start), ());
    }

    /// The relay, started on first use (`None` if it could not be).
    fn ensure_relay(&mut self) -> Option<&relay::Relay> {
        if self.relay.is_none() && !self.relay_failed {
            match relay::Relay::start(self.meter.clone(), self.hold.clone()) {
                Ok(r) => self.relay = Some(r),
                Err(_) => self.relay_failed = true,
            }
        }
        self.relay.as_ref()
    }

    /// The sound stream for the line as measured: one that leaves room for the
    /// picture, and none bigger than the line has kept up with.
    fn pick_audio(&self) -> usize {
        let Some(r) = &self.current else { return 0 };
        let mut i = abr::choose_audio(&r.audio, self.line_estimate());
        if let Some(cap) = self.audio_ceiling {
            while i > 0 && r.audio[i].bitrate > cap {
                i -= 1;
            }
        }
        i
    }

    /// Carries on from `at` with another size of the same sound. The picture
    /// stays on screen meanwhile.
    fn switch_audio(&mut self, index: usize, at: f64) {
        self.audio_index = index;
        self.audio_reload = true;
        self.loaded = false;
        self.load_current(Some(at));
        self.update_clock();
        self.publish();
    }

    /// When the line cannot keep up with the sound, plays a smaller stream: at once
    /// if the sound is about to run dry, otherwise from the next track on.
    fn check_audio(&mut self) {
        let Some(r) = &self.current else { return };
        let Some(cur) = r.audio.get(self.audio_index) else { return };
        let a = &self.audio;
        let seen = a.rate.unwrap_or(0.0).max(self.meter.estimate().unwrap_or(0.0));
        // The line got faster: allow the next size up again.
        if let Some(cap) = self.audio_ceiling {
            if let Some(i) = r.audio.iter().position(|s| s.bitrate > cap) {
                if seen >= r.audio[i].bitrate as f64 * 1000.0 * 3.0 {
                    self.audio_ceiling = (i + 1 < r.audio.len()).then(|| r.audio[i].bitrate);
                }
            }
        }
        let need = cur.bitrate as f64 * 1000.0;
        let rate = a.rate.unwrap_or(f64::INFINITY);
        let settled = self.audio_from.elapsed() >= AUDIO_SETTLE;
        let slow = settled && !self.paused && !a.done && a.ahead < AUDIO_LOW && need > 0.0 && rate < need * 1.25;
        if !slow {
            self.audio_trouble = None;
            return;
        }
        // Far too slow is clear sooner.
        let enough = if rate < need * 0.8 { AUDIO_TROUBLE / 2 } else { AUDIO_TROUBLE };
        if self.audio_trouble.get_or_insert_with(Instant::now).elapsed() < enough {
            return;
        }
        self.audio_trouble = None;
        // What mpv gets is the sound's share of the line; with video on, leave the
        // picture its part.
        let rate = a.rate.unwrap_or(0.0);
        let share = if self.mode == Mode::Video { 0.45 } else { 0.7 };
        let fit = r.audio.iter().rposition(|s| s.bitrate as f64 * 1000.0 <= rate * share).unwrap_or(0);
        if fit < self.audio_index {
            self.audio_ceiling = Some(r.audio[fit].bitrate);
            if a.ahead < HOLD_BELOW {
                self.switch_audio(fit, self.clock.now());
            }
        }
    }

    /// Left alone, mpv takes the whole line to download minutes of sound ahead,
    /// and on a slow line the picture starves meanwhile. With video on, the sound
    /// stays `AUDIO_OVER_VIDEO` ahead of the picture's buffer, so the two fill up
    /// together, and goes as far as it likes once the picture's buffer is full:
    /// then the line has room to spare.
    fn update_readahead(&mut self) {
        let video = if self.mode == Mode::Video && !self.live() { self.video_next.as_ref().or(self.video.as_ref()) } else { None };
        let secs = match video {
            None => None,
            Some(v) if v.buffer_full() => None,
            Some(v) => match v.buffered() {
                Some(b) => Some((((b.min(60.0) + AUDIO_OVER_VIDEO) / 5.0).round() as u32 * 5).max(MIN_READAHEAD)),
                // HLS, which ffmpeg reads itself: only the line's speed is known.
                None => self.line_estimate().is_some_and(|e| e < SLOW_LINE).then_some(SLOW_LINE_READAHEAD),
            },
        };
        if secs != self.readahead && self.player.is_some() {
            self.readahead = secs;
            self.with_player(|p, secs| p.set_readahead(secs), secs);
        }
        // The relay keeps to it as well: it hands over more once mpv's cache is a
        // little below the mark, so mpv reads it at once rather than leaving it
        // in the connection.
        if let Some(sound) = &self.sound {
            let a = &self.audio;
            sound.set_player_full(secs.is_some_and(|s| a.known && !a.done && a.ahead >= s as f64 - SOUND_REFILL));
        }
    }

    /// Video downloads give way while the sound runs low.
    fn update_hold(&mut self) {
        let a = &self.audio;
        let held = self.hold.load(Ordering::Relaxed);
        let hold = self.loaded && a.known && !a.done && !self.live() && a.ahead < if held { HOLD_UNTIL } else { HOLD_BELOW };
        if hold != held {
            self.hold.store(hold, Ordering::Relaxed);
        }
    }

    /// Looks the next track up once the sound is safe (see `PREFETCH_AHEAD`).
    fn maybe_prefetch(&mut self) {
        if self.prefetched || !self.loaded {
            return;
        }
        // Time for a lookup as slow as the last one, twice over.
        let window = (self.lookup_took.as_secs_f64() * 2.0 + 10.0).max(60.0);
        let near_end = self.duration.is_some_and(|d| d - self.clock.now() < window);
        if self.audio.done || self.audio.ahead >= PREFETCH_AHEAD || near_end {
            self.prefetched = true;
            self.prefetch_next();
        }
    }

    /// Looks the current track up again for fresh stream links (they expire, and
    /// stop working when the network changes) and carries on from `at` with them.
    /// Returns false when the track got fresh links only a little while ago.
    fn refresh(&mut self, at: f64) -> bool {
        let Some(track) = self.index.and_then(|i| self.queue.get(i)).cloned() else { return false };
        if self.refreshed.as_ref().is_some_and(|(id, when)| *id == track.id && when.elapsed() < REFRESH_EVERY) {
            return false;
        }
        self.refreshed = Some((track.id.clone(), Instant::now()));
        self.refresh_at = (!self.live()).then_some(at);
        self.loaded = false;
        self.holding = None;
        self.video_failed = None;
        self.video_retry = None;
        self.video_failures = 0;
        self.gen += 1;
        self.spawn_resolve(track, self.gen);
        self.update_clock();
        self.publish();
        true
    }

    /// Looks up the next track in the background so it starts without a gap.
    fn prefetch_next(&mut self) {
        if self.repeat == Repeat::One || self.live() {
            return;
        }
        let next = match self.order.get(self.pos + 1).or_else(|| (self.repeat == Repeat::All).then(|| self.order.first()).flatten()) {
            Some(&i) => self.queue[i].clone(),
            None => return,
        };
        if self.prefetch.contains_key(&next.id) {
            return;
        }
        let (provider, tx) = (self.provider.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = provider.resolve(&next);
            let _ = tx.send(Msg::Prefetched { id: next.id.clone(), result, took: started.elapsed() });
        });
    }

    /// A track could not be played. Skip it when it is just that video's problem.
    fn track_failed(&mut self, kind: ErrorKind, message: String) {
        self.failures += 1;
        let title = self.index.and_then(|i| self.queue.get(i)).map(|t| t.title.clone()).unwrap_or_default();
        let skippable = matches!(kind, ErrorKind::Unavailable | ErrorKind::Other)
            && self.pos + 1 < self.order.len()
            && self.failures < 5;
        self.set_notice(kind, if skippable { format!("Skipped \u{201c}{title}\u{201d}: {message}") } else { message });
        if skippable {
            let notice = self.notice.take();
            self.next(true);
            self.notice = notice;
            self.publish();
        } else {
            self.reset_playback();
            self.stopped = true;
            self.publish();
        }
    }

    fn fail(&mut self, kind: ErrorKind, message: String) {
        self.set_notice(kind, message);
        self.reset_playback();
        self.stopped = self.index.is_some();
        self.publish();
    }

    fn set_notice(&mut self, kind: ErrorKind, message: String) {
        self.notice_id += 1;
        self.notice = Some(Notice { id: self.notice_id, message, kind });
    }

    // --- mpv events -----------------------------------------------------------------

    fn player_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::TimePos(t) => {
                // Positions before `file-loaded` still belong to the previous file.
                if self.loaded {
                    self.clock.set(t);
                }
                return;
            }
            PlayerEvent::Duration(d) => {
                if !self.live() {
                    self.duration = Some(d);
                }
            }
            // Either our own command coming back, or mpv acting on media keys. While the
            // sound is held, mpv is paused on purpose.
            PlayerEvent::Pause(p) if p != self.paused && self.holding.is_none() => {
                if p {
                    self.pause();
                } else {
                    self.resume();
                }
                return;
            }
            PlayerEvent::Pause(_) => {}
            PlayerEvent::CoreIdle(i) => self.core_idle = i,
            PlayerEvent::Buffering(b) => self.buffering = b,
            PlayerEvent::Cache { ahead, done, rate } => {
                // Reports before `file-loaded` still belong to the previous file.
                if self.loaded {
                    self.audio = AudioCache { ahead, done, rate, known: true };
                    // The relay counts what it fetches for the sound itself. What mpv
                    // downloads on its own shares the line: with its cache full, mpv tops
                    // it up as the sound plays, about the sound's bitrate, in short
                    // bursts that show as idle.
                    let topping_up = !done && !self.paused && self.readahead.is_none_or(|secs| ahead < secs as f64 + 5.0);
                    let kbps = self.current.as_ref().and_then(|r| r.audio.get(self.audio_index)).map_or(0, |a| a.bitrate);
                    let own = rate.or(topping_up.then_some(kbps as f64 * 1000.0));
                    self.meter.audio(own.filter(|_| self.sound.is_none()));
                    self.check_audio();
                }
                return;
            }
            PlayerEvent::FileLoaded => {
                self.loaded = true;
                self.audio_reload = false;
                self.failures = 0;
            }
            PlayerEvent::EndFile { eof, error } => {
                if self.current.is_none() {
                    return;
                }
                // mpv reports a stream that broke off for good (an expired link refusing
                // the reconnect) as a normal end, just early.
                let at = self.clock.now();
                let early = eof && !self.live() && self.duration.is_some_and(|d| at < d - 5.0);
                if error.is_some() || early {
                    // Stream links expire; carry on with fresh ones before giving up.
                    if self.refresh(at) {
                        return;
                    }
                    let error = error.unwrap_or_else(|| "the stream broke off".into());
                    self.track_failed(ErrorKind::Other, format!("Playback failed: {error}"));
                } else if eof {
                    self.next(true);
                }
                return;
            }
            PlayerEvent::Exited => {
                // The reader thread of an old mpv can report after we replaced it.
                if self.player.as_mut().is_some_and(|p| !p.alive()) {
                    self.player = None;
                    self.fail(ErrorKind::Other, "The audio engine (mpv) stopped unexpectedly.".into());
                }
                return;
            }
        }
        self.update_clock();
        self.publish();
    }

    fn running(&self) -> bool {
        self.loaded && !self.paused && !self.core_idle && !self.buffering
    }

    fn update_clock(&self) {
        self.clock.set_running(self.running());
    }

    fn status(&self) -> Status {
        if self.index.is_none() {
            Status::Idle
        } else if self.stopped {
            Status::Stopped
        } else if !self.loaded {
            Status::Loading
        } else if self.paused {
            Status::Paused
        } else if self.core_idle || self.buffering {
            Status::Buffering
        } else {
            Status::Playing
        }
    }

    // --- video ----------------------------------------------------------------------

    /// Stops both decoders. Returns whether one was running.
    fn stop_video(&mut self) -> bool {
        let running = self.video.is_some() || self.video_next.is_some();
        self.video = None;
        self.video_next = None;
        running
    }

    /// Starts, restarts or stops the ffmpeg decoder to match what is on screen.
    fn reconcile_video(&mut self) {
        // Audio mode, or nothing to show: no decoder at all.
        let video_streams = self.current.as_ref().map(|r| !r.video.is_empty()).unwrap_or(false);
        if self.mode != Mode::Video || !(self.loaded || self.audio_reload) || !video_streams {
            if self.stop_video() {
                self.frames.release();
                self.publish();
            }
            return;
        }
        let Some((tw, th)) = self.target else {
            // Hidden (other tab, minimised): keep the decoder briefly, in case it comes back.
            let since = *self.hidden_since.get_or_insert_with(Instant::now);
            if since.elapsed() > HIDDEN_GRACE && self.stop_video() {
                self.frames.release();
                self.publish();
            }
            return;
        };
        self.hidden_since = None;

        // The stream the line can carry, and the height the window (or the user)
        // asks for when the line is no limit.
        self.update_ceiling();
        let budget = self.video_budget();
        // Until the line is measured, the lookup's guess may lower the first pick
        // but not raise it: an opening can need more than the stream's average,
        // and the sound fills its buffer beside it.
        let pick = match budget {
            Some(b) if self.quality.is_none() && !self.live() && self.meter.estimate().is_none() => Some(b.min(abr::UNKNOWN_START)),
            b => b,
        };
        let (track_id, live, stream, size_height) = {
            let r = self.current.as_ref().unwrap();
            let capped = self.height_cap.filter(|_| self.quality.is_none()).map_or(th, |(h, _)| th.min(h));
            let want = abr::choose_video(&r.video, self.quality, capped, pick);
            let full = abr::choose_video(&r.video, self.quality, th, Some(f64::INFINITY));
            (r.track.id.clone(), r.track.live, r.video[want].clone(), r.video[full].height)
        };
        if self.video_failed.as_deref() == Some(track_id.as_str()) {
            return;
        }
        if self.want.as_ref().is_none_or(|(url, _)| *url != stream.url) {
            self.want = Some((stream.url.clone(), Instant::now()));
        }
        let (sw, sh) = (stream.width.max(1) as f64, stream.height.max(1) as f64);
        // Fit the picture into the target box, never above the source size.
        let scale = (tw as f64 / sw).min(th as f64 / sh).clamp(0.05, 1.0);
        let mut w = ((sw * scale) as u32).clamp(64, 1920);
        let mut h = ((sh * scale) as u32).clamp(36, 1080);
        w &= !1;
        h &= !1;

        // How long a decoder takes to deliver its first frame: longer on a busy computer.
        if let Some(t) = self.video.as_ref().and_then(|v| v.startup()) {
            self.video_startup = t;
        }
        // A replacement decoder takes over once its frames are on screen. One that
        // dies or never catches up is dropped, and the current one carries on.
        if let Some(next) = &self.video_next {
            if next.frames_shown() > 0 {
                if let Some(t) = next.startup() {
                    self.video_startup = t;
                }
                let up = self.video.as_ref().is_some_and(|v| next.kbps > v.kbps);
                self.video = self.video_next.take();
                self.video_failures = 0;
                self.head_start = false;
                if up {
                    self.last_up = Some(Instant::now());
                }
                self.publish();
            } else if self.gpu_failed(next) {
                self.gpu_refused.push(next.codec.clone());
                self.video_next = None;
            } else if next.finished() || next.refused() {
                self.video_next = None;
                self.video_retry = Some(Instant::now());
                self.video_failures += 1;
            } else {
                // Through the relay, a slow line only makes it take longer; it is
                // dropped (and tried again later) once no data comes at all.
                let relayed = next.buffered().is_some();
                let stuck = if relayed {
                    next.age() > NEXT_VIDEO_GIVE_UP || next.starved_for() > NEXT_VIDEO_TIMEOUT
                } else {
                    next.age() > NEXT_VIDEO_TIMEOUT
                };
                if stuck {
                    self.video_next = None;
                    self.video_retry = Some(Instant::now());
                    if !relayed {
                        self.video_failures += 1;
                    }
                }
            }
        }
        // A restart meant to begin ahead of playback whose frames still come out
        // late, for want of data, on a line too slow to catch up: it needs a longer
        // head start. One the line can feed catches up by itself.
        let now = self.clock.now();
        let fed = |kbps: u32| budget.is_none_or(|b| b >= kbps as f64 * 1000.0);
        if let Some(next) = &self.video_next {
            let late = now > next.start + 1.0 && next.buffered().is_some_and(|b| b < 1.0);
            if next.frames_shown() == 0 && late && !fed(next.kbps) && !self.paused {
                self.video_next = None;
                if self.head_start {
                    self.late_restarts = (self.late_restarts + 1).min(4);
                }
                self.head_start = true;
                self.resynced = None;
            }
        }
        // A replacement whose reason went away before it showed anything: a bigger
        // stream the line no longer carries, or a smaller one now that the line
        // carries the current one again (and that one still plays: a picture that
        // stands still gets back soonest with the replacement on its way).
        if let (Some(next), Some(cur)) = (&self.video_next, &self.video) {
            let up = next.kbps > cur.kbps;
            let gone = if up {
                !fed(next.kbps)
            } else {
                let playing = cur.behind(now).is_some_and(|b| b < 0.5);
                next.kbps < cur.kbps && playing && cur.stream_height <= size_height && budget.is_some_and(|b| b >= cur.kbps as f64 * 1000.0 * 1.25)
            };
            if gone && next.frames_shown() == 0 && !cur.finished() && !self.paused {
                self.video_next = None;
                if up {
                    self.up_failures = (self.up_failures + 1).min(4);
                }
            }
        }
        // A first picture set to begin far ahead on the lookup's guess of the line
        // (a slow lookup is as often a busy computer as a slow line): once the line
        // is measured and has it begin much sooner, it starts again from there.
        if let Some(cur) = &self.video {
            let ahead = cur.start - now;
            if cur.frames_shown() == 0 && ahead > 5.0 && self.meter.estimate().is_some() && !self.paused && self.lead(&stream, budget, true, false) < ahead - 5.0 {
                self.restart_video = true;
            }
        }
        if self.video.as_ref().is_some_and(|v| v.behind(now).is_some_and(|b| b > 1.0)) {
            self.fell_behind = Some(Instant::now());
        } else if self.fell_behind.is_none_or(|t| t.elapsed() > RESYNC_RESET) {
            self.late_restarts = 0;
        }
        // While a replacement downloads, the one on screen stops downloading if it
        // has a few seconds in hand, or falls behind anyway, so the replacement gets
        // the line. Once the replacement has its fill, the line is free again.
        if let Some(cur) = &self.video {
            let spare = cur.buffered().is_some_and(|b| b > 3.0) || cur.behind(now).is_some_and(|b| b > 1.0) || !fed(cur.kbps);
            let fetching = self.video_next.as_ref().is_some_and(|n| !n.buffer_full());
            cur.set_standby(fetching && spare);
        }

        if let Some(session) = self.video.as_ref().filter(|s| self.gpu_failed(s)) {
            self.gpu_refused.push(session.codec.clone());
            self.restart_video = true;
        }
        // A decoder that died without showing anything, or could not be replaced
        // after it died: do not retry in a loop.
        if let Some(session) = self.video.as_ref().filter(|_| !self.restart_video) {
            let dead = session.finished() && self.video_next.is_none();
            if dead && (session.frames_shown() == 0 || self.video_failures >= VIDEO_ATTEMPTS) {
                self.video = None;
                self.video_failed = Some(track_id);
                self.set_notice(ErrorKind::Other, "Could not decode the video stream. Audio keeps playing.".into());
                self.publish();
                return;
            }
        }

        // Another track, or a seek: the old picture is useless, replace it at once.
        // A new size, quality or stream, or a decoder that gave up mid-stream (on a
        // broken connection it could not resume): keep showing the old picture
        // until the new one is ready.
        let hard = match &self.video {
            None => true,
            Some(s) => s.track_id != track_id || self.restart_video,
        };
        let may_retry = self.video_failures < VIDEO_ATTEMPTS && self.video_retry.is_none_or(|t| t.elapsed() > VIDEO_RETRY);
        let soft = !hard && may_retry && self.switch_wanted(&stream, size_height, budget, w);
        // A smaller stream for a decoder that has shown nothing yet: there is no
        // picture to keep up meanwhile, so the new one takes over (and the line) now.
        let hard = hard || (soft && self.video_next.is_none() && self.video.as_ref().is_some_and(|v| v.frames_shown() == 0 && stream.bitrate < v.kbps));
        if !hard && !soft {
            return;
        }
        let Some(ffmpeg) = self.tools.ffmpeg.clone().or_else(|| {
            self.tools.ffmpeg = platform::find_executable("ffmpeg");
            self.tools.ffmpeg.clone()
        }) else {
            self.video_failed = Some(track_id);
            self.set_notice(
                ErrorKind::Missing,
                format!("Video needs ffmpeg. Install it with: {}", platform::install_hint()),
            );
            self.publish();
            return;
        };
        self.restart_video = false;
        // A replacement for a picture that fell behind for want of data.
        let resync = !hard && self.video.as_ref().is_some_and(|v| v.behind(self.clock.now()).is_some_and(|b| b > RESYNC_BEHIND));
        let lead = self.lead(&stream, budget, hard, resync);
        if hard {
            self.stop_video();
            self.frames.clear();
        } else if let Some(cur) = &self.video {
            if resync {
                self.resynced = Some(Instant::now());
            }
            // A step down soon after a step up: that one was too much.
            if stream.bitrate < cur.kbps && self.last_up.is_some_and(|t| t.elapsed() < UP_UNDONE) {
                self.up_failures = (self.up_failures + 1).min(4);
                self.last_up = None;
            }
        }
        if stream.size.is_some() {
            self.ensure_relay();
        }
        let gpu = platform::gpu_video(&stream.codec).filter(|gpu| !self.gpu_refused.contains(&stream.codec) && video::gpu_ready(&ffmpeg, gpu));
        self.video_session += 1;
        self.switched = Some(Instant::now());
        let result = VideoSession::start(
            video::Params {
                ffmpeg: &ffmpeg,
                url: &stream.url,
                headers: &stream.headers,
                start: self.clock.now() + lead,
                live,
                width: w,
                height: h,
                codec: &stream.codec,
                kbps: stream.bitrate,
                relay: self.relay.as_ref().zip(stream.size),
                gpu: gpu.as_ref(),
            },
            self.video_session,
            track_id.clone(),
            stream.height,
            self.clock.clone(),
            self.frames.clone(),
            self.waker.clone(),
        );
        match result {
            Ok(session) if hard => self.video = Some(session),
            Ok(session) => self.video_next = Some(session),
            Err(e) => {
                self.stop_video();
                self.video_failed = Some(track_id);
                self.set_notice(ErrorKind::Other, e);
            }
        }
        self.publish();
    }

    /// A decoder on the graphics chip that ended without a frame, although its data
    /// came and the track goes on: the chip refused the stream (an unusual profile,
    /// or a codec it does not decode after all).
    fn gpu_failed(&self, session: &VideoSession) -> bool {
        session.gpu && session.finished() && session.frames_shown() == 0 && !session.refused() && !self.near_end()
    }

    /// How far ahead of playback a decoder for `stream` starts: at the point playback
    /// will have reached when its first frame is decoded, so it joins in step instead
    /// of catching up. Further when its data will be slow to come: when the line
    /// cannot keep it fed, after one came out late, or after a picture fell behind
    /// for want of data (`resync`) on a line without plenty to spare.
    fn lead(&self, stream: &crate::provider::VideoStream, budget: Option<f64>, hard: bool, resync: bool) -> f64 {
        if self.paused || self.live() {
            return 0.0;
        }
        let rate = stream.bitrate as f64 * 1000.0;
        // Video gets the line only while the sound has enough: count on less, unless
        // minutes of sound are in hand.
        let spare = self.audio.done || self.audio.ahead >= AUDIO_SPARE;
        let line = if spare { abr::video_budget(self.line_estimate(), 0, true) } else { budget };
        // Seconds to fetch the stream's header and `secs` of it.
        let fetch = |secs: f64| (RESYNC_HEADER_BITS + rate * secs) / (line.unwrap_or(0.0).max(rate * 0.25) * 0.8);
        let usual = if hard {
            // Its data waits until the sound has enough to start on (see `update_hold`),
            // then come the stream's header and the media from the key frame before
            // the start point. On a slow line playback is well on by then: start
            // there rather than catch up.
            let kbps = self.current.as_ref().and_then(|r| r.audio.get(self.audio_index)).map_or(0, |a| a.bitrate);
            let sound = if self.audio.done { 0.0 } else { (HOLD_UNTIL - self.audio.ahead).max(0.0) * kbps as f64 * 1000.0 };
            let wait = match (abr::video_budget(self.line_estimate(), 0, true), line) {
                (Some(all), Some(video)) => sound / all + (RESYNC_HEADER_BITS + rate * FRAGMENT / 2.0) / video.max(rate * 0.25),
                _ => 0.0,
            };
            // ffmpeg itself takes a moment, seconds on a busy computer; frames that
            // come out late are dropped until it catches up, so start where playback
            // will be by then.
            let ready = self.video_startup.as_secs_f64() * 1.2;
            let ready = if ready < 1.5 { 0.0 } else { ready.min(8.0) };
            if wait < 3.0 { ready } else { wait.min(MAX_RESYNC_LEAD).max(ready) }
        } else {
            // Its first frame needs the fragment that holds the start point.
            let first = if line.is_some() { fetch(FRAGMENT).min(MAX_RESYNC_LEAD) } else { 0.0 };
            (self.video_startup.as_secs_f64() * 1.5 + 0.25).clamp(0.5, 8.0).max(first)
        };
        let tight = budget.is_some_and(|b| b < rate * if resync { 2.0 } else { 1.0 });
        if !(self.head_start || tight) {
            return usual;
        }
        (fetch(RESYNC_PLAY) * (1 << self.late_restarts) as f64).clamp(usual, MAX_RESYNC_LEAD)
    }

    /// The line's speed as measured, or until then as the lookup suggested.
    fn line_estimate(&self) -> Option<f64> {
        self.meter.estimate().or_else(|| self.current.as_ref().and_then(|r| r.line_hint))
    }

    /// Bits per second the line can spare for the picture, once measured. Live
    /// streams (HLS, which ffmpeg reads itself) are not measured and go by size.
    /// Either way no more than a picture that fell behind could manage.
    fn video_budget(&self) -> Option<f64> {
        let kbps = self.current.as_ref().and_then(|r| r.audio.get(self.audio_index)).map_or(0, |a| a.bitrate);
        // With little of the picture in hand, a sharp drop counts at once: each
        // step down on the way while the average follows costs another start.
        let short = self.video.as_ref().is_some_and(|v| v.frames_shown() > 0 && v.buffered().is_some_and(|b| b < DOWN_AT));
        let estimate = if short { self.meter.estimate_now() } else { None }.or_else(|| self.line_estimate());
        let budget = if self.live() { Some(f64::INFINITY) } else { abr::video_budget(estimate, kbps, self.audio.done) };
        match self.ceiling {
            Some((most, _)) => Some(budget.unwrap_or(most).min(most)),
            None => budget,
        }
    }

    /// A picture that keeps falling behind for want of data on a line that should
    /// carry its stream (a passage that needs more than the stream's average) caps
    /// the bitrate below its stream's for a while. One that falls behind although
    /// its data is there (a computer too busy to decode it in time, or HLS, where the
    /// data is not seen) caps the height instead: on Auto the next smaller picture,
    /// in H.264 where there is one, which is quicker to decode than a smaller VP9 of
    /// the same height.
    fn update_ceiling(&mut self) {
        if self.ceiling.is_some_and(|(_, t)| t.elapsed() > CEILING_FOR) {
            self.ceiling = None;
        }
        if self.height_cap.is_some_and(|(_, t)| t.elapsed() > CEILING_FOR) {
            self.height_cap = None;
        }
        let kbps = self.current.as_ref().and_then(|r| r.audio.get(self.audio_index)).map_or(0, |a| a.bitrate);
        let line = abr::video_budget(self.line_estimate(), kbps, self.audio.done);
        let held = self.hold.load(Ordering::Relaxed);
        let (slow, starved) = self.video.as_ref().filter(|cur| cur.frames_shown() > 0).map_or((false, false), |cur| {
            let behind = cur.behind(self.clock.now()).unwrap_or(0.0);
            let fed = cur.buffered().is_none_or(|b| b > 3.0);
            let carried = !held && cur.buffered().is_some_and(|b| b < 1.0) && line.is_some_and(|l| l >= cur.kbps as f64 * 1000.0);
            (fed && behind > 2.0, carried && behind > 1.0)
        });
        if !(slow || starved) || self.paused || self.video_next.is_some() {
            self.lagging = None;
            return;
        }
        if self.lagging.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(2) {
            self.lagging = None;
            let Some(cur) = &self.video else { return };
            if slow {
                let below = self.current.as_ref().and_then(|r| r.video.iter().map(|v| v.height).filter(|&h| h < cur.stream_height).max());
                if let Some(h) = below.filter(|&h| self.height_cap.is_none_or(|(c, _)| h < c)) {
                    self.height_cap = Some((h, Instant::now()));
                }
            } else {
                let most = cur.kbps as f64 * 1000.0 * 0.9;
                if self.ceiling.is_none_or(|(c, _)| most < c) {
                    self.ceiling = Some((most, Instant::now()));
                }
            }
        }
    }

    /// Whether to replace the decoder by one for `want` (at output width `w`).
    /// Smaller streams are taken once the buffer runs low, bigger ones only after
    /// the line has had room for them for a while, so a passing dip or burst does
    /// not cause a switch.
    fn switch_wanted(&mut self, want: &crate::provider::VideoStream, size_height: u32, budget: Option<f64>, w: u32) -> bool {
        let now = self.clock.now();
        let Some(cur) = self.video.as_ref() else { return false };
        let latest = self.video_next.as_ref().unwrap_or(cur);
        let ended_early = self.video_next.is_none() && cur.finished() && !self.near_end();
        if ended_early {
            return true;
        }
        // Not while the sound needs the line, nor while the window is resizing.
        if self.hold.load(Ordering::Relaxed) || self.target_changed.elapsed() <= RESIZE_SETTLE {
            return false;
        }
        let since = self.switched.map_or(Duration::MAX, |t| t.elapsed());
        let behind = cur.behind(now).unwrap_or(0.0);
        // Unknown for HLS, which ffmpeg reads itself: good while it keeps up.
        let buffered = cur.buffered().unwrap_or(if behind < 0.5 { UP_AT } else { 0.0 });
        if latest.stream_url == want.url {
            // The same stream at another size, or a decoder starved so long it
            // would rather start over at the playback point.
            let size_off = (w as f64) > latest.width as f64 * 1.25 || (w as f64) < latest.width as f64 * 0.6;
            let starved = self.video_next.is_none()
                && !self.paused
                && behind > RESYNC_BEHIND
                && cur.buffered().is_some_and(|b| b < 1.0)
                && self.resynced.is_none_or(|t| t.elapsed() > RESYNC_EVERY);
            return size_off || starved;
        }
        if want.bitrate < latest.kbps {
            // Smaller window, or the user picked less: at once.
            if latest.stream_height > size_height {
                return true;
            }
            // The line cannot carry it: before the buffer runs out, early enough for
            // the smaller one to be ready by then.
            let early = DOWN_AT.max(self.lead(want, budget, false, false) + 4.0);
            return since > Duration::from_secs(2) && (buffered < early || behind > 1.0);
        }
        // A replacement on its way is not overtaken by a bigger one.
        if self.video_next.is_some() {
            return false;
        }
        let ample = budget.is_some_and(|b| b >= want.bitrate as f64 * 1000.0 * 1.5);
        // Another codec at the same height looks no better: only with room to spare.
        if want.height == latest.stream_height && !ample {
            return false;
        }
        let stable = self.want.as_ref().is_some_and(|(_, t)| t.elapsed() >= UP_STABLE);
        let wait = if ample && self.up_failures == 0 { Duration::from_secs(2) } else { UP_WAIT * (1 << self.up_failures) };
        since > wait && (ample || stable) && buffered >= UP_AT
    }

    // --- state for the UI -----------------------------------------------------------

    fn publish(&mut self) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.queue.len() != self.queue.len() || s.queue.iter().zip(&self.queue).any(|(a, b)| a != b) {
                s.queue = Arc::new(self.queue.clone());
            }
            s.index = self.index;
            s.status = self.status();
            s.duration = self.duration.filter(|_| !self.live());
            s.live = self.live();
            s.volume = self.volume;
            s.muted = self.muted;
            s.mode = self.mode;
            s.quality = self.quality;
            s.repeat = self.repeat;
            s.shuffle = self.shuffle;
            s.heights = self.current.as_ref().map(|r| r.video.iter().map(|v| v.height).collect()).unwrap_or_default();
            s.heights.dedup();
            s.active_height = self.video.as_ref().map(|v| v.stream_height);
            s.audio_label = self.current.as_ref().and_then(|r| r.audio.get(self.audio_index)).map(|a| a.label.clone()).unwrap_or_default();
            s.notice = self.notice.clone();
        }
        (self.waker)();
    }

    /// Updates the connection figures without asking the UI to redraw.
    fn publish_stats(&mut self) {
        self.stats_at = Instant::now();
        let now = self.clock.now();
        let net = NetStats {
            estimate: self.meter.estimate(),
            audio_kbps: self.current.as_ref().and_then(|r| r.audio.get(self.audio_index)).map_or(0, |a| a.bitrate),
            audio_ahead: (!self.audio.done).then_some(self.audio.ahead),
            video_ahead: self.video.as_ref().and_then(|v| v.buffered()).filter(|b| b.is_finite()),
            video_behind: self.video.as_ref().and_then(|v| v.behind(now)),
            video_held: self.hold.load(Ordering::Relaxed),
        };
        self.shared.lock().unwrap().net = net;
    }
}

/// How long until the site serves the streams (capped, in case of a bogus clock).
fn ready_in(resolved: &Resolved) -> Duration {
    resolved
        .ready_at
        .and_then(|at| at.duration_since(std::time::SystemTime::now()).ok())
        .map(|d| d.min(Duration::from_secs(90)))
        .unwrap_or_default()
}
