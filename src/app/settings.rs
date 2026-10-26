//! Settings window: what is installed, updating the YouTube backend, extra arguments.

use super::icons::Icon;
use super::widgets;
use super::*;
use eframe::egui::{Align, Align2, Frame, Layout as EguiLayout, Margin, RichText, Stroke, TextEdit};

impl PulseApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context, _state: &PlaybackState) {
        if !self.settings_open {
            return;
        }
        let frame = Frame::new()
            .fill(ELEVATED)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(theme::corner())
            .inner_margin(Margin::same(18));
        let mut close = false;
        let mut apply_args = false;
        let mut update = false;
        let mut open_sign_in = false;
        let width = (ctx.content_rect().width() - 32.0).clamp(260.0, 520.0);
        egui::Window::new("settings")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([width, 0.0])
            .frame(frame)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Settings").size(17.0));
                    ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| {
                        close = widgets::icon_button(ui, Icon::Close, 13.0, false).clicked();
                    });
                });
                ui.label(RichText::new("TOOLS").font(theme::mono()).color(TEXT3));
                let tool_row = |ui: &mut Ui, name: &str, status: Result<String, String>| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).color(TEXT));
                        ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| match &status {
                            Ok(v) => ui.label(RichText::new(v).font(theme::mono()).color(TEXT2)),
                            Err(e) => ui.label(RichText::new(e).font(theme::mono()).color(ERROR)),
                        });
                    });
                };
                match &self.health {
                    None => {
                        ui.label(RichText::new("Checking\u{2026}").color(TEXT3));
                    }
                    Some(info) => {
                        tool_row(ui, "yt-dlp", info.health.backend.clone());
                        let path = |p: &Option<std::path::PathBuf>| match p {
                            Some(p) => Ok(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
                            None => Err("not found".to_string()),
                        };
                        tool_row(ui, "mpv (audio)", path(&info.tools.mpv));
                        tool_row(ui, "ffmpeg (video)", path(&info.tools.ffmpeg));
                        for w in &info.health.warnings {
                            ui.label(RichText::new(w).size(12.0).color(TEXT2));
                        }
                    }
                }
                ui.horizontal(|ui| {
                    let busy = self.updating;
                    if widgets::button(ui, if busy { "Updating\u{2026}" } else { "Update yt-dlp" }, Some(Icon::Download), false).clicked() {
                        update = true;
                    }
                    if let Some(m) = &self.update_message {
                        ui.label(RichText::new(m).size(12.0).color(TEXT2));
                    }
                });
                ui.label(RichText::new("If YouTube stops working, updating yt-dlp usually fixes it.").size(12.0).color(TEXT3));
                ui.add_space(4.0);
                ui.label(RichText::new("YOUTUBE SIGN-IN").font(theme::mono()).color(TEXT3));
                ui.horizontal(|ui| {
                    let status = match &self.config.sign_in {
                        Some(id) => format!("{} \u{2014} used only when YouTube asks to confirm you\u{2019}re not a bot", super::signin::browser_label(id)),
                        None => "Off \u{2014} Pulse never reads browser cookies".to_string(),
                    };
                    ui.label(RichText::new(status).size(12.0).color(TEXT2));
                    ui.with_layout(EguiLayout::right_to_left(Align::Center), |ui| {
                        if widgets::button(ui, "Change\u{2026}", None, false).clicked() {
                            open_sign_in = true;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(RichText::new("EXTRA YT-DLP ARGUMENTS").font(theme::mono()).color(TEXT3));
                ui.horizontal(|ui| {
                    let r = ui.add(
                        TextEdit::singleline(&mut self.args_text)
                            .hint_text("e.g. --proxy socks5://127.0.0.1:1080")
                            .desired_width(width - 130.0),
                    );
                    if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || widgets::button(ui, "Apply", None, false).clicked() {
                        apply_args = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("SHORTCUTS").font(theme::mono()).color(TEXT3));
                let search = format!("/ or {}+K", platform::command_key());
                let keys = [
                    ("Space", "play / pause"),
                    ("\u{2190} \u{2192}", "seek 5s (Shift: 30s)"),
                    ("\u{2191} \u{2193}", "volume"),
                    ("N / P", "next / previous"),
                    ("V", "video / audio"),
                    ("F", "fullscreen"),
                    ("M", "mute"),
                    ("Q", "queue"),
                    (search.as_str(), "search"),
                ];
                for (k, d) in keys {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(k).font(theme::mono()).color(TEXT));
                        ui.label(RichText::new(d).size(12.0).color(TEXT2));
                    });
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{} {}", platform::APP_NAME, env!("CARGO_PKG_VERSION"))).font(theme::mono()).color(TEXT3));
            });
        if close {
            self.settings_open = false;
        }
        if update {
            self.start_update();
        }
        if open_sign_in {
            self.open_sign_in(None);
        }
        if apply_args {
            let args: Vec<String> = self.args_text.split_whitespace().map(str::to_string).collect();
            self.provider.set_extra_args(args.clone());
            self.config.ytdlp_args = args;
            self.config.save();
        }
    }
}
