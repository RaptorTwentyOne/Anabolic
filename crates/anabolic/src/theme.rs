//! Look and feel: a dense dark theme with one warm accent.
//!
//! Roles, not shades: the accent marks the brand, the selection and keyboard
//! focus, nothing else; data colors belong to their subsystem; amber and red
//! mean "look here". Text comes in three strengths, and the weakest one is
//! never placed on a raised surface or the selection, where it would fall
//! below 4.5:1.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};

// Surfaces, darkest to lightest.
/// Wells: graphs, inputs.
pub const BG: Color32 = Color32::from_rgb(0x0C, 0x0E, 0x12);
/// The app's base surface.
pub const PANEL: Color32 = Color32::from_rgb(0x14, 0x17, 0x1D);
/// Alternate table rows.
pub const STRIPE: Color32 = Color32::from_rgb(0x18, 0x1B, 0x22);
/// Popups, windows, cards.
pub const RAISED: Color32 = Color32::from_rgb(0x1C, 0x20, 0x28);
pub const HOVER: Color32 = Color32::from_rgb(0x24, 0x29, 0x34);
pub const PRESSED: Color32 = Color32::from_rgb(0x2C, 0x32, 0x40);
pub const BUTTON: Color32 = Color32::from_rgb(0x22, 0x27, 0x31);
pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x31, 0x3B);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x38, 0x3E, 0x4B);

// Text.
pub const TEXT: Color32 = Color32::from_rgb(0xE2, 0xE6, 0xEC);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0xA0, 0xA8, 0xB5);
/// Only on BG, PANEL and STRIPE.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x82, 0x8B, 0x9A);

/// Brand, selection mark, keyboard focus, the primary action.
pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x6A, 0x3D);
pub const SELECTION: Color32 = Color32::from_rgb(0x24, 0x30, 0x49);

// States.
pub const NEW_ROW: Color32 = Color32::from_rgb(0x1A, 0x3D, 0x29);
pub const EXITED_ROW: Color32 = Color32::from_rgb(0x4A, 0x1E, 0x22);
pub const SERVICE_TEXT: Color32 = Color32::from_rgb(0xE6, 0xA8, 0xC8);
pub const SUSPENDED_TEXT: Color32 = Color32::from_rgb(0x8A, 0x93, 0xA1);
/// Caution: elevated, needs attention.
pub const ELEVATED_TEXT: Color32 = Color32::from_rgb(0xF2, 0xC9, 0x6B);
pub const GOOD: Color32 = Color32::from_rgb(0x5F, 0xD0, 0x8A);
pub const BAD: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
/// Fills for warning surfaces (banners, destructive buttons).
pub const BAD_FILL: Color32 = Color32::from_rgb(0x5A, 0x1F, 0x1C);
pub const BAD_STRONG: Color32 = Color32::from_rgb(0xC0, 0x39, 0x2B);

// Data, one hue per subsystem.
pub const CPU: Color32 = Color32::from_rgb(0x4F, 0xC3, 0xF7);
/// A deeper shade of the CPU hue: kernel time is part of CPU time.
pub const KERNEL: Color32 = Color32::from_rgb(0x2B, 0x7D, 0xB8);
pub const MEMORY: Color32 = Color32::from_rgb(0xB3, 0x88, 0xFF);
pub const COMMIT: Color32 = Color32::from_rgb(0x8E, 0x6B, 0xD8);
/// Cached memory: still the memory hue, quieter.
pub const MEMORY_STANDBY: Color32 = Color32::from_rgb(0x5E, 0x55, 0x8C);
pub const DISK_READ: Color32 = Color32::from_rgb(0x66, 0xD1, 0x8F);
pub const DISK_WRITE: Color32 = Color32::from_rgb(0xFF, 0xB8, 0x4D);
pub const NET_RX: Color32 = Color32::from_rgb(0x4D, 0xD0, 0xC8);
pub const NET_TX: Color32 = Color32::from_rgb(0xF0, 0x7A, 0xC8);
pub const GPU: Color32 = Color32::from_rgb(0x8B, 0xD3, 0x4F);
/// Graph grid lines.
pub const GRID: Color32 = Color32::from_rgb(0x1C, 0x21, 0x2A);

// Type scale: five steps, nothing informational below the caption.
pub const CAPTION: f32 = 11.0;
pub const META: f32 = 12.0;
pub const BODY: f32 = 13.0;
pub const SUBTITLE: f32 = 16.0;
pub const TITLE: f32 = 20.0;

// Shape.
pub const RADIUS_CONTROL: u8 = 4;
pub const RADIUS_PANEL: u8 = 6;

