//! Colours, text sizes and lazily loaded fallback fonts.
//!
//! The palette follows the "Monochrome Audio Interface" design: graphite
//! surfaces, hairline borders, chalk-white for the one thing that matters.

use crate::platform::{self, Script};
use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Stroke, TextStyle};
use std::collections::HashSet;
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x0d, 0x0e, 0x11);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x17, 0x1d);
pub const ELEVATED: Color32 = Color32::from_rgb(0x20, 0x22, 0x2a);
pub const BORDER: Color32 = Color32::from_rgb(0x27, 0x29, 0x32);
pub const TEXT: Color32 = Color32::from_rgb(0xf4, 0xf4, 0xf6);
pub const TEXT2: Color32 = Color32::from_rgb(0x9a, 0x9d, 0xa8);
pub const TEXT3: Color32 = Color32::from_rgb(0x63, 0x66, 0x74);
pub const GROOVE: Color32 = Color32::from_rgb(0x2c, 0x2e, 0x38);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0xb4, 0xab);

pub const RADIUS: u8 = 4;

pub fn corner() -> CornerRadius {
    CornerRadius::same(RADIUS)
}

pub fn small() -> FontId {
    FontId::proportional(11.5)
}
pub fn body() -> FontId {
    FontId::proportional(13.5)
}
pub fn title() -> FontId {
    FontId::proportional(15.0)
}
pub fn mono() -> FontId {
    FontId::monospace(11.5)
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(TextStyle::Body, body());
        style.text_styles.insert(TextStyle::Button, body());
        style.text_styles.insert(TextStyle::Small, small());
        style.text_styles.insert(TextStyle::Monospace, mono());
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(20.0));
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.interaction.selectable_labels = false;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.window_fill = ELEVATED;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = corner();
        v.extreme_bg_color = SURFACE;
        v.faint_bg_color = SURFACE;
        v.selection.bg_fill = Color32::from_rgb(0x3a, 0x3d, 0x4a);
        v.selection.stroke = Stroke::new(1.0, TEXT2);
        v.text_cursor.stroke = Stroke::new(1.5, TEXT);
        v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(115) };
        v.window_shadow = v.popup_shadow;
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = corner();
            w.bg_stroke = Stroke::new(1.0, BORDER);
        }
        v.widgets.noninteractive.bg_fill = SURFACE;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT2);
        v.widgets.inactive.bg_fill = SURFACE;
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT2);
        v.widgets.hovered.bg_fill = ELEVATED;
        v.widgets.hovered.weak_bg_fill = ELEVATED;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, TEXT3);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.bg_fill = ELEVATED;
        v.widgets.active.weak_bg_fill = ELEVATED;
        v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.bg_stroke = Stroke::new(1.0, TEXT2);
    });
}

/// Loads a system font for a writing system the bundled fonts lack, but only
/// once such text actually shows up, so the common case stays light on RAM.
pub struct FallbackFonts {
    defs: FontDefinitions,
    loaded: HashSet<Script>,
}

impl FallbackFonts {
    pub fn new(ctx: &egui::Context) -> Self {
        let defs = base_fonts();
        ctx.set_fonts(defs.clone());
        Self { defs, loaded: HashSet::new() }
    }

    pub fn ensure_for(&mut self, ctx: &egui::Context, text: &str) {
        let mut changed = false;
        for script in scripts_in(text) {
            if !self.loaded.insert(script) {
                continue;
            }
            let Some(bytes) = platform::fallback_fonts(script).into_iter().find_map(|p| map_font(&p)) else {
                continue;
            };
            let name = format!("fallback-{script:?}");
            self.defs.font_data.insert(name.clone(), Arc::new(FontData::from_static(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                self.defs.families.entry(family).or_default().push(name.clone());
            }
            changed = true;
        }
        if changed {
            ctx.set_fonts(self.defs.clone());
        }
    }
}

/// egui's default fonts, except that the monospace face (Hack) is cut down to Latin,
/// punctuation and arrows: labels and times need no more. 48 KB instead of 309 KB;
/// any other character falls back to Ubuntu, as with egui's defaults.
/// (`pyftsubset Hack-Regular.ttf --unicodes=U+0020-007E,U+00A0-017F,U+2010-2027,
/// U+2030-203A,U+2190-2193,U+2212 --no-hinting --layout-features=''`)
fn base_fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    let hack = include_bytes!("../../assets/fonts/Hack-Regular-subset.ttf");
    defs.font_data.insert("Hack".into(), Arc::new(FontData::from_static(hack)));
    defs.font_data.insert("Ubuntu-Light".into(), Arc::new(FontData::from_static(epaint_default_fonts::UBUNTU_LIGHT)));
    let icons = FontData::from_static(epaint_default_fonts::EGUI_ICONS).tweak(FontTweak { scale: 0.90, ..Default::default() });
    defs.font_data.insert("egui-icons".into(), Arc::new(icons));
    defs.families.insert(FontFamily::Monospace, ["Hack", "Ubuntu-Light", "egui-icons"].map(String::from).into());
    defs.families.insert(FontFamily::Proportional, ["Ubuntu-Light", "egui-icons"].map(String::from).into());
    defs
}

/// Maps a font file instead of reading it: CJK fonts are 20+ MB, of which only
/// the glyphs actually drawn get paged in. Mapped once per writing system, for good.
fn map_font(path: &std::path::Path) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system font files are not modified while the app runs.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    Some(&Box::leak(Box::new(map))[..])
}

fn scripts_in(text: &str) -> Vec<Script> {
    let mut found = Vec::new();
    let mut add = |s: Script| {
        if !found.contains(&s) {
            found.push(s);
        }
    };
    for c in text.chars() {
        let u = c as u32;
        if u < 0x590 {
            continue;
        }
        match u {
            0x0590..=0x05FF => add(Script::Hebrew),
            0x0600..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => add(Script::Arabic),
            0x0900..=0x097F => add(Script::Devanagari),
            0x0E00..=0x0E7F => add(Script::Thai),
            0x1100..=0x11FF | 0x2E80..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF => {
                add(Script::Cjk)
            }
            _ => {}
        }
    }
    found
}
