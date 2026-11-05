// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Startup page's boots: how long each start took, from the firmware
//! to the sign-in, what kind of start it was and how the shutdown before it
//! went. Read once from the system's logs, in the background, and again on
//! request.

use std::sync::Arc;
use std::time::Duration;

use anabolic_core::boot::{Boot, Boots};
use anabolic_core::sysinfo::local_time;
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Align2, Label, Layout, Rect, RichText, Sense, Ui, vec2};

use crate::table::{Columns, Spec};
use crate::{panes, theme, widgets};

/// Boots read from the logs, and the reading under way.
#[derive(Default)]
pub struct BootsView {
    read: Option<Result<Arc<Boots>, String>>,
    reading: Option<Receiver<Result<Boots, String>>>,
    /// The boot shown in the pane, by when it began.
    selected: Option<i64>,
}

impl BootsView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("boots".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::boot::read());
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.reading = Some(rx),
            Err(e) => self.read = Some(Err(format!("could not start reading: {e}"))),
        }
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.reading
            && let Ok(result) = rx.try_recv()
        {
            self.read = Some(result.map(Arc::new));
            self.reading = None;
        }
    }
}

/// The system's part: to the sign-in on Windows, to the graphical target
/// on Linux.
const READY: &str = if cfg!(windows) {
    "Windows to sign-in"
} else {
    "To the desktop"
};

/// Above this, the sign-in screen waited for someone: the bar stops here.
const CHART_CAP_MS: u64 = 120_000;
/// Beyond this the screen surely waited; the time is shown as such.
const WAITED_MS: u64 = 10 * 60_000;

/// "16.2 s", "1 min 12 s", "1 h 9 min".
fn secs(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{} min {} s", ms / 60_000, ms / 1000 % 60)
    } else {
        crate::fmt::span(Duration::from_millis(ms))
    }
}

fn when(b: &Boot) -> String {
    local_time(b.started)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

/// Firmware and system, when both are known.
fn total(b: &Boot) -> Option<u64> {
    Some(b.firmware_ms? + b.ready_ms?)
}

const TITLES: [(&str, bool); 7] = [
    ("Started", false),
    ("Kind", false),
    ("Firmware", true),
    (
        if cfg!(windows) {
            "To sign-in"
        } else {
            "To the desktop"
        },
        true,
    ),
    ("Together", true),
    ("The shutdown before", false),
    ("Slower than usual", false),
];
const COLUMNS: [Spec; 7] = [
    Spec::new(130.0),
    Spec::new(110.0),
    Spec::new(80.0),
    Spec::new(96.0),
    Spec::new(80.0),
    Spec::new(150.0),
    Spec::new(200.0).clip(),
];

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

pub fn show(ui: &mut Ui, view: &mut BootsView) {
    view.poll();
    if view.read.is_none() && view.reading.is_none() {
        view.start(ui.ctx());
    }
    let boots = match &view.read {
        None => {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the system's logs…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("The boots could not be read: {e}")).color(theme::BAD));
            return;
        }
        Some(Ok(b)) => b.clone(),
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} boots  ·  read from the system's logs in {}",
                boots.boots.len(),
                crate::fmt::millis(boots.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        if let Some(note) = &boots.note {
            ui.label(RichText::new(note).small().color(theme::ELEVATED_TEXT));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let reading = view.reading.is_some();
            if ui
                .add_enabled(!reading, egui::Button::new("Read again"))
                .clicked()
            {
                view.start(ui.ctx());
            }
            if reading {
                ui.spinner();
            }
        });
    });
    if boots.boots.is_empty() {
        return;
    }
    ui.add_space(4.0);
    summary(ui, &boots.boots[0]);
    ui.add_space(6.0);
    chart(ui, &boots.boots, &mut view.selected);
    ui.add_space(6.0);

    if let Some(b) = view
        .selected
        .and_then(|at| boots.boots.iter().find(|b| b.started == at))
    {
        panes::side(ui, ("boot-pane", b.started), |ui| pane(ui, b));
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        table(ui, &boots.boots, &mut view.selected);
    });
}

/// The last boot, in a line of figures.
fn summary(ui: &mut Ui, b: &Boot) {
    ui.horizontal(|ui| {
        widgets::stat(
            ui,
            150.0,
            "Last boot",
            RichText::new(when(b)).color(theme::TEXT),
        );
        widgets::stat(
            ui,
            110.0,
            "Firmware",
            widgets::strong(b.firmware_ms.map_or("—".to_string(), secs), theme::SUBTITLE),
        );
        let ready = match b.ready_ms {
            Some(ms) if ms >= WAITED_MS => {
                RichText::new(format!("after {}", secs(ms))).color(theme::TEXT_WEAK)
            }
            Some(ms) => widgets::strong(secs(ms), theme::SUBTITLE),
            None => RichText::new("—").color(theme::TEXT_WEAK),
        };
        widgets::stat(ui, 140.0, READY, ready);
        let before = b.before.label();
        widgets::stat(
            ui,
            170.0,
            "The shutdown before",
            RichText::new(if before.is_empty() {
                "—".into()
            } else {
                before
            })
            .color(if b.before.bad() {
                theme::ELEVATED_TEXT
            } else {
                theme::TEXT
            }),
        );
        if let Some(m) = b.measured {
            widgets::stat(
                ui,
                200.0,
                "Windows' own measure",
                RichText::new(format!(
                    "{} to the desktop, {} after",
                    secs(m.main_path_ms),
                    secs(m.after_ms)
                ))
                .color(theme::TEXT),
            );
        }
    });
}