const SEMIBOLD: &str = "semibold";
const ICONS: &str = "icons";
const BRAND: &str = "brand";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// The brand voice: the wordmark and page titles, nowhere else.
pub fn brand(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BRAND.into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

/// Segoe Fluent Icons (Windows 11) / Segoe MDL2 Assets (Windows 10).
#[cfg(windows)]
pub mod icon {
    pub const MENU: &str = "\u{E700}";
    pub const PROCESSES: &str = "\u{E8FD}";
    pub const PERFORMANCE: &str = "\u{E9D2}";
    pub const SEARCH: &str = "\u{E721}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const PAUSE: &str = "\u{E769}";
    pub const PLAY: &str = "\u{E768}";
    pub const WARNING: &str = "\u{E7BA}";
    pub const LOCK: &str = "\u{E72E}";
    pub const CLOSE: &str = "\u{E711}";
    pub const CHECK: &str = "\u{E73E}";
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CHEVRON_UP: &str = "\u{E70E}";
    pub const SHIELD: &str = "\u{EA18}";
    pub const SPEED: &str = "\u{EC4A}";
    pub const HISTORY: &str = "\u{E81C}";
    pub const RULES: &str = "\u{E9E9}";
    /// Two gears.
    pub const SERVICES: &str = "\u{E9F5}";
    /// A power button.
    pub const STARTUP: &str = "\u{E7E8}";
    /// A monitor with a pulse.
    pub const STUTTERS: &str = "\u{E9D9}";
    /// A globe.
    pub const NETWORK: &str = "\u{E774}";
    /// A recording dot.
    pub const RECORD: &str = "\u{E7C8}";
    /// A robot: AI assistants (the MCP page).
    pub const MCP: &str = "\u{E99A}";
    /// An arrow into a tray: a new release.
    pub const UPDATE: &str = "\u{E896}";
}

/// The symbols in egui's own fonts (its emoji and icon fonts), which come
/// with Anabolic: Linux has no icon font everyone has.
#[cfg(target_os = "linux")]
pub mod icon {
    pub const MENU: &str = "\u{2630}";
    pub const PROCESSES: &str = "\u{1F5D0}";
    pub const PERFORMANCE: &str = "\u{1F4C8}";
    pub const SEARCH: &str = "\u{1F50D}";
    pub const SETTINGS: &str = "\u{2699}";
    pub const PAUSE: &str = "\u{23F8}";
    pub const PLAY: &str = "\u{25B6}";
    pub const WARNING: &str = "\u{26A0}";
    pub const LOCK: &str = "\u{1F512}";
    pub const CLOSE: &str = "\u{2716}";
    pub const CHECK: &str = "\u{2714}";
    pub const CHEVRON_DOWN: &str = "\u{23F7}";
    pub const CHEVRON_UP: &str = "\u{23F6}";
    pub const SHIELD: &str = "\u{1F6E1}";
    pub const SPEED: &str = "\u{26A1}";
    pub const HISTORY: &str = "\u{1F553}";
    pub const RULES: &str = "\u{1F4DC}";
    /// Tools.
    pub const SERVICES: &str = "\u{2692}";
    /// A rocket.
    pub const STARTUP: &str = "\u{1F680}";
    /// A game controller.
    pub const STUTTERS: &str = "\u{1F3AE}";
    /// Networked computers.
    pub const NETWORK: &str = "\u{1F5A7}";
    /// A recording dot.
    pub const RECORD: &str = "\u{23FA}";
    /// A plug: AI assistants connect here (the MCP page).
    pub const MCP: &str = "\u{1F50C}";
    /// Arrows going round: a new release.
    pub const UPDATE: &str = "\u{27F3}";
}

/// Heat behind busy cells: quiet below the top of the range, warm only for
/// real peaks, so the culprit stands out instead of every busy row.
pub fn heat(intensity: f32) -> Color32 {
    let i = intensity.clamp(0.0, 1.0);
    if i < 0.6 {
        // A neutral lift, enough to rank values at a glance.
        let a = (0.025 + i / 0.6 * 0.06) * 255.0;
        return Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, a as u8);
    }
    let t = (i - 0.6) / 0.4;
    let amber = [0xE8, 0xA3, 0x3D];
    let red = [0xE5, 0x48, 0x4D];
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    let a = ((0.16 + t * 0.22) * 255.0) as u8;
    Color32::from_rgba_unmultiplied(
        mix(amber[0], red[0]),
        mix(amber[1], red[1]),
        mix(amber[2], red[2]),
        a,
    )
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER_STRONG);
    v.extreme_bg_color = BG;
    v.faint_bg_color = STRIPE;
    v.code_bg_color = RAISED;
    v.override_text_color = Some(TEXT);
    v.weak_text_color = Some(TEXT_WEAK);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.window_corner_radius = CornerRadius::same(RADIUS_PANEL);
    v.menu_corner_radius = CornerRadius::same(RADIUS_PANEL);
    v.striped = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.weak_bg_fill = BUTTON;
    w.inactive.bg_fill = BUTTON;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x48, 0x50, 0x5F));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    // Pressed and keyboard-focused controls carry the accent.
    w.active.weak_bg_fill = PRESSED;
    w.active.bg_fill = PRESSED;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.weak_bg_fill = PRESSED;
    w.open.bg_fill = PRESSED;
    w.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    for wv in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        wv.corner_radius = CornerRadius::same(RADIUS_CONTROL);
    }

    ctx.set_visuals(v);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 3.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        // Labels must not swallow clicks meant for table rows.
        style.interaction.selectable_labels = false;
        style.text_styles = [
            (TextStyle::Heading, semibold(SUBTITLE)),
            (TextStyle::Body, FontId::new(BODY, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(BODY, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(CAPTION, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(META, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

/// The faces taken from the system.
#[derive(Clone, Copy)]
enum Face {
    Regular,
    Semibold,
    Mono,
    Symbol,
    Icons,
}

/// Windows' own faces: Segoe UI (its digits are tabular, so live numbers
/// keep still), Segoe UI Semibold for hierarchy, Cascadia Mono for code and
/// addresses, the Fluent icon font for icons.
#[cfg(windows)]
fn system_face(face: Face) -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR")
        .map(|w| format!(r"{w}\Fonts"))
        .unwrap_or_else(|_| r"C:\Windows\Fonts".into());
    let files: &[&str] = match face {
        Face::Regular => &["segoeui.ttf"],
        Face::Semibold => &["seguisb.ttf", "segoeuib.ttf"],
        Face::Mono => &["CascadiaMono.ttf", "consola.ttf"],
        Face::Symbol => &["seguisym.ttf"],
        Face::Icons => &["SegoeIcons.ttf", "segmdl2.ttf"],
    };
    files
        .iter()
        .find_map(|file| std::fs::read(format!(r"{dir}\{file}")).ok())
}

/// The desktop's own faces, as fontconfig picks them (Noto Sans on KDE,
/// Cantarell or Adwaita Sans on GNOME), medium weight for hierarchy, its
/// monospace for code and addresses. Icons come from egui's fonts.
#[cfg(target_os = "linux")]
fn system_face(face: Face) -> Option<Vec<u8>> {
    let pattern = match face {
        Face::Regular => "sans-serif",
        Face::Semibold => "sans-serif:weight=medium",
        Face::Mono => "monospace",
        Face::Symbol | Face::Icons => return None,
    };
    let out = std::process::Command::new("fc-match")
        .args(["--format=%{file}", pattern])
        .output()
        .ok()?;
    let file = String::from_utf8(out.stdout).ok()?;
    // egui reads TrueType and OpenType outlines, not bitmap or Type 1 faces.
    let usable = [".ttf", ".otf", ".TTF", ".OTF"]
        .iter()
        .any(|e| file.ends_with(e));
    usable.then(|| std::fs::read(file.trim()).ok()).flatten()
}

/// The system's faces (see `system_face`); Aldrich, embedded (OFL, see
/// assets/fonts), is the brand. egui's own fonts stay as fallbacks.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut loaded = |name: &str, face: Face| -> bool {
        let Some(bytes) = system_face(face) else {
            return false;
        };
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_owned(bytes)));
        true
    };
    let segoe = loaded("segoe", Face::Regular);
    let semibold = loaded("segoe-semibold", Face::Semibold);
    let mono = loaded("cascadia", Face::Mono);
    let symbol = loaded("segoe-symbol", Face::Symbol);
    let icons = loaded("fluent", Face::Icons);
    fonts.font_data.insert(
        "aldrich".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Aldrich-Regular.ttf"
        ))),
    );

    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    if segoe {
        proportional.insert(0, "segoe".into());
    }
    if symbol {
        proportional.push("segoe-symbol".into());
    }
    let proportional = proportional.clone();
    if mono {
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "cascadia".into());
    }
    let mut strong = Vec::new();
    if semibold {
        strong.push("segoe-semibold".into());
    }
    strong.extend(proportional.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), strong);
    // Aldrich has no ğ, ş or İ: those come from Segoe UI.
    let mut brand = vec!["aldrich".into()];
    brand.extend(proportional.iter().cloned());
    fonts.families.insert(FontFamily::Name(BRAND.into()), brand);
    let mut glyphs = Vec::new();
    if icons {
        glyphs.push("fluent".into());
    }
    glyphs.extend(defaults);
    fonts
        .families
        .insert(FontFamily::Name(ICONS.into()), glyphs);
    ctx.set_fonts(fonts);
}
