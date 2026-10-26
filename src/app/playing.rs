//! "Now playing": the picture (video frames or cover art), track info and the queue.

use super::controls::{self, Style};
use super::icons::Icon;
use super::widgets;
use super::*;
use eframe::egui::{pos2, Align, Align2, ScrollArea, Sense, Stroke, StrokeKind, UiBuilder};

const QUEUE_WIDTH: f32 = 340.0;

impl PulseApp {
    pub(super) fn playing_view(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState, _style: Style) {
        let wide = rect.width() >= 900.0 && rect.height() >= 300.0;
        let mut main = rect;
        if wide && self.show_queue {
            let queue = Rect::from_min_max(pos2(rect.right() - QUEUE_WIDTH, rect.top()), rect.max);
            main.max.x = queue.left();
            ui.painter().vline(queue.left(), queue.y_range(), Stroke::new(1.0, BORDER));
            self.queue_panel(ui, queue, state);
        } else if !wide && self.queue_overlay {
            self.queue_panel(ui, rect, state);
            return;
        }

        let roomy = main.width() > 480.0 && main.height() > 260.0;
        let area = if roomy { main.shrink(20.0) } else { main };
        let info_h = if main.height() >= 340.0 && main.width() >= 420.0 { 78.0 } else { 0.0 };
        let avail = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - info_h));
        let pic = if roomy || info_h > 0.0 {
            // 16:9, as large as fits, centred horizontally
            let h = (avail.width() * 9.0 / 16.0).min(avail.height());
            let w = h * 16.0 / 9.0;
            Rect::from_min_size(pos2(avail.center().x - w / 2.0, avail.top()), vec2(w, h))
        } else {
            avail
        };
        self.picture(ui, pic, state, false);
        if info_h > 0.0 {
            let info = Rect::from_min_size(pos2(pic.left(), pic.bottom() + 14.0), vec2(pic.width(), info_h - 14.0));
            self.track_info(ui, info, state);
        }
    }

    fn track_info(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let Some(track) = state.current() else { return };
        // Share sits right of the title, in room kept for its wider "Copied" state.
        let share_w = widgets::button_width(ui, "Copied", true);
        let text_w = rect.width() - share_w - 16.0;
        let copied = self.just_done(Done::Copied, track);
        let share = Rect::from_min_size(pos2(rect.right() - share_w, rect.top() - 6.0), vec2(share_w, 30.0));
        let mut clicked = false;
        ui.scope_builder(UiBuilder::new().max_rect(share).layout(eframe::egui::Layout::right_to_left(Align::Center)), |ui| {
            let (label, icon) = if copied { ("Copied", Icon::Check) } else { ("Share", Icon::Share) };
            clicked = widgets::button(ui, label, Some(icon), false).on_hover_text("Copy the link to this video").clicked();
        });
        if clicked {
            self.copy_link(track);
        }
        let p = ui.painter();
        let h = widgets::text_clipped(p, rect.min, &track.title, theme::title(), TEXT, text_w, 2);
        let mut meta = Vec::new();
        if let Some(c) = &track.channel {
            meta.push(c.clone());
        }
        if let Some(v) = track.views {
            meta.push(format!("{} views", widgets::fmt_count(v)));
        }
        if state.mode == Mode::Audio && !state.audio_label.is_empty() {
            meta.push(state.audio_label.clone());
        }
        widgets::text_clipped(p, rect.min + vec2(0.0, h + 4.0), &meta.join("  \u{b7}  "), theme::small(), TEXT2, text_w, 1);
    }

    /// The video (or cover art). Also reports the size the decoder should produce.
    pub(super) fn picture(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState, fullscreen: bool) {
        let radius = if fullscreen || rect.width() < 480.0 { 0 } else { 6 };
        let id = Id::unique("picture");
        let resp = ui.interact(rect, id, Sense::click());
        if resp.double_clicked() {
            self.ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!self.fullscreen));
        } else if resp.clicked() {
            self.engine.send(Cmd::TogglePause);
        }
        let ppp = self.ctx.pixels_per_point();
        let track = state.current();
        let p = ui.painter().clone();
        p.rect_filled(rect, radius, Color32::BLACK);

        let Some(track) = track else {
            icons::paint(&p, Icon::Play, rect.center() - vec2(0.0, 10.0), 28.0, TEXT3);
            p.text(rect.center() + vec2(0.0, 22.0), Align2::CENTER_CENTER, "Nothing playing", theme::body(), TEXT3);
            p.rect_stroke(rect, radius, Stroke::new(1.0, BORDER), StrokeKind::Inside);
            return;
        };

        let loading = matches!(state.status, Status::Loading);
        let mut showing_video = false;
        if state.mode == Mode::Video {
            self.frame_target = Some(((rect.width() * ppp) as u32, (rect.height() * ppp) as u32));
            if let Some(tex) = &self.video_tex {
                let size = tex.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let shown = Rect::from_center_size(rect.center(), size * scale);
                p.image(tex.id(), shown, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                showing_video = true;
            }
        }
        if !showing_video {
            let thumb = track.thumbnail.clone().and_then(|u| self.thumbs.get(&u));
            widgets::picture(&p, rect, thumb, radius);
            p.rect_filled(rect, radius, Color32::from_black_alpha(if state.mode == Mode::Audio { 120 } else { 150 }));
            let center = rect.center();
            let big = (rect.height() * 0.22).clamp(18.0, 46.0);
            if state.mode == Mode::Audio {
                icons::paint(&p, Icon::Headphones, center, big, Color32::from_white_alpha(200));
                if rect.height() > 120.0 {
                    p.text(center + vec2(0.0, big * 0.9), Align2::CENTER_TOP, "AUDIO ONLY", theme::mono(), Color32::from_white_alpha(150));
                }
            } else if loading || matches!(state.status, Status::Playing | Status::Paused | Status::Buffering) {
                // Waiting for the first frame.
                spinner(ui, &p, center, big * 0.5);
                if rect.height() > 120.0 {
                    p.text(center + vec2(0.0, big * 0.8), Align2::CENTER_TOP, "LOADING VIDEO", theme::mono(), Color32::from_white_alpha(150));
                }
            }
        } else if matches!(state.status, Status::Buffering | Status::Loading) {
            spinner(ui, &p, rect.center(), 14.0);
        }
        if radius > 0 {
            p.rect_stroke(rect, radius, Stroke::new(1.0, BORDER), StrokeKind::Inside);
        }
        if state.status == Status::Paused && showing_video {
            let r = 26.0;
            p.circle_filled(rect.center(), r, Color32::from_black_alpha(150));
            icons::paint(&p, Icon::Play, rect.center() + vec2(2.0, 0.0), r * 0.9, TEXT);
        }
    }

    fn queue_panel(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let inner = rect.shrink2(vec2(12.0, 0.0));
        let header = Rect::from_min_size(inner.min + vec2(0.0, 12.0), vec2(inner.width(), 28.0));
        let p = ui.painter().clone();
        let title_w = p.layout_no_wrap("Playing Queue".into(), theme::body(), TEXT).size().x;
        p.text(pos2(header.left() + 4.0, header.center().y), Align2::LEFT_CENTER, "Playing Queue", theme::body(), TEXT);
        p.text(pos2(header.left() + 4.0 + title_w + 8.0, header.center().y), Align2::LEFT_CENTER, state.queue.len(), theme::mono(), TEXT3);
        let engine = self.engine.clone();
        ui.scope_builder(UiBuilder::new().max_rect(header).layout(eframe::egui::Layout::right_to_left(Align::Center)), |ui| {
            if !state.queue.is_empty() && widgets::button(ui, "Clear", None, false).clicked() {
                engine.send(Cmd::ClearQueue);
            }
        });
        let list = Rect::from_min_max(pos2(inner.left(), header.bottom() + 8.0), pos2(inner.right(), rect.bottom() - 8.0));
        if state.queue.is_empty() {
            p.text(list.center_top() + vec2(0.0, 40.0), Align2::CENTER_CENTER, "Queue is empty", theme::body(), TEXT3);
            return;
        }
        let current = state.index;
        let mut play = None;
        let mut remove = None;
        let scroll_target = current.filter(|_| self.queue_scrolled != current);
        ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, track) in state.queue.iter().enumerate() {
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                    if scroll_target == Some(i) {
                        ui.scroll_to_rect(rect, Some(Align::Center));
                    }
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let out = self.track_row(ui, rect, track, Some(i + 1), current == Some(i), RowActions::Remove);
                    if out.clicked {
                        play = Some(i);
                    }
                    if out.remove {
                        remove = Some(i);
                    }
                }
            });
        });
        self.queue_scrolled = current;
        if let Some(i) = play {
            self.engine.send(Cmd::PlayIndex(i));
        }
        if let Some(i) = remove {
            self.engine.send(Cmd::RemoveFromQueue(i));
        }
    }

    /// Error / hint message that floats over the bottom of the content area.
    pub(super) fn notice_toast(&mut self, ui: &mut Ui, content: Rect, state: &PlaybackState) {
        let Some(notice) = &state.notice else {
            self.notice_seen = None;
            return;
        };
        let first_seen = match self.notice_seen {
            Some((id, at)) if id == notice.id => at,
            _ => {
                let now = Instant::now();
                self.notice_seen = Some((notice.id, now));
                now
            }
        };
        use crate::provider::ErrorKind;
        let action = match notice.kind {
            ErrorKind::Blocked => Some("Sign in\u{2026}"),
            ErrorKind::BackendOutdated => Some(if self.updating { "Updating\u{2026}" } else { "Update yt-dlp" }),
            ErrorKind::Missing => Some("Open settings"),
            _ => None,
        };
        // Notices the user can act on stay until they do (or close them).
        if action.is_none() && first_seen.elapsed() > Duration::from_secs(14) {
            self.engine.send(Cmd::DismissNotice);
            return;
        }
        self.ctx.request_repaint_after(Duration::from_secs(1));
        let width = (content.width() - 24.0).min(620.0);
        let galley = widgets::clipped(ui.painter(), &notice.message, theme::body(), TEXT, width - 60.0 - if action.is_some() { 120.0 } else { 0.0 }, 2);
        let height = (galley.size().y + 24.0).max(48.0);
        let rect = Rect::from_min_size(pos2(content.center().x - width / 2.0, content.bottom() - height - 12.0), vec2(width, height));
        let p = ui.painter();
        p.rect(rect, theme::corner(), ELEVATED, Stroke::new(1.0, BORDER), StrokeKind::Inside);
        p.galley(pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0), galley, ERROR);
        let mut close = false;
        let mut act = false;
        ui.scope_builder(
            UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 0.0))).layout(eframe::egui::Layout::right_to_left(Align::Center)),
            |ui| {
                close = widgets::icon_button(ui, Icon::Close, 12.0, false).clicked();
                if let Some(label) = action {
                    act = widgets::button(ui, label, None, true).clicked();
                }
            },
        );
        if act {
            match notice.kind {
                ErrorKind::Missing => self.settings_open = true,
                ErrorKind::Blocked => self.open_sign_in(state.index.map(super::signin::Retry::Track)),
                _ => self.start_update(),
            }
            close = true;
        }
        if close {
            self.engine.send(Cmd::DismissNotice);
        }
    }
}

