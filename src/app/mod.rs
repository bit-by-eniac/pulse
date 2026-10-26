//! The egui user interface.
//!
//! Layout adapts to the window size in tiers: a full window has a top bar,
//! content and a transport bar; smaller windows drop the top bar, then the
//! content, until only a one-row strip of playback controls is left.

mod browse;
mod controls;
mod icons;
mod playing;
mod settings;
mod signin;
mod theme;
mod thumbs;
mod widgets;

use crate::config::{Config, Mode};
use crate::engine::{self, Cmd, EngineHandle, PlaybackState, Status, Tools};
use crate::platform;
use crate::provider::{Container, Health, Input, Listing, Provider, ProviderError, SearchItem, SearchKind, Track};
use eframe::egui::{self, vec2, Color32, Id, Key, Rect, TextureHandle, TextureOptions, Ui, ViewportCommand};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use theme::*;
use thumbs::Thumbs;

pub const MIN_SIZE: [f32; 2] = [220.0, 64.0];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Browse,
    Playing,
}

enum Loadable<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(ProviderError),
}

struct ListingState {
    url: String,
    autoplay: Option<Option<String>>,
    title: String,
    owner: Option<String>,
    data: Loadable<Listing>,
}

struct HealthInfo {
    health: Health,
    tools: Tools,
}

/// A one-click action on a track, confirmed with a tick on its button for a moment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Done {
    Queued,
    Copied,
}

enum UiMsg {
    Search { gen: u64, result: Result<Vec<SearchItem>, ProviderError> },
    Listing { gen: u64, result: Result<Listing, ProviderError>, autoplay: Option<Option<String>> },
    Health(HealthInfo),
    Updated(Result<String, ProviderError>),
}

pub struct PulseApp {
    ctx: egui::Context,
    provider: Arc<dyn Provider>,
    engine: EngineHandle,
    config: Config,
    saved_config: String,
    last_save: Instant,
    thumbs: Thumbs,
    fonts: FallbackFonts,
    inbox: Receiver<UiMsg>,
    tx: Sender<UiMsg>,

    tab: Tab,
    query: String,
    search_id: Id,
    focus_search: bool,
    kind: SearchKind,
    results: Loadable<Vec<SearchItem>>,
    listing: Option<ListingState>,
    request_gen: u64,
    show_queue: bool,
    queue_overlay: bool,
    queue_scrolled: Option<usize>,

    settings_open: bool,
    health: Option<HealthInfo>,
    updating: bool,
    update_message: Option<String>,
    args_text: String,
    sign_in: Option<signin::SignInDialog>,

    video_tex: Option<TextureHandle>,
    /// The frame last handed to `video_tex`, to reuse its buffer.
    video_pixels: Option<Arc<egui::ColorImage>>,
    video_track: Option<String>,
    /// Where the picture was drawn this frame, in physical pixels.
    frame_target: Option<(u32, u32)>,
    sent_target: Option<(u32, u32)>,
    last_title: String,
    seek_drag: Option<f32>,

    fullscreen: bool,
    last_activity: Instant,
    notice_seen: Option<(u64, Instant)>,
    window_size: Option<[f32; 2]>,
    done: Option<(Done, String, Instant)>,
}

impl PulseApp {
    pub fn new(cc: &eframe::CreationContext<'_>, provider: Arc<dyn Provider>, config: Config, initial: String) -> Self {
        let ctx = cc.egui_ctx.clone();
        theme::apply(&ctx);
        let waker: engine::Waker = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let engine = engine::start(provider.clone(), &config, waker);
        let (tx, inbox) = channel();
        let mut app = Self {
            thumbs: Thumbs::new(&ctx),
            fonts: FallbackFonts::new(&ctx),
            saved_config: serde_json::to_string(&config).unwrap_or_default(),
            last_save: Instant::now(),
            args_text: config.ytdlp_args.join(" "),
            ctx,
            provider,
            engine,
            config,
            inbox,
            tx,
            tab: Tab::Browse,
            query: String::new(),
            search_id: Id::unique("search-box"),
            focus_search: true,
            kind: SearchKind::Video,
            results: Loadable::Idle,
            listing: None,
            request_gen: 0,
            show_queue: true,
            queue_overlay: false,
            queue_scrolled: None,
            settings_open: false,
            sign_in: None,
            health: None,
            updating: false,
            update_message: None,
            video_tex: None,
            video_pixels: None,
            video_track: None,
            frame_target: None,
            sent_target: None,
            last_title: String::new(),
            seek_drag: None,
            fullscreen: false,
            last_activity: Instant::now(),
            notice_seen: None,
            window_size: None,
            done: None,
        };
        app.refresh_health();
        if !initial.trim().is_empty() {
            app.query = initial;
            app.focus_search = false;
            app.submit();
        }
        app
    }

