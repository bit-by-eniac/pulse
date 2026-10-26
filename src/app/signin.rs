//! The optional "sign in through your browser" dialog. It explains what turning
//! it on does before anything is read, and is the only place that turns it on.

use super::widgets;
use super::*;
use crate::provider::SignInSource;
use eframe::egui::{Align, Align2, Frame, Layout as EguiLayout, Margin, RichText, Stroke};

/// What to try again once sign-in is turned on.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Retry {
    Track(usize),
    Search,
    Listing,
}

pub(super) struct SignInDialog {
    sources: Vec<SignInSource>,
    /// Highlighted browser; `None` = off.
    choice: Option<String>,
    retry: Option<Retry>,
}

/// "chrome" -> "Chrome".
pub(super) fn browser_label(id: &str) -> String {
    let mut chars = id.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl PulseApp {
    pub(super) fn open_sign_in(&mut self, retry: Option<Retry>) {
        let sources = self.provider.sign_in_sources();
        let choice = self.config.sign_in.clone().or_else(|| sources.first().map(|s| s.id.clone()));
        self.sign_in = Some(SignInDialog { sources, choice, retry });
        self.settings_open = false;
    }

    pub(super) fn sign_in_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.sign_in.as_mut() else { return };
        let frame = Frame::new()
            .fill(ELEVATED)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(theme::corner())
            .inner_margin(Margin::same(18));
        let width = (ctx.content_rect().width() - 32.0).clamp(260.0, 520.0);
        let mut close = false;
        let mut apply = false;
        let para = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).size(13.0).color(TEXT2));
        };
        let bullet = |ui: &mut Ui, text: &str| {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("\u{2022}").size(13.0).color(TEXT3));
                ui.add(egui::Label::new(RichText::new(text).size(13.0).color(TEXT2)).wrap());
            });
        };
        egui::Window::new("sign-in")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .min_width(width)
            .max_width(width)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(width - 36.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Sign in through your browser").size(17.0));
                    ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| {
                        close = widgets::icon_button(ui, icons::Icon::Close, 13.0, false).clicked();
                    });
                });
                egui::ScrollArea::vertical().auto_shrink([false, true]).max_height((ctx.content_rect().height() - 140.0).max(80.0)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    para(
                        ui,
                        "YouTube sometimes refuses to play a video for apps that aren\u{2019}t signed in, and asks to \u{201c}confirm you\u{2019}re not a bot\u{201d}. Pulse can\u{2019}t answer that check itself, but it can borrow the YouTube sign-in of a browser on this computer. This is optional.",
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new("WHAT HAPPENS").font(theme::mono()).color(TEXT3));
                    bullet(ui, "Only when YouTube refuses a request, yt-dlp reads the browser\u{2019}s cookies and repeats the request with them, as the browser itself would. For the next half hour it signs in right away, so later videos start quickly.");
                    bullet(ui, "Cookies are sent only to the site they belong to (YouTube). Pulse doesn\u{2019}t save, copy or upload them anywhere.");
                    bullet(ui, "You need to be signed in to YouTube in that browser.");
                    bullet(ui, "YouTube can occasionally limit an account it sees used with tools like this. If that worries you, sign in to YouTube with a second account in that browser.");
                    bullet(ui, "You can turn it off at any time in Settings.");
                    ui.add_space(2.0);
                    ui.label(RichText::new("BROWSER").font(theme::mono()).color(TEXT3));
                    if dialog.sources.is_empty() {
                        para(ui, "No supported browser was found on this computer. Install Firefox, sign in to YouTube there, and open this again.");
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            for s in &dialog.sources {
                                if widgets::chip(ui, &s.name, dialog.choice.as_deref() == Some(s.id.as_str())).clicked() {
                                    dialog.choice = Some(s.id.clone());
                                }
                            }
                            if widgets::chip(ui, "Off", dialog.choice.is_none()).clicked() {
                                dialog.choice = None;
                            }
                        });
                        if let Some(source) = dialog.choice.as_ref().and_then(|id| dialog.sources.iter().find(|s| &s.id == id)) {
                            ui.add_space(2.0);
                            ui.label(RichText::new("WHAT YOU\u{2019}LL SEE").font(theme::mono()).color(TEXT3));
                            match &source.note {
                                Some(note) => para(ui, note),
                                None => para(ui, &format!("Nothing to approve: {}\u{2019}s cookies can be read directly.", source.name)),
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| {
                    let label = match dialog.choice.as_ref().and_then(|id| dialog.sources.iter().find(|s| &s.id == id)) {
                        Some(s) => format!("Use {}", s.name),
                        None => "Keep off".to_string(),
                    };
                    apply = widgets::button(ui, &label, None, true).clicked();
                    close |= widgets::button(ui, "Not now", None, false).clicked();
                });
            });
        if apply {
            let dialog = self.sign_in.take().expect("dialog is open");
            self.config.sign_in = dialog.choice.clone();
            self.config.save();
            self.provider.set_sign_in(dialog.choice.clone());
            if dialog.choice.is_some() {
                match dialog.retry {
                    Some(Retry::Track(i)) => self.engine.send(Cmd::PlayIndex(i)),
                    Some(Retry::Search) => self.start_search(),
                    Some(Retry::Listing) => self.reopen_listing(),
                    None => {}
                }
            }
        } else if close {
            self.sign_in = None;
        }
    }
}
