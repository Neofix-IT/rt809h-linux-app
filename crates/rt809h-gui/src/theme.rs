//! Colours, fonts and widget styling.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Theme, Vec2,
};

/// Colours used by the custom widgets, picked to match the active theme.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x12, 0x14, 0x19),
    surface: Color32::from_rgb(0x1b, 0x1e, 0x25),
    surface2: Color32::from_rgb(0x23, 0x27, 0x30),
    border: Color32::from_rgb(0x2c, 0x31, 0x3c),
    text: Color32::from_rgb(0xe6, 0xe8, 0xec),
    dim: Color32::from_rgb(0x9a, 0xa1, 0xad),
    faint: Color32::from_rgb(0x5c, 0x63, 0x70),
    accent: Color32::from_rgb(0x4f, 0x8c, 0xff),
    on_accent: Color32::WHITE,
    ok: Color32::from_rgb(0x3f, 0xb9, 0x7a),
    warn: Color32::from_rgb(0xe5, 0xa2, 0x3a),
    err: Color32::from_rgb(0xec, 0x5b, 0x53),
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf2, 0xf3, 0xf6),
    surface: Color32::WHITE,
    surface2: Color32::from_rgb(0xee, 0xf0, 0xf4),
    border: Color32::from_rgb(0xdb, 0xdf, 0xe6),
    text: Color32::from_rgb(0x1c, 0x1f, 0x24),
    dim: Color32::from_rgb(0x5f, 0x66, 0x72),
    faint: Color32::from_rgb(0xa4, 0xaa, 0xb4),
    accent: Color32::from_rgb(0x2f, 0x6f, 0xeb),
    on_accent: Color32::WHITE,
    ok: Color32::from_rgb(0x1f, 0x9d, 0x5a),
    warn: Color32::from_rgb(0xb7, 0x79, 0x1f),
    err: Color32::from_rgb(0xd1, 0x38, 0x2f),
};

impl Palette {
    pub fn of(ui: &egui::Ui) -> Self {
        if ui.visuals().dark_mode {
            DARK
        } else {
            LIGHT
        }
    }
}

/// Mix `c` over `base` with the given opacity (0..=1).
pub fn tint(base: Color32, c: Color32, a: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * a).round() as u8;
    Color32::from_rgb(m(base.r(), c.r()), m(base.g(), c.g()), m(base.b(), c.b()))
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, p) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        ctx.style_mut_of(theme, |s| {
            s.text_styles = [
                (TextStyle::Heading, FontId::new(18.0, bold())),
                (TextStyle::Body, FontId::proportional(14.0)),
                (TextStyle::Button, FontId::proportional(14.0)),
                (TextStyle::Small, FontId::proportional(12.0)),
                (TextStyle::Monospace, FontId::monospace(13.0)),
            ]
            .into();
            s.spacing.item_spacing = Vec2::new(8.0, 8.0);
            s.spacing.button_padding = Vec2::new(12.0, 6.0);
            s.spacing.interact_size.y = 30.0;
            s.spacing.combo_height = 360.0;
            s.spacing.menu_margin = Margin::same(8);

            let v = &mut s.visuals;
            let r = CornerRadius::same(7);
            v.panel_fill = p.bg;
            v.window_fill = p.surface;
            v.window_stroke = Stroke::new(1.0, p.border);
            v.window_corner_radius = CornerRadius::same(12);
            v.menu_corner_radius = CornerRadius::same(10);
            v.extreme_bg_color = p.surface2;
            v.faint_bg_color = p.surface2;
            v.code_bg_color = p.surface2;
            v.hyperlink_color = p.accent;
            v.warn_fg_color = p.warn;
            v.error_fg_color = p.err;
            v.selection.bg_fill = tint(p.surface, p.accent, 0.35);
            v.selection.stroke = Stroke::new(1.0, p.text);

            let w = &mut v.widgets;
            w.noninteractive.bg_fill = p.surface;
            w.noninteractive.weak_bg_fill = p.surface;
            w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
            w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
            w.noninteractive.corner_radius = r;

            w.inactive.bg_fill = p.surface2;
            w.inactive.weak_bg_fill = p.surface2;
            w.inactive.bg_stroke = Stroke::new(1.0, p.border);
            w.inactive.fg_stroke = Stroke::new(1.0, p.text);
            w.inactive.corner_radius = r;

            w.hovered.bg_fill = tint(p.surface2, p.text, 0.06);
            w.hovered.weak_bg_fill = tint(p.surface2, p.text, 0.06);
            w.hovered.bg_stroke = Stroke::new(1.0, tint(p.border, p.accent, 0.6));
            w.hovered.fg_stroke = Stroke::new(1.5, p.text);
            w.hovered.corner_radius = r;
            w.hovered.expansion = 0.0;

            w.active.bg_fill = tint(p.surface2, p.accent, 0.25);
            w.active.weak_bg_fill = tint(p.surface2, p.accent, 0.25);
            w.active.bg_stroke = Stroke::new(1.0, p.accent);
            w.active.fg_stroke = Stroke::new(1.5, p.text);
            w.active.corner_radius = r;
            w.active.expansion = 0.0;

            w.open = w.active;
        });
    }
}

/// Use the desktop's sans-serif font when one can be found; the built-in
/// font stays as a fallback for missing glyphs.
fn install_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let regular = system_font("sans-serif").and_then(|p| std::fs::read(p).ok());
    let heavy = system_font("sans-serif:bold").and_then(|p| std::fs::read(p).ok());

    let mut prop = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(d) = regular {
        defs.font_data
            .insert("system".into(), Arc::new(FontData::from_owned(d)));
        prop.insert(0, "system".into());
    }
    let mut bold_list = prop.clone();
    if let Some(d) = heavy {
        defs.font_data
            .insert("system-bold".into(), Arc::new(FontData::from_owned(d)));
        bold_list.insert(0, "system-bold".into());
    }
    defs.families.insert(FontFamily::Proportional, prop);
    defs.families.insert(bold(), bold_list);
    ctx.set_fonts(defs);
}

fn system_font(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let ok = out.status.success()
        && (path.ends_with(".ttf") || path.ends_with(".otf"))
        && std::path::Path::new(&path).exists();
    ok.then_some(path)
}