/// A bar for each boot, oldest on the left: the firmware's part below,
/// the system's to the sign-in above. A dot under a boot whose shutdown
/// before was not clean.
fn chart(ui: &mut Ui, boots: &[Boot], selected: &mut Option<i64>) {
    let height = 110.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme::RADIUS_PANEL, theme::STRIPE);
    let plot = Rect::from_min_max(rect.min + vec2(44.0, 10.0), rect.max - vec2(10.0, 22.0));
    let most = boots
        .iter()
        .map(|b| b.firmware_ms.unwrap_or(0) + b.ready_ms.unwrap_or(0).min(CHART_CAP_MS))
        .max()
        .unwrap_or(1)
        .max(10_000);
    // A line every 10 or 30 s, labelled.
    let step = if most > 90_000 { 30_000 } else { 10_000 };
    let y_of = |ms: u64| plot.bottom() - plot.height() * (ms as f32 / most as f32);
    let mut tick = step;
    while tick <= most {
        let y = y_of(tick);
        p.hline(plot.x_range(), y, egui::Stroke::new(1.0, theme::GRID));
        p.text(
            egui::pos2(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            format!("{} s", tick / 1000),
            egui::FontId::proportional(theme::CAPTION),
            theme::TEXT_DIM,
        );
        tick += step;
    }
    let n = boots.len();
    let slot = plot.width() / n.max(1) as f32;
    let bar = (slot * 0.7).clamp(3.0, 28.0);
    let pointer = ui.ctx().pointer_hover_pos();
    for (i, b) in boots.iter().rev().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        let column =
            Rect::from_center_size(egui::pos2(cx, plot.center().y), vec2(slot, rect.height()));
        let hovered = pointer.is_some_and(|p| column.contains(p) && rect.contains(p));
        let firmware = b.firmware_ms.unwrap_or(0);
        let ready = b.ready_ms.unwrap_or(0);
        let shown = ready.min(CHART_CAP_MS);
        let x = egui::Rangef::new(cx - bar / 2.0, cx + bar / 2.0);
        let is_selected = *selected == Some(b.started);
        if hovered || is_selected {
            p.rect_filled(
                Rect::from_x_y_ranges(column.x_range(), plot.y_range()),
                2.0,
                theme::HOVER,
            );
        }
        p.rect_filled(
            Rect::from_x_y_ranges(x, egui::Rangef::new(y_of(firmware), plot.bottom())),
            1.0,
            theme::KERNEL,
        );
        p.rect_filled(
            Rect::from_x_y_ranges(x, egui::Rangef::new(y_of(firmware + shown), y_of(firmware))),
            1.0,
            theme::CPU,
        );
        // The screen waited: the bar goes on beyond the chart.
        if ready > CHART_CAP_MS {
            p.text(
                egui::pos2(cx, y_of(firmware + shown) - 1.0),
                Align2::CENTER_BOTTOM,
                "…",
                egui::FontId::proportional(theme::CAPTION),
                theme::TEXT_WEAK,
            );
        }
        if b.before.bad() {
            p.circle_filled(
                egui::pos2(cx, plot.bottom() + 8.0),
                3.0,
                theme::ELEVATED_TEXT,
            );
        }
        if hovered {
            let response = ui.interact(
                column,
                ui.id().with(("boot bar", b.started)),
                Sense::click(),
            );
            let response = response.on_hover_ui(|ui| {
                ui.label(widgets::strong(when(b), theme::BODY));
                ui.label(format!(
                    "Firmware {}",
                    b.firmware_ms.map_or("—".into(), secs)
                ));
                ui.label(format!("{READY} {}", b.ready_ms.map_or("—".into(), secs)));
                let before = b.before.label();
                if !before.is_empty() {
                    ui.label(format!("The shutdown before: {before}"));
                }
            });
            if response.clicked() {
                *selected = if is_selected { None } else { Some(b.started) };
            }
        }
    }
    // The legend, under the plot on the right.
    let mut x = rect.right() - 10.0;
    for (label, color) in [
        ("not shut down cleanly before", theme::ELEVATED_TEXT),
        (READY, theme::CPU),
        ("firmware", theme::KERNEL),
    ] {
        let galley = p.layout_no_wrap(
            label.to_string(),
            egui::FontId::proportional(theme::CAPTION),
            theme::TEXT_WEAK,
        );
        x -= galley.size().x;
        let y = rect.bottom() - 9.0;
        p.galley(
            egui::pos2(x, y - galley.size().y / 2.0),
            galley,
            theme::TEXT_WEAK,
        );
        x -= 12.0;
        p.rect_filled(
            Rect::from_center_size(egui::pos2(x + 4.0, y), vec2(8.0, 8.0)),
            2.0,
            color,
        );
        x -= 14.0;
    }
}

