//! Top bar, search results and playlist / channel listings.

use super::icons::Icon;
use super::playing::{RowActions, ROW_H};
use super::widgets;
use super::*;
use crate::provider::{ContainerKind, ErrorKind};
use eframe::egui::{pos2, Align, Align2, Frame, Layout as EguiLayout, Margin, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, UiBuilder};

impl PulseApp {
    pub(super) fn top_bar(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let w = rect.width();
        ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, BORDER));
        let mut left = rect.left() + 16.0;
        if w >= 440.0 {
            icons::logo(ui.painter(), pos2(left + 12.0, rect.center().y), 24.0, BG);
            left += 30.0;
            if w >= 700.0 {
                ui.painter().text(pos2(left + 6.0, rect.center().y), Align2::LEFT_CENTER, platform::APP_NAME, theme::title(), TEXT);
                left += 60.0;
            }
            left += 8.0;
        }

        // Right side: tabs and settings.
        let labels = w >= 700.0;
        let tabs_w = if labels { 190.0 } else { 74.0 };
        let right_w = tabs_w + 34.0 + 12.0;
        let right = Rect::from_min_size(pos2(rect.right() - 12.0 - right_w, rect.center().y - 14.0), vec2(right_w, 28.0));
        ui.scope_builder(UiBuilder::new().max_rect(right).layout(EguiLayout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if widgets::icon_button(ui, Icon::Gear, 16.0, self.settings_open).on_hover_text("Settings").clicked() {
                self.settings_open = !self.settings_open;
                if self.settings_open {
                    self.refresh_health();
                }
            }
            let items = [(Icon::Search, "Browse"), (Icon::Play, "Playing")];
            let sel = if self.tab == Tab::Browse { 0 } else { 1 };
            if let Some(i) = widgets::segmented(ui, &items, sel, labels) {
                self.tab = if i == 0 { Tab::Browse } else { Tab::Playing };
            }
        });

        self.search_box(ui, Rect::from_min_max(pos2(left, rect.center().y - 16.0), pos2(right.left() - 12.0, rect.center().y + 16.0)));
        let _ = state;
    }

    fn search_box(&mut self, ui: &mut Ui, rect: Rect) {
        let id = self.search_id;
        let focused = ui.memory(|m| m.has_focus(id));
        {
            let p = ui.painter();
            p.rect_filled(rect, theme::corner(), SURFACE);
            p.rect_stroke(rect, theme::corner(), Stroke::new(1.0, if focused { TEXT2 } else { BORDER }), StrokeKind::Inside);
            icons::paint(p, Icon::Search, pos2(rect.left() + 18.0, rect.center().y), 15.0, TEXT3);
        }
        let hint = if rect.width() > 300.0 { "Search YouTube or paste a link" } else { "Search or paste link" };
        let clear_w = if self.query.is_empty() { 0.0 } else { 30.0 };
        let hint_w = if self.query.is_empty() && !focused && rect.width() > 420.0 { 64.0 } else { 0.0 };
        let edit_rect = Rect::from_min_max(
            pos2(rect.left() + 34.0, rect.top()),
            pos2(rect.right() - 8.0 - clear_w - hint_w, rect.bottom()),
        );
        let edit = TextEdit::singleline(&mut self.query)
            .id(id)
            .hint_text(hint)
            .frame(Frame::NONE)
            .margin(Margin::symmetric(0, 0))
            .align(Align2::LEFT_CENTER)
            .desired_width(edit_rect.width())
            .font(theme::body());
        let resp = ui.put(edit_rect, edit);
        if self.focus_search {
            resp.request_focus();
            self.focus_search = false;
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.submit();
        }
        if hint_w > 0.0 {
            let badge = Rect::from_center_size(pos2(rect.right() - 12.0 - 22.0, rect.center().y), vec2(44.0, 20.0));
            let p = ui.painter();
            p.rect_stroke(badge, 3.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
            p.text(badge.center(), Align2::CENTER_CENTER, format!("{} K", platform::command_key()), theme::mono(), TEXT3);
        }
        if clear_w > 0.0 {
            let btn = Rect::from_center_size(pos2(rect.right() - 18.0, rect.center().y), vec2(26.0, 26.0));
            let r = ui.interact(btn, id.with("clear"), Sense::click());
            widgets::paint_icon_button(ui.painter(), &r, btn, Icon::Close, 11.0, false, TEXT3);
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.query.clear();
                self.focus_search = true;
            }
        }
    }

    pub(super) fn browse_view(&mut self, ui: &mut Ui, rect: Rect, state: &PlaybackState) {
        let pad = if rect.width() > 520.0 { 20.0 } else { 10.0 };
        let inner = Rect::from_min_max(rect.min + vec2(pad, 14.0), rect.max - vec2(pad, 0.0));
        let current = state.current().map(|t| t.id.clone());
        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            self.setup_banner(ui);
            if self.listing.is_some() {
                self.listing_view(ui, state, current.as_deref());
            } else {
                self.search_view(ui, current.as_deref());
            }
        });
        self.notice_toast(ui, rect, state);
    }

    fn search_view(&mut self, ui: &mut Ui, current: Option<&str>) {
        let before = self.kind;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (kind, label) in [(SearchKind::Video, "Videos"), (SearchKind::Playlist, "Playlists"), (SearchKind::Channel, "Channels")] {
                if widgets::chip(ui, label, self.kind == kind).clicked() {
                    self.kind = kind;
                }
            }
        });
        if self.kind != before && !self.query.trim().is_empty() {
            self.start_search();
        }
        ui.add_space(10.0);

        let message = |ui: &mut Ui, text: &str, color: Color32| {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(text).color(color));
            });
        };
        match &self.results {
            Loadable::Idle => {
                ui.add_space(70.0);
                let w = ui.available_width();
                let (r, _) = ui.allocate_exact_size(vec2(w, 90.0), Sense::hover());
                let p = ui.painter();
                icons::logo(p, r.center_top() + vec2(0.0, 14.0), 34.0, BG);
                p.text(r.center_top() + vec2(0.0, 52.0), Align2::CENTER_CENTER, "Search YouTube or paste a link", theme::title(), TEXT2);
                p.text(r.center_top() + vec2(0.0, 76.0), Align2::CENTER_CENTER, "videos, playlists and channels", theme::small(), TEXT3);
            }
            Loadable::Loading => {
                message(ui, "Searching\u{2026}", TEXT3);
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            }
            Loadable::Failed(e) => {
                let kind = e.kind;
                let text = e.message.clone();
                message(ui, &text, ERROR);
                self.error_action(ui, kind, super::signin::Retry::Search);
            }
            Loadable::Ready(items) if items.is_empty() => message(ui, "No results", TEXT3),
            Loadable::Ready(_) => self.results_grid(ui, current),
        }
    }

    fn results_grid(&mut self, ui: &mut Ui, current: Option<&str>) {
        let Loadable::Ready(items) = &self.results else { return };
        let items = items.clone();
        let tracks: Vec<Track> = items
            .iter()
            .filter_map(|i| if let SearchItem::Track(t) = i { Some(t.clone()) } else { None })
            .collect();
        let mut play: Option<usize> = None;
        let mut open: Option<Container> = None;
        let mut enqueue: Option<Track> = None;
        let mut share: Option<Track> = None;

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let avail = ui.available_width();
            let gap = 18.0;
            let cols = if avail < 560.0 { 1 } else { (((avail + gap) / (230.0 + gap)).floor() as usize).max(2) };
            let card_w = (avail - gap * (cols as f32 - 1.0)) / cols as f32;
            let card_h = if cols == 1 { 84.0 } else { card_w * 9.0 / 16.0 + 74.0 };
            let row_gap = if cols == 1 { 6.0 } else { 22.0 };
            let rows = items.len().div_ceil(cols);
            let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * (card_h + row_gap)), Sense::hover());
            for (n, item) in items.iter().enumerate() {
                let (r, c) = (n / cols, n % cols);
                let rect = Rect::from_min_size(
                    pos2(area.left() + c as f32 * (card_w + gap), area.top() + r as f32 * (card_h + row_gap)),
                    vec2(card_w, card_h),
                );
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let is_current = matches!(item, SearchItem::Track(t) if Some(t.id.as_str()) == current);
                match self.result_item(ui, rect, item, cols == 1, is_current, n) {
                    ItemAction::Play => {
                        if let SearchItem::Track(t) = item {
                            play = tracks.iter().position(|x| x.id == t.id);
                        }
                    }
                    ItemAction::Open => {
                        if let SearchItem::Container(c) = item {
                            open = Some(c.clone());
                        }
                    }
                    ItemAction::Enqueue => {
                        if let SearchItem::Track(t) = item {
                            enqueue = Some(t.clone());
                        }
                    }
                    ItemAction::Share => {
                        if let SearchItem::Track(t) = item {
                            share = Some(t.clone());
                        }
                    }
                    ItemAction::None => {}
                }
            }
        });
        if let Some(index) = play {
            self.play_tracks(tracks, index);
        }
        if let Some(c) = open {
            self.open_container(&c);
        }
        if let Some(t) = enqueue {
            self.enqueue(t);
        }
        if let Some(t) = share {
            self.copy_link(&t);
        }
    }

    /// One search result, as a card (grid) or a row (single column).
    fn result_item(&mut self, ui: &mut Ui, rect: Rect, item: &SearchItem, as_row: bool, current: bool, n: usize) -> ItemAction {
        let id = Id::unique(("result", n));
        let resp = ui.interact(rect, id, Sense::click());
        // Not `resp.hovered()`: that turns false while the pointer is on one of the
        // buttons drawn over the card, which would then vanish and come back every frame.
        let hover = ui.rect_contains_pointer(rect);
        let (title, sub, thumb_url, badge, is_track, avatar) = match item {
            SearchItem::Track(t) => {
                let mut sub = t.channel.clone().unwrap_or_default();
                if let Some(v) = t.views {
                    sub = format!("{sub}  \u{b7}  {} views", widgets::fmt_count(v));
                }
                let badge = if t.live { Some("LIVE".to_string()) } else { t.duration.map(|d| widgets::fmt_time(d as f64)) };
                (t.title.clone(), sub, t.thumbnail.clone(), badge, true, None)
            }
            SearchItem::Container(c) => match c.kind {
                ContainerKind::Playlist => (c.title.clone(), c.owner.clone().unwrap_or_default(), c.thumbnail.clone(), Some("PLAYLIST".to_string()), false, None),
                ContainerKind::Channel => {
                    let sub = c.subscribers.map(|s| format!("{} subscribers", widgets::fmt_count(s))).unwrap_or_else(|| "Channel".into());
                    (c.title.clone(), sub, None, None, false, Some(c.title.chars().next().unwrap_or('?').to_uppercase().to_string()))
                }
            },
        };

        let thumb_rect = if as_row {
            Rect::from_min_size(rect.min, vec2(rect.height() * 16.0 / 9.0, rect.height()))
        } else {
            Rect::from_min_size(rect.min, vec2(rect.width(), rect.width() * 9.0 / 16.0))
        };
        let tex = thumb_url.and_then(|u| self.thumbs.get(&u));
        let p = ui.painter().clone();
        widgets::picture(&p, thumb_rect, tex, 6);
        if let Some(letter) = &avatar {
            let c = thumb_rect.center();
            let r = (thumb_rect.height() * 0.32).min(40.0);
            p.circle_filled(c, r, ELEVATED);
            p.circle_stroke(c, r, Stroke::new(1.0, BORDER));
            p.text(c, Align2::CENTER_CENTER, letter, egui::FontId::proportional(r * 0.9), TEXT);
        }
        if let Some(b) = &badge {
            let live = b == "LIVE";
            let font = theme::mono();
            let w = p.layout_no_wrap(b.clone(), font.clone(), TEXT).size().x + 10.0;
            let pos = if b == "PLAYLIST" { thumb_rect.left_bottom() + vec2(6.0, -6.0) } else { thumb_rect.right_bottom() - vec2(6.0, 6.0) };
            let r = if b == "PLAYLIST" {
                Rect::from_min_size(pos - vec2(0.0, 18.0), vec2(w + 18.0, 18.0))
            } else {
                Rect::from_min_size(pos - vec2(w, 18.0), vec2(w, 18.0))
            };
            p.rect_filled(r, 3.0, if live { TEXT } else { Color32::from_black_alpha(190) });
            if b == "PLAYLIST" {
                icons::paint(&p, Icon::Playlist, pos2(r.left() + 10.0, r.center().y), 10.0, TEXT);
                p.text(pos2(r.left() + 18.0, r.center().y), Align2::LEFT_CENTER, b, font, TEXT);
            } else {
                p.text(r.center(), Align2::CENTER_CENTER, b, font, if live { BG } else { TEXT });
            }
        }
        if hover {
            p.rect_filled(thumb_rect, 6.0, Color32::from_white_alpha(22));
        }
        if current {
            p.rect_stroke(thumb_rect, 6.0, Stroke::new(1.5, TEXT), StrokeKind::Inside);
        }

        let (tx, ty, tw) = if as_row {
            (thumb_rect.right() + 14.0, rect.top() + 4.0, rect.right() - thumb_rect.right() - 14.0 - 34.0)
        } else {
            (rect.left() + 2.0, thumb_rect.bottom() + 10.0, rect.width() - 4.0)
        };
        let h = widgets::text_clipped(&p, pos2(tx, ty), &title, egui::FontId::proportional(14.0), TEXT, tw, 2);
        if !sub.is_empty() {
            widgets::text_clipped(&p, pos2(tx, ty + h + 4.0), &sub, theme::small(), TEXT2, tw, 1);
        }

        let mut action = if resp.clicked() {
            if is_track {
                ItemAction::Play
            } else {
                ItemAction::Open
            }
        } else {
            ItemAction::None
        };
        if let (true, SearchItem::Track(t)) = (hover, item) {
            // In the thumbnail's top-right corner, from the right.
            let buttons = [
                (ItemAction::Enqueue, Done::Queued, Icon::QueueAdd, "Add to queue", "Added to queue"),
                (ItemAction::Share, Done::Copied, Icon::Share, "Share (copy link)", "Link copied"),
            ];
            for (i, (act, done, icon, tip, done_tip)) in buttons.into_iter().enumerate() {
                let btn = Rect::from_center_size(thumb_rect.right_top() + vec2(-20.0 - 32.0 * i as f32, 20.0), vec2(28.0, 28.0));
                let br = ui.interact(btn, id.with(tip), Sense::click());
                let ticked = self.just_done(done, t);
                p.circle_filled(btn.center(), 14.0, Color32::from_black_alpha(200));
                let color = if br.hovered() || ticked { TEXT } else { TEXT2 };
                icons::paint(&p, if ticked { Icon::Check } else { icon }, btn.center(), 13.0, color);
                let br = br.on_hover_text(if ticked { done_tip } else { tip }).on_hover_cursor(egui::CursorIcon::PointingHand);
                if br.clicked() {
                    action = act;
                } else if br.hovered() && matches!(action, ItemAction::Play) {
                    action = ItemAction::None;
                }
            }
        }
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        action
    }

    fn listing_view(&mut self, ui: &mut Ui, state: &PlaybackState, current: Option<&str>) {
        let Some(listing) = &self.listing else { return };
        let title = listing.title.clone();
        let owner = listing.owner.clone();
        let (count, tracks): (Option<usize>, Vec<Track>) = match &listing.data {
            Loadable::Ready(l) => (Some(l.tracks.len()), l.tracks.clone()),
            _ => (None, Vec::new()),
        };
        let mut back = false;
        let mut play_all = false;
        let mut shuffle = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            back = widgets::icon_button(ui, Icon::Back, 16.0, false).on_hover_text("Back to results").clicked();
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let w = ui.available_width() - 250.0;
                let (r, _) = ui.allocate_exact_size(vec2(w.max(60.0), 22.0), Sense::hover());
                widgets::text_clipped(ui.painter(), r.min, &title, egui::FontId::proportional(17.0), TEXT, r.width(), 1);
                let mut meta = Vec::new();
                if let Some(o) = &owner {
                    meta.push(o.clone());
                }
                if let Some(n) = count {
                    meta.push(format!("{n} videos"));
                }
                let (r, _) = ui.allocate_exact_size(vec2(w.max(60.0), 16.0), Sense::hover());
                widgets::text_clipped(ui.painter(), r.min, &meta.join("  \u{b7}  "), theme::small(), TEXT2, r.width(), 1);
            });
            ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| {
                if count.is_some_and(|n| n > 0) {
                    shuffle = widgets::button(ui, "Shuffle", Some(Icon::Shuffle), false).clicked();
                    play_all = widgets::button(ui, "Play all", Some(Icon::Play), true).clicked();
                }
            });
        });
        ui.add_space(8.0);

        match self.listing.as_ref().map(|l| &l.data) {
            Some(Loadable::Loading) | Some(Loadable::Idle) => {
                ui.add_space(50.0);
                ui.vertical_centered(|ui| ui.label(egui::RichText::new("Loading\u{2026}").color(TEXT3)));
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            }
            Some(Loadable::Failed(e)) => {
                let kind = e.kind;
                ui.add_space(50.0);
                ui.vertical_centered(|ui| ui.label(egui::RichText::new(e.message.clone()).color(ERROR)));
                self.error_action(ui, kind, super::signin::Retry::Listing);
            }
            Some(Loadable::Ready(_)) if tracks.is_empty() => {
                ui.add_space(50.0);
                ui.vertical_centered(|ui| ui.label(egui::RichText::new("Nothing to play here").color(TEXT3)));
            }
            _ => {
                let mut play = None;
                let mut add = None;
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, t) in tracks.iter().enumerate() {
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let out = self.track_row(ui, rect, t, Some(i + 1), current == Some(t.id.as_str()), RowActions::Add);
                        if out.clicked {
                            play = Some(i);
                        }
                        if out.add {
                            add = Some(t.clone());
                        }
                    }
                });
                if let Some(i) = play {
                    self.play_tracks(tracks.clone(), i);
                }
                if let Some(t) = add {
                    self.enqueue(t);
                }
            }
        }
        if back {
            self.listing = None;
        }
        if play_all {
            self.play_tracks(tracks.clone(), 0);
        }
        if shuffle && !tracks.is_empty() {
            let pick = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize).unwrap_or(0)) % tracks.len();
            self.engine.send(Cmd::SetShuffle(true));
            self.play_tracks(tracks, pick);
        }
        let _ = state;
    }

    /// Things that stop playback from working, with a way to fix each.
    fn setup_banner(&mut self, ui: &mut Ui) {
        let Some(info) = &self.health else { return };
        let mut lines: Vec<(String, Option<&'static str>)> = Vec::new();
        if let Err(e) = &info.health.backend {
            lines.push((e.to_string(), Some("Download yt-dlp")));
        }
        if info.tools.mpv.is_none() {
            lines.push((format!("mpv is needed for sound. Install: {}", platform::install_hint()), None));
        }
        if info.tools.ffmpeg.is_none() {
            lines.push((format!("ffmpeg is needed for video (audio works without it). Install: {}", platform::install_hint()), None));
        }
        if let Some(w) = info.health.warnings.first() {
            lines.push((w.clone(), None));
        }
        if lines.is_empty() {
            return;
        }
        let mut download = false;
        let busy = self.updating;
        Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER)).corner_radius(theme::corner()).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (text, action) in &lines {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(text).size(12.5).color(TEXT2));
                    if let Some(a) = action {
                        ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, if busy { "Downloading\u{2026}" } else { a }, Some(Icon::Download), true).clicked() {
                                download = true;
                            }
                        });
                    }
                });
            }
        });
        ui.add_space(10.0);
        if download {
            self.start_update();
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ItemAction {
    None,
    Play,
    Open,
    Enqueue,
    Share,
}

impl PulseApp {
    /// The button under a failed search or listing, when there is something to do about it.
    fn error_action(&mut self, ui: &mut Ui, kind: ErrorKind, retry: super::signin::Retry) {
        let label = match kind {
            ErrorKind::Blocked => "Sign in\u{2026}",
            ErrorKind::BackendOutdated if self.updating => "Updating\u{2026}",
            ErrorKind::BackendOutdated => "Update yt-dlp",
            _ => return,
        };
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            let icon = (kind == ErrorKind::BackendOutdated).then_some(Icon::Download);
            if widgets::button(ui, label, icon, true).clicked() {
                match kind {
                    ErrorKind::Blocked => self.open_sign_in(Some(retry)),
                    _ => self.start_update(),
                }
            }
        });
    }
}
