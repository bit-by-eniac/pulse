//! The transport bar. It comes in four shapes, picked from the window size:
//! `Wide` (three zones), `Medium` and `Narrow` (stacked rows) and `Strip`
//! (a single row of essentials for tiny windows).

use super::icons::Icon;
use super::widgets::{self, PlayVis};
use super::*;
use crate::config::Repeat;
use eframe::egui::{pos2, Align, Align2, Layout as EguiLayout, Popup, Sense, Stroke, UiBuilder};

pub const STRIP_HEIGHT: f32 = 48.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Strip,
    Narrow,
    Medium,
    Wide,
}

impl Style {
    pub fn from_width(w: f32) -> Style {
        if w >= 900.0 {
            Style::Wide
        } else if w >= 560.0 {
            Style::Medium
        } else {
            Style::Narrow
        }
    }
}

pub fn full_height(w: f32) -> f32 {
    match Style::from_width(w) {
        Style::Wide => 84.0,
        Style::Medium => 92.0,
        _ => 108.0,
    }
}

const PAD: f32 = 16.0;

/// What the right cluster shows besides fullscreen, queue, the volume icon and
/// the video/audio switch, which are always there.
#[derive(Clone, Copy)]
struct Cluster {
    labels: bool,
    quality: bool,
    slider: bool,
}

impl Cluster {
    /// Richest first. The quality button outlasts the volume slider: it is the only
    /// way to pick a quality, while volume also has the arrow keys and mute.
    const ALL: [Cluster; 4] = [
        Cluster { labels: true, quality: true, slider: true },
        Cluster { labels: false, quality: true, slider: true },
        Cluster { labels: false, quality: true, slider: false },
        Cluster { labels: false, quality: false, slider: false },
    ];
}

const MODES: [(Icon, &str); 2] = [(Icon::Video, "Video"), (Icon::Headphones, "Audio")];
const CLUSTER_GAP: f32 = 6.0;
const VOLUME_W: f32 = 84.0;

fn quality_width(ui: &Ui, label: &str) -> f32 {
    ui.painter().layout_no_wrap(label.to_string(), theme::mono(), TEXT).size().x + 38.0
}

fn cluster_width(ui: &Ui, c: Cluster) -> f32 {
    let icon = widgets::icon_button_side(16.0);
    let mut items = vec![icon, icon, icon, widgets::segmented_width(ui, &MODES, c.labels)];
    if c.slider {
        items.push(VOLUME_W);
    }
    if c.quality {
        // the longest label it can show, so the layout does not move when it changes
        items.push(quality_width(ui, "Auto 2160p"));
    }
    items.iter().sum::<f32>() + CLUSTER_GAP * (items.len() - 1) as f32
}

/// The richest cluster that fits in `room`, and the width to keep for it. The width
/// is the video mode one, so the transport stays put when the mode changes; audio
/// mode has no quality button and may show the volume slider in its place.
fn fit_cluster(ui: &Ui, room: f32, mode: Mode) -> (Cluster, f32) {
    let video = Cluster::ALL.into_iter().find(|&c| cluster_width(ui, c) <= room).unwrap_or(Cluster::ALL[3]);
    let width = cluster_width(ui, video);
    if mode == Mode::Video {
        return (video, width);
    }
    let audio = Cluster::ALL
        .into_iter()
        .map(|c| Cluster { quality: false, ..c })
        .find(|&c| cluster_width(ui, c) <= width)
        .unwrap_or(video);
    (audio, width)
}

fn row(ui: &mut Ui, rect: Rect, rtl: bool, add: impl FnOnce(&mut Ui)) {
    let layout = if rtl { EguiLayout::right_to_left(Align::Center) } else { EguiLayout::left_to_right(Align::Center) };
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(layout), |ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
        add(ui);
    });
}

