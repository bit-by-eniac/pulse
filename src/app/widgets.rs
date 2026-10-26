//! Small custom widgets in the app's visual language.

use super::icons::{self, Icon};
use super::theme::{self, *};
use eframe::egui::{
    self, epaint::RectShape, pos2, text::LayoutJob, vec2, Align2, Color32, CornerRadius, FontId, Galley, Painter, Rect,
    Response, Sense, Stroke, StrokeKind, TextureId, Ui, Vec2,
};
use std::sync::Arc;

/// The side of the square `icon_button` takes for an icon of `size`.
pub fn icon_button_side(size: f32) -> f32 {
    (size + 14.0).max(30.0)
}

/// Square ghost icon button. `active` shows the solitary pip under the icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, active: bool) -> Response {
    let hit = icon_button_side(size);
    let (rect, resp) = ui.allocate_exact_size(vec2(hit, hit), Sense::click());
    paint_icon_button(ui.painter(), &resp, rect, icon, size, active, if active { TEXT } else { TEXT2 });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn paint_icon_button(p: &Painter, resp: &Response, rect: Rect, icon: Icon, size: f32, active: bool, base: Color32) {
    if resp.hovered() {
        p.rect_filled(rect.shrink(2.0), theme::corner(), ELEVATED);
    }
    let color = if resp.hovered() || resp.is_pointer_button_down_on() { TEXT } else { base };
    let center = rect.center() - if active { vec2(0.0, 1.5) } else { Vec2::ZERO };
    icons::paint(p, icon, center, size, color);
    if active {
        p.circle_filled(pos2(rect.center().x, rect.center().y + size / 2.0 + 2.5), 1.5, TEXT);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum PlayVis {
    Playing,
    Paused,
    Loading,
}

/// The one white disc.
pub fn play_button(ui: &mut Ui, vis: PlayVis, diameter: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(diameter, diameter), Sense::click());
    let p = ui.painter();
    let fill = if resp.hovered() { TEXT2 } else { TEXT };
    p.circle_filled(rect.center(), diameter / 2.0, fill);
    let glyph = diameter * 0.42;
    match vis {
        PlayVis::Playing => icons::paint(p, Icon::Pause, rect.center(), glyph, BG),
        PlayVis::Paused => icons::paint(p, Icon::Play, rect.center() + vec2(glyph * 0.06, 0.0), glyph, BG),
        PlayVis::Loading => {
            let t = ui.input(|i| i.time) as f32;
            let r = diameter * 0.2;
            let pts: Vec<_> = (0..=14)
                .map(|i| {
                    let a = t * 6.0 + i as f32 * 0.14 * std::f32::consts::PI;
                    rect.center() + vec2(a.cos(), a.sin()) * r
                })
                .collect();
            p.add(egui::Shape::line(pts, Stroke::new(2.0, BG)));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub struct BarOut {
    /// Fraction under the pointer while scrubbing or after a click.
    pub value: Option<f32>,
    pub released: bool,
}

/// 2px groove, elapsed part in chalk white, a thin vertical head on hover.
pub fn bar(ui: &mut Ui, width: f32, height: f32, frac: f32, enabled: bool) -> BarOut {
    let (rect, resp) = ui.allocate_exact_size(vec2(width.max(8.0), height), if enabled { Sense::click_and_drag() } else { Sense::hover() });
    let mut value = None;
    let mut released = false;
    if enabled {
        let frac_at = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.drag_started() {
                value = Some(frac_at(pos.x));
            }
            if resp.clicked() {
                value = Some(frac_at(pos.x));
                released = true;
            }
        }
        if resp.drag_stopped() {
            released = true;
            if value.is_none() {
                value = resp.interact_pointer_pos().map(|p| frac_at(p.x));
            }
        }
    }
    let shown = value.unwrap_or(frac).clamp(0.0, 1.0);
    let hot = enabled && (resp.hovered() || resp.dragged());
    let thick = if hot { 4.0 } else { 2.0 };
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), thick));
    let p = ui.painter();
    p.rect_filled(track, 1.0, GROOVE);
    let fill = Rect::from_min_size(track.min, vec2(track.width() * shown, thick));
    p.rect_filled(fill, 1.0, if enabled { TEXT } else { TEXT3 });
    if hot {
        let x = rect.left() + rect.width() * shown;
        p.rect_filled(Rect::from_center_size(pos2(x, rect.center().y), vec2(4.0, 12.0)), 1.0, TEXT);
    }
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let _ = resp;
    BarOut { value, released }
}

const SEGMENT_PAD: f32 = 9.0;

fn segment_widths(ui: &Ui, items: &[(Icon, &str)], labels: bool) -> Vec<f32> {
    let font = FontId::proportional(12.5);
    items
        .iter()
        .map(|(_, label)| {
            let text_w = if labels { ui.painter().layout_no_wrap(label.to_string(), font.clone(), TEXT).size().x + 6.0 } else { 0.0 };
            SEGMENT_PAD * 2.0 + 14.0 + text_w
        })
        .collect()
}

/// The width `segmented` takes.
pub fn segmented_width(ui: &Ui, items: &[(Icon, &str)], labels: bool) -> f32 {
    segment_widths(ui, items, labels).iter().sum::<f32>() + 4.0
}

