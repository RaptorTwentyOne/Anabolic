//! Look and feel: a dense dark theme with one hot accent.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};

pub const BG: Color32 = Color32::from_rgb(0x0E, 0x10, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x14, 0x17, 0x1D);
pub const RAISED: Color32 = Color32::from_rgb(0x1B, 0x1F, 0x27);
pub const STRIPE: Color32 = Color32::from_rgb(0x17, 0x1A, 0x21);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2B, 0x35);
pub const TEXT: Color32 = Color32::from_rgb(0xD8, 0xDD, 0xE6);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x86, 0x8F, 0x9E);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x5A, 0x62, 0x70);

pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x6A, 0x3D);
pub const SELECTION: Color32 = Color32::from_rgb(0x2B, 0x3A, 0x5C);

/// Heat shading for busy cells.
pub const HEAT: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x3D);
pub const NEW_ROW: Color32 = Color32::from_rgb(0x1C, 0x4A, 0x2C);
pub const EXITED_ROW: Color32 = Color32::from_rgb(0x5C, 0x1F, 0x22);
pub const SERVICE_TEXT: Color32 = Color32::from_rgb(0xE6, 0xA8, 0xC8);
pub const SUSPENDED_TEXT: Color32 = Color32::from_rgb(0x6E, 0x76, 0x84);
pub const ELEVATED_TEXT: Color32 = Color32::from_rgb(0xF2, 0xC9, 0x6B);
pub const GOOD: Color32 = Color32::from_rgb(0x5F, 0xD0, 0x8A);
pub const BAD: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x5C);

pub const CPU: Color32 = Color32::from_rgb(0x4F, 0xC3, 0xF7);
pub const KERNEL: Color32 = Color32::from_rgb(0xFF, 0x6A, 0x3D);
pub const MEMORY: Color32 = Color32::from_rgb(0xB3, 0x88, 0xFF);
pub const COMMIT: Color32 = Color32::from_rgb(0x7C, 0x5C, 0xC4);
pub const DISK_READ: Color32 = Color32::from_rgb(0x66, 0xD1, 0x8F);
pub const DISK_WRITE: Color32 = Color32::from_rgb(0xFF, 0xB8, 0x4D);
pub const NET_RX: Color32 = Color32::from_rgb(0x4D, 0xD0, 0xC8);
pub const NET_TX: Color32 = Color32::from_rgb(0xF0, 0x7A, 0xC8);
pub const GPU: Color32 = Color32::from_rgb(0x8B, 0xD3, 0x4F);

pub fn heat(intensity: f32) -> Color32 {
    let a = (intensity.clamp(0.0, 1.0) * 0.55 * 255.0) as u8;
    Color32::from_rgba_unmultiplied(HEAT.r(), HEAT.g(), HEAT.b(), a)
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.extreme_bg_color = BG;
    v.faint_bg_color = STRIPE;
    v.code_bg_color = RAISED;
    v.override_text_color = Some(TEXT);
    v.weak_text_color = Some(TEXT_WEAK);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(4);
    v.striped = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_fill = RAISED;
    w.hovered.weak_bg_fill = Color32::from_rgb(0x26, 0x2C, 0x38);
    w.hovered.bg_fill = Color32::from_rgb(0x26, 0x2C, 0x38);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3A, 0x42, 0x52));
    w.active.weak_bg_fill = Color32::from_rgb(0x30, 0x38, 0x48);
    w.active.bg_fill = Color32::from_rgb(0x30, 0x38, 0x48);
    for wv in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        wv.corner_radius = CornerRadius::same(4);
    }

    ctx.set_visuals(v);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 3.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 20.0;
        // Labels must not swallow clicks meant for table rows.
        style.interaction.selectable_labels = false;
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(17.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

/// Segoe UI reads far better than the bundled font on Windows; keep egui's
/// own fonts as fallbacks for symbols and emoji.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var("WINDIR")
        .map(|w| format!(r"{w}\Fonts"))
        .unwrap_or_else(|_| r"C:\Windows\Fonts".into());
    let mut load = |name: &str, file: &str, family: FontFamily, front: bool| {
        if let Ok(bytes) = std::fs::read(format!(r"{dir}\{file}")) {
            fonts
                .font_data
                .insert(name.into(), Arc::new(FontData::from_owned(bytes)));
            let list = fonts.families.entry(family).or_default();
            if front {
                list.insert(0, name.into())
            } else {
                list.push(name.into())
            }
        }
    };
    load("segoe", "segoeui.ttf", FontFamily::Proportional, true);
    load("cascadia", "CascadiaMono.ttf", FontFamily::Monospace, true);
    load(
        "segoe-symbol",
        "seguisym.ttf",
        FontFamily::Proportional,
        false,
    );
    ctx.set_fonts(fonts);
}