fn table(ui: &mut Ui, boots: &[Boot], selected: &mut Option<i64>) {
    let mut pick = None;
    Columns::load(ui.ctx(), "boots", &COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, boots.len(), |mut row| {
                    let b = &boots[row.index()];
                    row.set_selected(*selected == Some(b.started));
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                ui.label(when(b));
                            }
                            1 => {
                                ui.label(RichText::new(b.kind.label()).color(theme::TEXT_WEAK));
                            }
                            2 => right(ui, b.firmware_ms.map_or(String::new(), secs)),
                            3 => match b.ready_ms {
                                Some(ms) if ms >= WAITED_MS => {
                                    right(ui, RichText::new(secs(ms)).color(theme::TEXT_DIM))
                                }
                                Some(ms) => right(ui, secs(ms)),
                                None => {}
                            },
                            4 => {
                                if let Some(t) = total(b).filter(|_| b.ready_ms < Some(WAITED_MS)) {
                                    right(ui, widgets::strong(secs(t), theme::BODY));
                                }
                            }
                            5 => {
                                ui.label(RichText::new(b.before.label()).color(
                                    if b.before.bad() {
                                        theme::ELEVATED_TEXT
                                    } else {
                                        theme::TEXT_WEAK
                                    },
                                ));
                            }
                            _ => {
                                let names: Vec<&str> =
                                    b.slowed.iter().map(|s| s.name.as_str()).collect();
                                ui.add(
                                    Label::new(
                                        RichText::new(names.join(", ")).color(theme::TEXT_WEAK),
                                    )
                                    .truncate(),
                                );
                            }
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(b.started);
                    }
                });
            })
    });
    if let Some(at) = pick {
        *selected = if *selected == Some(at) {
            None
        } else {
            Some(at)
        };
    }
}

/// One boot in full.
fn pane(ui: &mut Ui, b: &Boot) {
    ui.label(widgets::strong(when(b), theme::SUBTITLE));
    ui.label(RichText::new(b.kind.label()).color(theme::TEXT_WEAK));
    panes::section(ui, "Times");
    let fact = |ui: &mut Ui, label: &str, value: String| {
        ui.horizontal(|ui| {
            widgets::text(
                ui,
                130.0,
                Align::Min,
                RichText::new(label).color(theme::TEXT_WEAK),
            );
            ui.label(value);
        });
    };
    fact(ui, "Firmware", b.firmware_ms.map_or("—".into(), secs));
    fact(ui, READY, b.ready_ms.map_or("—".into(), secs));
    if cfg!(windows) {
        ui.label(
            RichText::new(
                "From the kernel's start to the first sign-in; the time the sign-in \
                 screen waited is part of it.",
            )
            .small()
            .color(theme::TEXT_WEAK),
        );
    }
    if let Some(m) = b.measured {
        fact(ui, "Whole boot", secs(m.total_ms));
        fact(ui, "To the desktop", secs(m.main_path_ms));
        fact(ui, "After the desktop", secs(m.after_ms));
    }
    if b.ready_ms.is_some_and(|ms| ms >= WAITED_MS) {
        ui.label(
            RichText::new("The sign-in came long after: the sign-in screen waited for someone.")
                .small()
                .color(theme::TEXT_WEAK),
        );
    }
    panes::section(ui, "The shutdown before");
    let before = b.before.label();
    ui.label(if before.is_empty() {
        "Not known".into()
    } else {
        before
    });
    ui.label(
        RichText::new(match b.before {
            anabolic_core::boot::Shutdown::Crashed(_) => {
                "Windows stopped with an error (a blue screen) and restarted."
            }
            anabolic_core::boot::Shutdown::PowerButton => {
                "Someone held the power button: the system was not shut down."
            }
            anabolic_core::boot::Shutdown::Unexpected => {
                "It stopped without shutting down: the power went, it stopped responding, \
                 or it was reset."
            }
            _ => "",
        })
        .small()
        .color(theme::TEXT_WEAK),
    );
    if !b.slowed.is_empty() {
        panes::section(ui, "Slower than usual");
        for s in &b.slowed {
            ui.horizontal(|ui| {
                widgets::text(
                    ui,
                    70.0,
                    Align::Min,
                    RichText::new(s.kind.label()).color(theme::TEXT_WEAK),
                );
                ui.add(Label::new(&s.name).truncate());
            });
            let more = if s.over_ms > 0 {
                format!("{}, {} more than usual", secs(s.took_ms), secs(s.over_ms))
            } else {
                secs(s.took_ms)
            };
            ui.label(RichText::new(more).small().color(theme::TEXT_WEAK));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_times_in_words() {
        assert_eq!(secs(16_225), "16.2 s");
        assert_eq!(secs(72_000), "1 min 12 s");
        assert_eq!(secs(4_177_722), "1 h 9 min");
    }
}