    // --- background work --------------------------------------------------------

    fn spawn<F: FnOnce(Arc<dyn Provider>) -> UiMsg + Send + 'static>(&self, work: F) {
        let (provider, tx, ctx) = (self.provider.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work(provider));
            ctx.request_repaint();
        });
    }

    fn refresh_health(&self) {
        self.spawn(|p| UiMsg::Health(HealthInfo { health: p.health(), tools: Tools::discover() }));
    }

    fn start_search(&mut self) {
        let query = self.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.request_gen += 1;
        let (gen, kind) = (self.request_gen, self.kind);
        self.results = Loadable::Loading;
        self.listing = None;
        self.tab = Tab::Browse;
        self.spawn(move |p| UiMsg::Search { gen, result: p.search(&query, kind, 30) });
    }

    fn open_listing(&mut self, url: String, title: String, owner: Option<String>, autoplay: Option<Option<String>>) {
        self.request_gen += 1;
        let gen = self.request_gen;
        self.listing = Some(ListingState { url: url.clone(), autoplay: autoplay.clone(), title, owner, data: Loadable::Loading });
        self.tab = Tab::Browse;
        self.spawn(move |p| UiMsg::Listing { gen, result: p.open(&url, 300), autoplay });
    }

    fn reopen_listing(&mut self) {
        if let Some(l) = &self.listing {
            let (url, title, owner, autoplay) = (l.url.clone(), l.title.clone(), l.owner.clone(), l.autoplay.clone());
            self.open_listing(url, title, owner, autoplay);
        }
    }

    fn open_container(&mut self, c: &Container) {
        self.open_listing(c.url.clone(), c.title.clone(), c.owner.clone(), None);
    }

    /// Search box submit: a search, or a pasted link.
    fn submit(&mut self) {
        let text = self.query.trim().to_string();
        match self.provider.parse_input(&text) {
            Input::Search(_) => self.start_search(),
            Input::Track(track) => {
                self.play_tracks(vec![track], 0);
                self.tab = Tab::Playing;
            }
            Input::Playlist { url, focus } => self.open_listing(url, "Playlist".into(), None, Some(focus)),
            Input::Channel { url } => self.open_listing(url, "Channel".into(), None, None),
        }
    }

    fn play_tracks(&mut self, tracks: Vec<Track>, index: usize) {
        self.engine.send(Cmd::PlayQueue { tracks, index });
        if self.config.mode == Mode::Video && self.tab == Tab::Browse {
            self.tab = Tab::Playing;
        }
    }

    fn enqueue(&mut self, track: Track) {
        self.done = Some((Done::Queued, track.id.clone(), Instant::now()));
        self.engine.send(Cmd::Enqueue(vec![track]));
    }

    /// Share: puts the track's link on the clipboard.
    fn copy_link(&mut self, track: &Track) {
        self.ctx.copy_text(track.url.clone());
        self.done = Some((Done::Copied, track.id.clone(), Instant::now()));
    }

    /// Whether `what` just happened to this track (its button shows a tick).
    fn just_done(&self, what: Done, track: &Track) -> bool {
        const SHOWN: Duration = Duration::from_millis(1600);
        match &self.done {
            Some((w, id, at)) if *w == what && *id == track.id && at.elapsed() < SHOWN => {
                self.ctx.request_repaint_after(SHOWN - at.elapsed());
                true
            }
            _ => false,
        }
    }

    fn start_update(&mut self) {
        if self.updating {
            return;
        }
        self.updating = true;
        self.update_message = None;
        self.spawn(|p| UiMsg::Updated(p.update()));
    }

    fn poll_inbox(&mut self) {
        while let Ok(msg) = self.inbox.try_recv() {
            match msg {
                UiMsg::Search { gen, result } if gen == self.request_gen => {
                    self.results = match result {
                        Ok(items) => Loadable::Ready(items),
                        Err(e) => Loadable::Failed(e),
                    };
                }
                UiMsg::Listing { gen, result, autoplay } if gen == self.request_gen => {
                    let Some(state) = self.listing.as_mut() else { continue };
                    match result {
                        Ok(listing) => {
                            if !listing.title.is_empty() {
                                state.title = listing.title.clone();
                            }
                            if listing.owner.is_some() {
                                state.owner = listing.owner.clone();
                            }
                            if let Some(focus) = autoplay {
                                let index = focus
                                    .and_then(|id| listing.tracks.iter().position(|t| t.id == id))
                                    .unwrap_or(0);
                                if !listing.tracks.is_empty() {
                                    self.engine.send(Cmd::PlayQueue { tracks: listing.tracks.clone(), index });
                                    self.tab = Tab::Playing;
                                }
                            }
                            state.data = Loadable::Ready(listing);
                        }
                        Err(e) => state.data = Loadable::Failed(e),
                    }
                }
                UiMsg::Health(info) => self.health = Some(info),
                UiMsg::Updated(result) => {
                    self.updating = false;
                    self.update_message = Some(match result {
                        Ok(msg) => msg,
                        Err(e) => format!("Update failed: {e}"),
                    });
                    self.refresh_health();
                }
                _ => {} // a stale answer to an older request
            }
        }
    }

    // --- per-frame housekeeping -----------------------------------------------------

    fn upload_video_frame(&mut self, ctx: &egui::Context, state: &PlaybackState) {
        let current = state.current().map(|t| t.id.clone());
        if self.video_track != current || state.mode != Mode::Video {
            self.video_tex = None;
            self.video_track = current;
        }
        // The last frame's pixels go back to the decoder once egui has uploaded them,
        // so playback reuses a few buffers instead of allocating one per frame.
        if let Some(Ok(image)) = self.video_pixels.take().map(Arc::try_unwrap) {
            self.engine.frames().recycle(bytemuck::allocation::cast_vec(image.pixels));
        }
        let Some(frame) = self.engine.frames().take() else { return };
        if state.mode != Mode::Video {
            self.engine.frames().recycle(frame.pixels);
            return;
        }
        let size = [frame.width, frame.height];
        let image = Arc::new(egui::ColorImage::new(size, bytemuck::allocation::cast_vec(frame.pixels)));
        match self.video_tex.as_mut() {
            // Same size: update the texture in place instead of making a new one.
            Some(tex) if tex.size() == size => tex.set_partial([0, 0], image.clone(), TextureOptions::LINEAR),
            Some(tex) => tex.set(image.clone(), TextureOptions::LINEAR),
            None => self.video_tex = Some(ctx.load_texture("video", image.clone(), TextureOptions::LINEAR)),
        }
        self.video_pixels = Some(image);
    }

    fn sync_window(&mut self, ctx: &egui::Context, state: &PlaybackState) {
        let title = match state.current() {
            Some(t) if state.status != Status::Idle => format!("{} \u{2014} {}", t.title, platform::APP_NAME),
            _ => platform::APP_NAME.to_string(),
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        let info = ctx.input(|i| i.viewport().clone());
        self.fullscreen = info.fullscreen.unwrap_or(false);
        if !self.fullscreen && info.maximized != Some(true) {
            if let Some(r) = info.inner_rect {
                self.window_size = Some([r.width(), r.height()]);
            }
        }
        // Load fonts for scripts that show up in titles.
        if let Some(t) = state.current() {
            self.fonts.ensure_for(ctx, &t.title);
        }
        if let Loadable::Ready(items) = &self.results {
            for item in items.iter().take(12) {
                match item {
                    SearchItem::Track(t) => self.fonts.ensure_for(ctx, &t.title),
                    SearchItem::Container(c) => self.fonts.ensure_for(ctx, &c.title),
                }
            }
        }
        if let Some(Loadable::Ready(l)) = self.listing.as_ref().map(|l| &l.data) {
            self.fonts.ensure_for(ctx, &l.title);
            for t in l.tracks.iter().take(12) {
                self.fonts.ensure_for(ctx, &t.title);
            }
        }
    }

    fn persist(&mut self, state: &PlaybackState, force: bool) {
        self.config.volume = state.volume;
        self.config.muted = state.muted;
        self.config.mode = state.mode;
        self.config.quality = state.quality;
        self.config.repeat = state.repeat;
        self.config.shuffle = state.shuffle;
        self.config.window_size = self.window_size;
        if !force && self.last_save.elapsed() < Duration::from_secs(5) {
            return;
        }
        let json = serde_json::to_string(&self.config).unwrap_or_default();
        if json != self.saved_config {
            self.config.save();
            self.saved_config = json;
        }
        self.last_save = Instant::now();
    }

    fn handle_keys(&mut self, ctx: &egui::Context, state: &PlaybackState) {
        let command = ctx.input(|i| i.modifiers.command);
        if ctx.input(|i| (i.key_pressed(Key::K) || i.key_pressed(Key::L)) && i.modifiers.command) {
            self.focus_search = true;
            self.tab = Tab::Browse;
            if self.fullscreen {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.settings_open {
                self.settings_open = false;
            } else if self.fullscreen {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            }
        }
        if ctx.egui_wants_keyboard_input() || command {
            return;
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let shift = ctx.input(|i| i.modifiers.shift);
        let e = &self.engine;
        if pressed(Key::Space) {
            e.send(Cmd::TogglePause);
        }
        if pressed(Key::ArrowLeft) {
            e.send(Cmd::SeekBy(if shift { -30.0 } else { -5.0 }));
        }
        if pressed(Key::ArrowRight) {
            e.send(Cmd::SeekBy(if shift { 30.0 } else { 5.0 }));
        }
        if pressed(Key::J) {
            e.send(Cmd::SeekBy(-10.0));
        }
        if pressed(Key::L) {
            e.send(Cmd::SeekBy(10.0));
        }
        if pressed(Key::ArrowUp) {
            e.send(Cmd::SetVolume(state.volume + 0.05));
        }
        if pressed(Key::ArrowDown) {
            e.send(Cmd::SetVolume(state.volume - 0.05));
        }
        if pressed(Key::N) {
            e.send(Cmd::Next);
        }
        if pressed(Key::P) {
            e.send(Cmd::Prev);
        }
        if pressed(Key::M) {
            e.send(Cmd::SetMuted(!state.muted));
        }
        if pressed(Key::V) {
            e.send(Cmd::SetMode(if state.mode == Mode::Video { Mode::Audio } else { Mode::Video }));
        }
        if pressed(Key::F) {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!self.fullscreen));
        }
        if pressed(Key::Q) {
            self.toggle_queue(ctx.content_rect().width());
        }
        if pressed(Key::Slash) {
            self.focus_search = true;
            self.tab = Tab::Browse;
        }
    }

    fn report_video_target(&mut self, ctx: &egui::Context) {
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let target = if minimized { None } else { self.frame_target };
        if target != self.sent_target {
            let jitter = |a: u32, b: u32| a.abs_diff(b) <= 6;
            let changed = match (target, self.sent_target) {
                (Some(a), Some(b)) => !(jitter(a.0, b.0) && jitter(a.1, b.1)),
                _ => true,
            };
            if changed {
                self.sent_target = target;
                self.engine.send(Cmd::VideoTarget(target));
            }
        }
        self.frame_target = None;
    }
}