impl PulseApp {
    pub(super) fn controls(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState, style: Style) {
        if !self.fullscreen {
            ui.painter().rect_filled(rect, 0.0, SURFACE);
            ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, BORDER));
        }
        let w = rect.width();
        let has_track = state.current().is_some();
        match style {
            Style::Strip => self.strip(ui, rect, state),
            Style::Narrow => {
                // Title row, seek row, button row.
                let title = Rect::from_min_size(rect.min + vec2(PAD, 5.0), vec2(w - 2.0 * PAD, 20.0));
                self.title_line(ui, title, state);
                let seek = Rect::from_min_size(rect.min + vec2(PAD, 28.0), vec2(w - 2.0 * PAD, 20.0));
                self.seek_row(ui, seek, state);
                let buttons = Rect::from_min_size(rect.min + vec2(0.0, 52.0), vec2(w, 52.0));
                self.button_row(ui, buttons, state, false);
            }
            Style::Medium => {
                let seek = Rect::from_min_size(rect.min + vec2(PAD, 6.0), vec2(w - 2.0 * PAD, 20.0));
                self.seek_row(ui, seek, state);
                let buttons = Rect::from_min_size(rect.min + vec2(0.0, 30.0), vec2(w, 56.0));
                self.button_row(ui, buttons, state, true);
            }
            Style::Wide => {
                // Room goes to the transport with a usable seek bar first, then to
                // the right cluster, then to the track info.
                const GAP: f32 = 24.0;
                const CENTER_MIN: f32 = 320.0;
                const LEFT_MIN: f32 = 190.0;
                let inner = w - 2.0 * PAD;
                let (cluster, right_w) = fit_cluster(ui, inner - LEFT_MIN - CENTER_MIN - 2.0 * GAP, state.mode);
                let left_w = (inner - right_w - CENTER_MIN - 2.0 * GAP).min(270.0);
                let left = Rect::from_min_size(rect.min + vec2(PAD, 14.0), vec2(left_w, 56.0));
                self.now_playing(ui, left, state);
                let right = Rect::from_min_size(pos2(rect.right() - PAD - right_w, rect.top() + 28.0), vec2(right_w, 28.0));
                self.right_cluster(ui, right, state, cluster);
                let cx0 = left.right() + GAP;
                let cx1 = right.left() - GAP;
                let cw = (cx1 - cx0).clamp(240.0, 640.0);
                let cx = cx0 + ((cx1 - cx0) - cw) / 2.0;
                let transport = Rect::from_min_size(pos2(cx, rect.top() + 8.0), vec2(cw, 40.0));
                row(ui, transport, false, |ui| {
                    let free = (cw - self.transport_width(true)).max(0.0) / 2.0;
                    ui.add_space(free);
                    self.transport(ui, state, true);
                });
                let seek = Rect::from_min_size(pos2(cx, rect.top() + 52.0), vec2(cw, 20.0));
                self.seek_row(ui, seek, state);
            }
        }
        let _ = has_track;
    }

    // --- pieces ---------------------------------------------------------------------

    fn transport_width(&self, full: bool) -> f32 {
        // shuffle, prev, play, next, repeat (or just prev, play, next) with 4px gaps
        if full {
            34.0 * 4.0 + 40.0 + 16.0
        } else {
            34.0 * 2.0 + 40.0 + 8.0
        }
    }

    fn transport(&mut self, ui: &mut Ui, state: &PlaybackState, full: bool) {
        let e = self.engine.clone();
        if full {
            let on = state.shuffle;
            if widgets::icon_button(ui, Icon::Shuffle, 16.0, on).on_hover_text("Shuffle").clicked() {
                e.send(Cmd::SetShuffle(!on));
            }
        }
        if widgets::icon_button(ui, Icon::Prev, 16.0, false).on_hover_text("Previous (P)").clicked() {
            e.send(Cmd::Prev);
        }
        let vis = match state.status {
            Status::Playing => PlayVis::Playing,
            Status::Loading | Status::Buffering => PlayVis::Loading,
            _ => PlayVis::Paused,
        };
        let tip = if vis == PlayVis::Playing { "Pause (Space)" } else { "Play (Space)" };
        if widgets::play_button(ui, vis, 36.0).on_hover_text(tip).clicked() {
            e.send(Cmd::TogglePause);
        }
        if widgets::icon_button(ui, Icon::Next, 16.0, false).on_hover_text("Next (N)").clicked() {
            e.send(Cmd::Next);
        }
        if full {
            let (icon, on) = match state.repeat {
                Repeat::Off => (Icon::Repeat, false),
                Repeat::All => (Icon::Repeat, true),
                Repeat::One => (Icon::RepeatOne, true),
            };
            if widgets::icon_button(ui, icon, 16.0, on).on_hover_text("Repeat").clicked() {
                e.send(Cmd::SetRepeat(match state.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                }));
            }
        }
    }

    /// `01:23 ──────── 03:45`
    fn seek_row(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let dur = state.duration.unwrap_or(0.0);
        let seekable = dur > 0.0 && !state.live && state.current().is_some();
        let pos = match self.seek_drag {
            Some(f) => f as f64 * dur,
            None => self.engine.position(),
        };
        let pos = if dur > 0.0 { pos.min(dur) } else { pos };
        let label_w = if dur >= 3600.0 { 56.0 } else { 44.0 };
        let engine = self.engine.clone();
        let mut drag = self.seek_drag;
        row(ui, rect, false, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (r, _) = ui.allocate_exact_size(vec2(label_w, rect.height()), Sense::hover());
            ui.painter().text(r.right_center(), Align2::RIGHT_CENTER, widgets::fmt_time(pos), theme::mono(), TEXT2);
            let bar_w = rect.width() - 2.0 * label_w - 16.0;
            let frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
            let out = widgets::bar(ui, bar_w, rect.height(), frac, seekable);
            if let Some(v) = out.value {
                drag = Some(v);
            }
            if out.released {
                if let Some(v) = drag.take() {
                    engine.send(Cmd::SeekTo(v as f64 * dur));
                }
            }
            let (r, _) = ui.allocate_exact_size(vec2(label_w, rect.height()), Sense::hover());
            if state.live {
                ui.painter().text(r.left_center(), Align2::LEFT_CENTER, "LIVE", theme::mono(), TEXT);
            } else if dur > 0.0 {
                ui.painter().text(r.left_center(), Align2::LEFT_CENTER, widgets::fmt_time(dur), theme::mono(), TEXT3);
            }
        });
        self.seek_drag = drag;
    }

    fn title_line(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let text = match state.current() {
            Some(t) => t.title.clone(),
            None => "Nothing playing".to_string(),
        };
        let galley = widgets::clipped(ui.painter(), &text, theme::body(), if state.current().is_some() { TEXT } else { TEXT3 }, rect.width(), 1);
        let pos = pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, TEXT);
    }

    /// Cover art with title and channel.
    fn now_playing(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let track = state.current();
        let art = Rect::from_min_size(rect.min + vec2(0.0, (rect.height() - 48.0) / 2.0), vec2(48.0, 48.0));
        let tex = track.and_then(|t| t.thumbnail.clone()).and_then(|u| self.thumbs.get(&u));
        widgets::picture(ui.painter(), art, tex, 4);
        let tx = art.right() + 12.0;
        let width = (rect.right() - tx).max(10.0);
        let p = ui.painter();
        match track {
            Some(t) => {
                let h = widgets::text_clipped(p, pos2(tx, art.top() + 5.0), &t.title, theme::body(), TEXT, width, 1);
                if let Some(c) = &t.channel {
                    widgets::text_clipped(p, pos2(tx, art.top() + 8.0 + h), c, theme::small(), TEXT2, width, 1);
                }
            }
            None => {
                widgets::text_clipped(p, pos2(tx, art.center().y - 8.0), "Nothing playing", theme::body(), TEXT3, width, 1);
            }
        }
    }

    /// Mode switch, quality, volume, queue, fullscreen. Right aligned in `rect`,
    /// which is `cluster_width` wide.
    fn right_cluster(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState, cluster: Cluster) {
        let engine = self.engine.clone();
        let ctx = self.ctx.clone();
        row(ui, rect, true, |ui| {
            ui.spacing_mut().item_spacing.x = CLUSTER_GAP;
            let fs = if self.fullscreen { Icon::ExitFullscreen } else { Icon::Fullscreen };
            if widgets::icon_button(ui, fs, 16.0, false).on_hover_text("Fullscreen (F)").clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!self.fullscreen));
            }
            if widgets::icon_button(ui, Icon::Queue, 16.0, self.queue_visible(ctx.content_rect().width())).on_hover_text("Queue (Q)").clicked() {
                self.toggle_queue(ctx.content_rect().width());
            }
            self.volume(ui, state, cluster.slider);
            if state.mode == Mode::Video && cluster.quality {
                self.quality_button(ui, state);
            }
            let sel = if state.mode == Mode::Video { 0 } else { 1 };
            // `row` is right-to-left, so the segmented control goes last (leftmost).
            if let Some(i) = widgets::segmented(ui, &MODES, sel, cluster.labels) {
                engine.send(Cmd::SetMode(if i == 0 { Mode::Video } else { Mode::Audio }));
            }
        });
    }

    fn volume(&mut self, ui: &mut Ui, state: &PlaybackState, slider: bool) {
        let muted = state.muted || state.volume <= 0.001;
        let icon = if muted {
            Icon::Mute
        } else if state.volume < 0.5 {
            Icon::VolumeLow
        } else {
            Icon::Volume
        };
        // right-to-left: slider first (rightmost), then the icon
        if slider {
            let out = widgets::bar(ui, VOLUME_W, 20.0, if state.muted { 0.0 } else { state.volume }, true);
            if let Some(v) = out.value {
                self.engine.send(Cmd::SetVolume(v));
            }
        }
        if widgets::icon_button(ui, icon, 16.0, false).on_hover_text("Mute (M)").clicked() {
            self.engine.send(Cmd::SetMuted(!state.muted));
        }
    }

    fn quality_button(&mut self, ui: &mut Ui, state: &PlaybackState) {
        let label = match (state.quality, state.active_height) {
            (Some(q), _) => format!("{q}p"),
            (None, Some(h)) => format!("Auto {h}p"),
            (None, None) => "Auto".to_string(),
        };
        let font = theme::mono();
        let (rect, resp) = ui.allocate_exact_size(vec2(quality_width(ui, &label), 28.0), Sense::click());
        let p = ui.painter();
        p.rect_stroke(rect, theme::corner(), Stroke::new(1.0, if resp.hovered() { TEXT3 } else { BORDER }), eframe::epaint::StrokeKind::Inside);
        p.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, font, if resp.hovered() { TEXT } else { TEXT2 });
        icons::paint(p, Icon::ChevronDown, pos2(rect.right() - 12.0, rect.center().y), 12.0, TEXT2);
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        let engine = self.engine.clone();
        Popup::menu(&resp).show(|ui| {
            ui.set_min_width(150.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut options: Vec<(Option<u32>, String)> = vec![(None, "Auto".to_string())];
            let mut heights = state.heights.clone();
            heights.sort_unstable_by(|a, b| b.cmp(a));
            for h in heights {
                let tag = match h {
                    2160 => " 4K",
                    1440 => " 2K",
                    1080 => " HD",
                    _ => "",
                };
                options.push((Some(h), format!("{h}p{tag}")));
            }
            for (value, text) in options {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(150.0), 28.0), Sense::click());
                let on = state.quality == value;
                if resp.hovered() {
                    ui.painter().rect_filled(r, theme::corner(), SURFACE);
                }
                ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, theme::body(), if on { TEXT } else { TEXT2 });
                if on {
                    icons::paint(ui.painter(), Icon::Check, pos2(r.right() - 14.0, r.center().y), 12.0, TEXT);
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    engine.send(Cmd::SetQuality(value));
                }
            }
        });
    }

    /// Medium and narrow layouts: shuffle/repeat on the left, transport in the
    /// middle, the right cluster on the right. Items drop out when space runs out.
    fn button_row(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState, medium: bool) {
        let w = rect.width();
        let c = self.transport_width(false);
        let mid = Rect::from_min_size(pos2(rect.center().x - c / 2.0, rect.top()), vec2(c, rect.height()));
        row(ui, mid, false, |ui| self.transport(ui, state, false));

        let side = ((w - c) / 2.0 - PAD - 8.0).max(0.0);
        if medium {
            // art + title on the left when there is room, the cluster right of the transport
            let (cluster, right_w) = fit_cluster(ui, rect.right() - PAD - (mid.right() + 12.0), state.mode);
            let cl = Rect::from_min_size(pos2(rect.right() - PAD - right_w, rect.top() + 14.0), vec2(right_w, 28.0));
            self.right_cluster(ui, cl, state, cluster);
            let left_w = (mid.left() - rect.left() - PAD - 12.0).max(0.0);
            if left_w >= 110.0 {
                let left = Rect::from_min_size(pos2(rect.left() + PAD, rect.top() + 4.0), vec2(left_w, 48.0));
                self.now_playing(ui, left, state);
            }
        } else {
            if side >= 64.0 {
                let left = Rect::from_min_size(pos2(rect.left() + PAD, rect.top() + 11.0), vec2(side, 30.0));
                row(ui, left, false, |ui| {
                    let on = state.shuffle;
                    if widgets::icon_button(ui, Icon::Shuffle, 15.0, on).clicked() {
                        self.engine.send(Cmd::SetShuffle(!on));
                    }
                });
            }
            let right = Rect::from_min_size(pos2(rect.right() - PAD - side, rect.top() + 11.0), vec2(side, 30.0));
            if side >= 34.0 {
                row(ui, right, true, |ui| {
                    // most important first (rightmost)
                    let room = (side / 34.0) as usize;
                    let mode_icon = if state.mode == Mode::Video { Icon::Video } else { Icon::Headphones };
                    if widgets::icon_button(ui, mode_icon, 16.0, false).on_hover_text("Switch video / audio (V)").clicked() {
                        self.engine.send(Cmd::SetMode(if state.mode == Mode::Video { Mode::Audio } else { Mode::Video }));
                    }
                    if room >= 2 {
                        let fs = if self.fullscreen { Icon::ExitFullscreen } else { Icon::Fullscreen };
                        if widgets::icon_button(ui, fs, 16.0, false).clicked() {
                            self.ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!self.fullscreen));
                        }
                    }
                    if room >= 3 {
                        let width = self.ctx.content_rect().width();
                        if widgets::icon_button(ui, Icon::Queue, 16.0, self.queue_visible(width)).clicked() {
                            self.toggle_queue(width);
                        }
                    }
                });
            }
        }
    }

    /// One row for tiny windows: transport, position, seek bar, a few extras.
    fn strip(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, BORDER));
        let w = rect.width();
        let inner = Rect::from_min_max(rect.min + vec2(6.0, 0.0), rect.max - vec2(8.0, 0.0));
        let show_time = w >= 300.0;
        let show_mode = w >= 400.0;
        let show_vol = w >= 520.0;
        let show_fs = w >= 460.0;
        let dur = state.duration.unwrap_or(0.0);
        let seekable = dur > 0.0 && !state.live && state.current().is_some();
        let pos = match self.seek_drag {
            Some(f) => f as f64 * dur,
            None => self.engine.position(),
        };
        let pos = if dur > 0.0 { pos.min(dur) } else { pos };
        let engine = self.engine.clone();
        let mut drag = self.seek_drag;
        row(ui, inner, false, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let fixed = 3.0 * 34.0
                + if show_time { 50.0 } else { 0.0 }
                + if show_mode { 34.0 } else { 0.0 }
                + if show_vol { 34.0 } else { 0.0 }
                + if show_fs { 34.0 } else { 0.0 }
                + 24.0;
            self.transport(ui, state, false);
            if show_time {
                let (r, _) = ui.allocate_exact_size(vec2(50.0, rect.height()), Sense::hover());
                ui.painter().text(r.center(), Align2::CENTER_CENTER, widgets::fmt_time(pos), theme::mono(), TEXT2);
            }
            let bar_w = (inner.width() - fixed).max(30.0);
            ui.add_space(4.0);
            let frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
            let out = widgets::bar(ui, bar_w, rect.height(), frac, seekable);
            if let Some(v) = out.value {
                drag = Some(v);
            }
            if out.released {
                if let Some(v) = drag.take() {
                    engine.send(Cmd::SeekTo(v as f64 * dur));
                }
            }
            ui.add_space(4.0);
            if show_mode {
                let icon = if state.mode == Mode::Video { Icon::Video } else { Icon::Headphones };
                if widgets::icon_button(ui, icon, 16.0, false).on_hover_text("Switch video / audio (V)").clicked() {
                    engine.send(Cmd::SetMode(if state.mode == Mode::Video { Mode::Audio } else { Mode::Video }));
                }
            }
            if show_vol {
                let muted = state.muted || state.volume <= 0.001;
                let icon = if muted { Icon::Mute } else { Icon::Volume };
                if widgets::icon_button(ui, icon, 16.0, false).clicked() {
                    engine.send(Cmd::SetMuted(!state.muted));
                }
            }
            if show_fs {
                let fs = if self.fullscreen { Icon::ExitFullscreen } else { Icon::Fullscreen };
                if widgets::icon_button(ui, fs, 16.0, false).clicked() {
                    self.ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!self.fullscreen));
                }
            }
        });
        self.seek_drag = drag;
    }

    // --- queue visibility ---------------------------------------------------------------

    pub(super) fn queue_visible(&self, width: f32) -> bool {
        if width >= 900.0 {
            self.show_queue
        } else {
            self.queue_overlay
        }
    }

    pub(super) fn toggle_queue(&mut self, width: f32) {
        if width >= 900.0 {
            self.show_queue = !self.show_queue;
        } else {
            self.queue_overlay = !self.queue_overlay;
            self.tab = Tab::Playing;
        }
    }
}