pub(super) const ROW_H: f32 = 54.0;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum RowActions {
    None,
    Remove,
    Add,
}

pub(super) struct RowOut {
    pub clicked: bool,
    pub remove: bool,
    pub add: bool,
}

fn spinner(ui: &Ui, p: &eframe::egui::Painter, center: eframe::egui::Pos2, r: f32) {
    let t = ui.input(|i| i.time) as f32;
    let pts: Vec<_> = (0..=16)
        .map(|i| {
            let a = t * 5.0 + i as f32 * 0.13 * std::f32::consts::PI;
            center + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(eframe::egui::Shape::line(pts, Stroke::new(2.0, Color32::from_white_alpha(220))));
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

impl PulseApp {
    /// Compact list row: index, thumbnail, title + channel, duration.
    pub(super) fn track_row(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        track: &Track,
        index: Option<usize>,
        current: bool,
        actions: RowActions,
    ) -> RowOut {
        let id = Id::unique(("row", &track.id, index, rect.min.x as i32));
        let resp = ui.interact(rect, id, Sense::click());
        // Not `resp.hovered()`, which is false while the pointer is on the action button.
        let hover = ui.rect_contains_pointer(rect);
        {
            let p = ui.painter();
            if current {
                p.rect(rect, theme::corner(), ELEVATED, Stroke::new(1.0, BORDER), StrokeKind::Inside);
            } else if hover {
                p.rect_filled(rect, theme::corner(), SURFACE);
            }
        }
        let mut x = rect.left() + 8.0;
        if let Some(i) = index {
            let p = ui.painter();
            if current {
                icons::paint(p, Icon::Play, pos2(x + 9.0, rect.center().y), 11.0, TEXT);
            } else if hover {
                icons::paint(p, Icon::Play, pos2(x + 9.0, rect.center().y), 11.0, TEXT2);
            } else {
                p.text(pos2(x + 9.0, rect.center().y), Align2::CENTER_CENTER, i, theme::mono(), TEXT3);
            }
            x += 26.0;
        }
        let thumb = Rect::from_min_size(pos2(x, rect.center().y - 18.0), vec2(64.0, 36.0));
        let tex = track.thumbnail.clone().and_then(|u| self.thumbs.get(&u));
        widgets::picture(ui.painter(), thumb, tex, 3);
        x = thumb.right() + 12.0;

        let right_w = if track.duration.is_some() || track.live { 52.0 } else { 0.0 };
        let action_w = if actions != RowActions::None && hover { 28.0 } else { 0.0 };
        let text_w = (rect.right() - 8.0 - right_w - action_w - x).max(20.0);
        let p = ui.painter();
        let h = widgets::text_clipped(p, pos2(x, rect.top() + 9.0), &track.title, theme::body(), if current { TEXT } else { Color32::from_gray(0xd6) }, text_w, 1);
        if let Some(c) = &track.channel {
            widgets::text_clipped(p, pos2(x, rect.top() + 11.0 + h), c, theme::small(), TEXT2, text_w, 1);
        }
        if track.live {
            p.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, "LIVE", theme::mono(), TEXT);
        } else if let Some(d) = track.duration {
            p.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, widgets::fmt_time(d as f64), theme::mono(), TEXT3);
        }

        let mut out = RowOut { clicked: false, remove: false, add: false };
        if actions != RowActions::None && hover {
            let btn = Rect::from_center_size(pos2(rect.right() - 8.0 - right_w - 14.0, rect.center().y), vec2(28.0, 28.0));
            let bid = id.with("action");
            let br = ui.interact(btn, bid, Sense::click());
            let ticked = actions == RowActions::Add && self.just_done(Done::Queued, track);
            let (icon, tip) = match actions {
                RowActions::Remove => (Icon::Close, "Remove from queue"),
                _ if ticked => (Icon::Check, "Added to queue"),
                _ => (Icon::QueueAdd, "Add to queue"),
            };
            widgets::paint_icon_button(ui.painter(), &br, btn, icon, 13.0, false, if ticked { TEXT } else { TEXT2 });
            let br = br.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
            if br.clicked() {
                out.remove = actions == RowActions::Remove;
                out.add = actions == RowActions::Add;
            }
            out.clicked = resp.clicked() && !br.hovered();
        } else {
            out.clicked = resp.clicked();
        }
        let _ = controls::STRIP_HEIGHT;
        out
    }
}
