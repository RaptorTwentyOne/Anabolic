// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Performance > Displays: the monitors as they sit on the desktop, and for
//! each the GPU it hangs off, its mode against the best it offers, HDR and
//! scaling. A monitor left at 60 Hz that does 144, or run below its own
//! resolution, is pointed out.

use anabolic_core::displays::Display;
use eframe::egui::{
    self, Align, Align2, FontId, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, pos2, vec2,
};

use crate::{theme, widgets};

/// Who scales a picture up to a panel run below its own resolution, and
/// where its refresh rate is set.
#[cfg(windows)]
const SCALER: &str = "Windows";
#[cfg(windows)]
const SETTINGS: &str = "Windows' display settings, Advanced display";
#[cfg(target_os = "linux")]
const SCALER: &str = "the desktop";
#[cfg(target_os = "linux")]
const SETTINGS: &str = "the desktop's display settings";

/// What a monitor is called on the map: Windows' number for it.
#[cfg(windows)]
fn tag(d: &Display) -> String {
    d.device.trim_start_matches(r"\\.\DISPLAY").to_string()
}

/// What a monitor is called on the map: its connector (eDP-1, DP-2).
#[cfg(target_os = "linux")]
fn tag(d: &Display) -> String {
    d.device.clone()
}

fn label(d: &Display) -> String {
    let tag = if cfg!(windows) {
        format!("Display {}", tag(d))
    } else {
        tag(d)
    };
    if d.name.is_empty() {
        tag
    } else {
        format!("{}  ·  {tag}", d.name)
    }
}

/// How much of the desktop a monitor covers. Windows' desktop is laid out
/// in pixels; a Wayland desktop in scaled units, where a monitor at 125 %
/// covers less than its resolution.
fn extent(d: &Display) -> (f32, f32) {
    let scale = if cfg!(windows) {
        1.0
    } else {
        d.scale_pct.max(1) as f32 / 100.0
    };
    (d.width as f32 / scale, d.height as f32 / scale)
}

fn hz(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{v:.0} Hz")
    } else {
        format!("{v:.2} Hz")
    }
}