/// How much room each part of the window gets.
struct Layout {
    top: Option<Rect>,
    content: Option<Rect>,
    controls: Rect,
    style: controls::Style,
}

fn layout(full: Rect) -> Layout {
    let (w, h) = (full.width(), full.height());
    let full_h = controls::full_height(w);
    let (style, controls_h) = if h - full_h >= 110.0 {
        (controls::Style::from_width(w), full_h)
    } else {
        (controls::Style::Strip, controls::STRIP_HEIGHT)
    };
    let controls = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - controls_h), full.max);
    let mut y = full.top();
    let top = (h >= 330.0 && w >= 380.0).then(|| {
        let r = Rect::from_min_size(full.min, vec2(w, 52.0));
        y += 52.0;
        r
    });
    let content_h = controls.top() - y;
    let content = (content_h >= 56.0).then(|| Rect::from_min_max(egui::pos2(full.left(), y), egui::pos2(full.right(), controls.top())));
    Layout { top, content, controls, style }
}

impl eframe::App for PulseApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // A minimised or covered window gets no `ui` calls at all: report the
        // picture as hidden from here, so the engine stops decoding it.
        if ctx.input(|i| i.viewport().visible() == Some(false)) {
            self.report_video_target(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_inbox();
        self.thumbs.poll();
        let state = self.engine.state();
        self.sync_window(&ctx, &state);
        self.upload_video_frame(&ctx, &state);
        self.handle_keys(&ctx, &state);

        if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down() || !i.events.is_empty()) {
            self.last_activity = Instant::now();
        }

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, BG);

        if self.fullscreen {
            self.fullscreen_ui(ui, full, &state);
            self.notice_toast(ui, full, &state);
        } else {
            let lay = layout(full);
            // Tiny windows show the player only; there is no room to browse.
            let tab = if lay.top.is_some() { self.tab } else { Tab::Playing };
            if let Some(content) = lay.content {
                match tab {
                    Tab::Browse => self.browse_view(ui, content, &state),
                    Tab::Playing => self.playing_view(ui, content, &state, lay.style),
                }
                self.notice_toast(ui, content, &state);
            }
            if let Some(top) = lay.top {
                self.top_bar(ui, top, &state);
            }
            self.controls(ui, lay.controls, &state, lay.style);
        }
        self.settings_window(&ctx, &state);
        self.sign_in_window(&ctx);

        self.report_video_target(&ctx);
        self.persist(&state, false);

        // Animate the clock while something is playing; otherwise stay idle.
        if state.status == Status::Playing && !ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
            ctx.request_repaint_after(Duration::from_millis(if self.frame_target.is_some() { 33 } else { 200 }));
        }
    }


    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let state = self.engine.state();
        self.persist(&state, true);
        self.engine.shutdown();
    }
}