/// `Video | Audio` style switch. Labels are dropped (and become tooltips) when `labels` is false.
pub fn segmented(ui: &mut Ui, items: &[(Icon, &str)], selected: usize, labels: bool) -> Option<usize> {
    let font = FontId::proportional(12.5);
    let pad = SEGMENT_PAD;
    let widths = segment_widths(ui, items, labels);
    let total = segmented_width(ui, items, labels);
    let (outer, outer_resp) = ui.allocate_exact_size(vec2(total, 28.0), Sense::hover());
    let p = ui.painter();
    p.rect(outer, CornerRadius::same(6), SURFACE, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let mut x = outer.left() + 2.0;
    let mut clicked = None;
    for (i, ((icon, label), w)) in items.iter().zip(&widths).enumerate() {
        let seg = Rect::from_min_size(pos2(x, outer.top() + 2.0), vec2(*w, outer.height() - 4.0));
        let resp = ui.interact(seg, outer_resp.id.with(("seg", i)), Sense::click());
        let on = i == selected;
        if on {
            ui.painter().rect_filled(seg, CornerRadius::same(4), ELEVATED);
        }
        let color = if on || resp.hovered() { TEXT } else { TEXT2 };
        let mut cx = seg.left() + pad + 7.0;
        icons::paint(ui.painter(), *icon, pos2(cx, seg.center().y), 14.0, color);
        let resp = if labels {
            cx += 7.0 + 6.0;
            ui.painter().text(pos2(cx, seg.center().y), Align2::LEFT_CENTER, label, font.clone(), color);
            resp
        } else {
            resp.on_hover_text(*label)
        };
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            clicked = Some(i);
        }
        x += w;
    }
    clicked
}

/// Pill-shaped filter chip.
pub fn chip(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let font = FontId::proportional(12.5);
    let w = ui.painter().layout_no_wrap(text.to_string(), font.clone(), TEXT).size().x + 24.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(255), TEXT);
        p.text(rect.center(), Align2::CENTER_CENTER, text, font, BG);
    } else {
        let border = if resp.hovered() { TEXT3 } else { BORDER };
        p.rect_stroke(rect, CornerRadius::same(255), Stroke::new(1.0, border), StrokeKind::Inside);
        p.text(rect.center(), Align2::CENTER_CENTER, text, font, if resp.hovered() { TEXT } else { TEXT2 });
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The width `button` takes.
pub fn button_width(ui: &Ui, text: &str, icon: bool) -> f32 {
    let text_w = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(13.0), TEXT).size().x;
    text_w + if icon { 22.0 } else { 0.0 } + 26.0
}

/// Ghost button (outlined) or primary button (white).
pub fn button(ui: &mut Ui, text: &str, icon: Option<Icon>, primary: bool) -> Response {
    let font = FontId::proportional(13.0);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(button_width(ui, text, icon.is_some()), 30.0), Sense::click());
    let p = ui.painter();
    let (fg, bg) = if primary {
        (BG, if resp.hovered() { TEXT2 } else { TEXT })
    } else {
        (if resp.hovered() { TEXT } else { TEXT2 }, Color32::TRANSPARENT)
    };
    p.rect_filled(rect, theme::corner(), bg);
    if !primary {
        p.rect_stroke(rect, theme::corner(), Stroke::new(1.0, if resp.hovered() { TEXT3 } else { BORDER }), StrokeKind::Inside);
    }
    let mut x = rect.left() + 13.0;
    if let Some(icon) = icon {
        icons::paint(p, icon, pos2(x + 7.0, rect.center().y), 14.0, fg);
        x += icon_w;
    }
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, text, font, fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Text cut to `rows` lines with an ellipsis.
pub fn clipped(painter: &Painter, text: &str, font: FontId, color: Color32, width: f32, rows: usize) -> Arc<Galley> {
    // The bundled fonts have no colour emoji; they would show up as empty boxes.
    let text: String = text
        .chars()
        .filter(|&c| !matches!(c as u32, 0x1F000..=0x1FFFF | 0x2600..=0x27BF | 0xFE00..=0xFE0F | 0x200D | 0x2B00..=0x2BFF))
        .collect();
    let mut job = LayoutJob::simple(text, font, color, width.max(1.0));
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = rows == 1;
    job.wrap.overflow_character = Some('\u{2026}');
    painter.layout_job(job)
}

/// Paints `text` clipped to `width`; returns the height used.
pub fn text_clipped(painter: &Painter, pos: egui::Pos2, text: &str, font: FontId, color: Color32, width: f32, rows: usize) -> f32 {
    let galley = clipped(painter, text, font, color, width, rows);
    let h = galley.size().y;
    painter.galley(pos, galley, color);
    h
}

/// Fills `rect` with a texture, cropped to cover (or the placeholder when absent).
pub fn picture(p: &Painter, rect: Rect, tex: Option<(TextureId, Vec2)>, radius: u8) {
    let cr = CornerRadius::same(radius);
    match tex {
        Some((id, size)) if size.x > 0.0 && size.y > 0.0 => {
            let (ra, rt) = (rect.aspect_ratio(), size.x / size.y);
            let uv = if rt > ra {
                let w = ra / rt;
                Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
            } else {
                let h = rt / ra;
                Rect::from_min_max(pos2(0.0, 0.5 - h / 2.0), pos2(1.0, 0.5 + h / 2.0))
            };
            p.add(RectShape::filled(rect, cr, Color32::WHITE).with_texture(id, uv));
        }
        _ => {
            p.rect_filled(rect, cr, SURFACE);
        }
    }
    p.rect_stroke(rect, cr, Stroke::new(1.0, BORDER), StrokeKind::Inside);
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn fmt_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1e3).replace(".0K", "K"),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
        _ => format!("{:.1}B", n as f64 / 1e9).replace(".0B", "B"),
    }
}