/// The desktop in miniature: each monitor where it sits, primary marked.
fn layout(ui: &mut Ui, displays: &[Display]) {
    let (min_x, min_y, max_x, max_y) = displays.iter().fold(
        (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
        |(a, b, c, d), m| {
            let (w, h) = extent(m);
            (
                a.min(m.x),
                b.min(m.y),
                c.max(m.x + w as i32),
                d.max(m.y + h as i32),
            )
        },
    );
    let (w, h) = ((max_x - min_x).max(1) as f32, (max_y - min_y).max(1) as f32);
    let width = ui.available_width().min(560.0);
    let scale = (width / w).min(200.0 / h);
    let (outer, _) = ui.allocate_exact_size(vec2(w * scale, h * scale), Sense::hover());
    let painter = ui.painter();
    for d in displays {
        let (w, h) = extent(d);
        let r = Rect::from_min_size(
            pos2(
                outer.left() + (d.x - min_x) as f32 * scale,
                outer.top() + (d.y - min_y) as f32 * scale,
            ),
            vec2(w * scale, h * scale),
        )
        .shrink(2.0);
        let warn = d.below_max() || d.below_native();
        painter.rect_filled(r, 4.0, theme::RAISED);
        painter.rect_stroke(
            r,
            4.0,
            Stroke::new(
                if d.primary { 2.0 } else { 1.0 },
                if d.primary {
                    theme::ACCENT
                } else if warn {
                    theme::ELEVATED_TEXT
                } else {
                    theme::BORDER_STRONG
                },
            ),
            StrokeKind::Inside,
        );
        painter.text(
            r.center() - vec2(0.0, 8.0),
            Align2::CENTER_CENTER,
            tag(d),
            FontId::proportional(theme::TITLE),
            theme::TEXT,
        );
        painter.text(
            r.center() + vec2(0.0, 12.0),
            Align2::CENTER_CENTER,
            format!("{}×{} · {}", d.width, d.height, hz(d.refresh_hz)),
            FontId::proportional(theme::CAPTION),
            theme::TEXT_WEAK,
        );
    }
}

fn fact(ui: &mut Ui, key: &str, value: impl Into<egui::WidgetText>) {
    ui.label(RichText::new(key).color(theme::TEXT_WEAK));
    ui.label(value);
    ui.end_row();
}

fn card(ui: &mut Ui, d: &Display) {
    Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::strong(label(d), theme::SUBTITLE));
                if d.primary {
                    ui.label(RichText::new("main").color(theme::ACCENT));
                }
            });
            ui.add_space(4.0);
            egui::Grid::new(("display", &d.device))
                .num_columns(2)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    fact(ui, "GPU", d.gpu.as_str());
                    fact(ui, "Connection", d.connection);
                    let res = if d.below_native() {
                        RichText::new(format!(
                            "{}×{}  ·  its own is {}×{}: {SCALER} scales the picture up, and \
                             text and edges come out soft",
                            d.width, d.height, d.native.0, d.native.1
                        ))
                        .color(theme::ELEVATED_TEXT)
                    } else {
                        RichText::new(format!("{}×{}", d.width, d.height))
                    };
                    fact(ui, "Resolution", res);
                    let rate = if d.below_max() {
                        RichText::new(format!(
                            "{}  ·  it can do {} Hz at this resolution ({SETTINGS})",
                            hz(d.refresh_hz),
                            d.max_hz_here
                        ))
                        .color(theme::ELEVATED_TEXT)
                    } else {
                        RichText::new(format!(
                            "{}  ·  its fastest at this resolution{}",
                            hz(d.refresh_hz),
                            if d.max_hz > d.max_hz_here {
                                format!(" ({} Hz at a lower one)", d.max_hz)
                            } else {
                                String::new()
                            }
                        ))
                    };
                    fact(ui, "Refresh", rate);
                    fact(
                        ui,
                        "HDR",
                        match (d.hdr_supported, d.hdr_on) {
                            (_, true) => "on".to_string(),
                            (true, false) => "supported, off".to_string(),
                            _ => "not supported".to_string(),
                        },
                    );
                    if d.bits_per_color > 0 {
                        fact(
                            ui,
                            "Colour depth",
                            format!("{} bits per channel", d.bits_per_color),
                        );
                    }
                    fact(ui, "Scaling", format!("{} %", d.scale_pct));
                    fact(ui, "Position", format!("{}, {}  ·  {}", d.x, d.y, d.device));
                });
        });
}

pub fn show(ui: &mut Ui, displays: Option<&[Display]>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Displays").font(theme::brand(theme::TITLE)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(d) = displays {
                ui.label(
                    RichText::new(format!(
                        "{} monitor{}",
                        d.len(),
                        if d.len() == 1 { "" } else { "s" }
                    ))
                    .color(theme::TEXT_WEAK),
                );
            }
        });
    });
    ui.add_space(4.0);
    let Some(displays) = displays else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Reading the monitors…").color(theme::TEXT_WEAK));
        });
        return;
    };
    layout(ui, displays);
    ui.add_space(10.0);
    let gpus: Vec<&str> = {
        let mut g: Vec<&str> = displays.iter().map(|d| d.gpu.as_str()).collect();
        g.sort_unstable();
        g.dedup();
        g
    };
    if gpus.len() > 1 {
        ui.label(
            RichText::new(
                "The monitors hang off different GPUs: a game on one GPU shown on another's \
                 monitor has every frame copied across, which costs time.",
            )
            .color(theme::ELEVATED_TEXT),
        );
        ui.add_space(6.0);
    }
    let width = ui.available_width();
    let columns = if width > 980.0 { 2 } else { 1 };
    ui.columns(columns, |cols| {
        for (i, d) in displays.iter().enumerate() {
            let ui = &mut cols[i % columns];
            card(ui, d);
            ui.add_space(8.0);
        }
    });
}