impl PulseApp {
    fn fullscreen_ui(&mut self, ui: &mut egui::Ui, full: Rect, state: &PlaybackState) {
        let active = self.last_activity.elapsed() < Duration::from_millis(2500) || state.status == Status::Paused;
        self.picture(ui, full, state, true);
        let h = controls::full_height(full.width());
        let bar = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - h), full.max);
        if active {
            // Fade the picture under the controls so they stay readable.
            ui.painter().rect_filled(bar.expand2(vec2(0.0, 24.0)), 0.0, Color32::from_black_alpha(205));
            self.controls(ui, bar, state, controls::Style::from_width(full.width()));
        } else {
            self.ctx.set_cursor_icon(egui::CursorIcon::None);
            self.ctx.request_repaint_after(Duration::from_millis(500));
        }
        if active {
            self.ctx.request_repaint_after(Duration::from_millis(300));
        }
    }
}

/// Opens the window and runs until it is closed.
pub fn run(provider: Arc<dyn Provider>, config: Config, initial: String) -> eframe::Result {
    let size = config.window_size.unwrap_or([1100.0, 720.0]);
    // Without an icon eframe puts egui's logo in the Dock; an empty one leaves the bundle's icon there.
    let icon = platform::window_icon_png().and_then(|png| eframe::icon_data::from_png_bytes(png).ok()).unwrap_or_default();
    let viewport = egui::ViewportBuilder::default()
        .with_title(platform::APP_NAME)
        .with_app_id("pulse")
        .with_inner_size(size)
        .with_min_inner_size(MIN_SIZE)
        .with_icon(Arc::new(icon));
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        platform::APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(PulseApp::new(cc, provider, config, initial)))),
    )
}
