// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Clean up tab: what the scan found that can go, each kind with what
//! it is, how safe deleting it is, where it lies, and a button, or the
//! system's own tool to use instead.

use anabolic_core::space::clean::{Level, Suggestion};
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};

use super::Data;
use crate::{fmt, theme, widgets};

fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::Safe => theme::GOOD,
        Level::Review => theme::ELEVATED_TEXT,
        Level::System => theme::TEXT_DIM,
    }
}

fn button_text(s: &Suggestion) -> String {
    let trash = if cfg!(windows) {
        "the Recycle Bin"
    } else {
        "the trash"
    };
    if s.bin {
        format!("Empty {trash}…")
    } else if s.level == Level::Review {
        format!("Move to {trash}…")
    } else if s.contents {
        "Delete what is inside…".into()
    } else {
        "Delete…".into()
    }
}

pub fn show(ui: &mut Ui, data: &mut Data, open: &mut std::collections::HashSet<usize>) {
    if data.clean.is_empty() {
        ui.add_space(8.0);
        ui.label(
            RichText::new("Nothing the clean-up knows about takes room here.")
                .color(theme::TEXT_WEAK),
        );
        return;
    }
    let total = |level| -> u64 {
        data.clean
            .iter()
            .filter(|s| s.level == level)
            .map(|s| s.size)
            .sum()
    };
    ui.label(
        RichText::new(format!(
            "Safe to delete: {}  ·  look first: {}  ·  the system's own: {}",
            fmt::bytes(total(Level::Safe)),
            fmt::bytes(total(Level::Review)),
            fmt::bytes(total(Level::System)),
        ))
        .color(theme::TEXT_WEAK),
    );
    ui.add_space(4.0);
    let mut ask = None;
    let mut select = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, s) in data.clean.iter().enumerate() {
                egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(4)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(widgets::strong(s.title, theme::BODY));
                            ui.label(
                                RichText::new(s.level.label())
                                    .small()
                                    .color(level_color(s.level)),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if s.deletable() && ui.button(button_text(s)).clicked() {
                                    ask = Some(i);
                                }
                                ui.label(widgets::strong(fmt::bytes(s.size), theme::BODY));
                            });
                        });
                        ui.label(RichText::new(s.about).color(theme::TEXT_WEAK));
                        if let Some(how) = s.how {
                            ui.label(RichText::new(how).small().color(theme::TEXT));
                        }
                        // Where it is: the largest few, all when opened.
                        let shown = if open.contains(&i) { s.nodes.len() } else { 3 };
                        for &n in s.nodes.iter().take(shown) {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(fmt::bytes(data.tree.size(n)))
                                        .small()
                                        .color(theme::TEXT_WEAK),
                                );
                                let path = data.path(n);
                                if ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(path).small().color(theme::TEXT_DIM),
                                        )
                                        .truncate()
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_text("Show it in the list and the map")
                                    .clicked()
                                {
                                    select = Some(n);
                                }
                            });
                        }
                        if s.nodes.len() > 3 {
                            let text = if open.contains(&i) {
                                "Fewer".to_string()
                            } else {
                                format!("All {} places", fmt::count(s.nodes.len() as u64))
                            };
                            if widgets::link(ui, &text).clicked() && !open.remove(&i) {
                                open.insert(i);
                            }
                        }
                    });
                ui.add_space(6.0);
            }
        });
    if let Some(i) = ask {
        data.ask_clean = Some(i);
    }
    if let Some(n) = select {
        data.selected = Some(n);
        data.reveal = Some(n);
    }
}
