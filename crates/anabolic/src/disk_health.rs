// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The drives' health, read in the background when Anabolic starts and
//! every half hour after (a drive's wear and errors change slowly), or
//! when asked. Shown on the Performance page's disks and, when a drive is
//! failing, on the Overview.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::health::{Health, Verdict};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, RichText, Ui};

use crate::{fmt, theme, widgets};

const EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub struct DiskHealth {
    read: Option<Arc<Vec<Health>>>,
    at: Option<Instant>,
    reading: Option<Receiver<Vec<Health>>>,
}

impl DiskHealth {
    /// Takes in a finished read and starts the next when it is due.
    pub fn tick(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.reading
            && let Ok(all) = rx.try_recv()
        {
            self.read = Some(Arc::new(all));
            self.reading = None;
        }
        if self.reading.is_none() && self.at.is_none_or(|t| t.elapsed() >= EVERY) {
            self.start(ctx);
        }
    }

    pub fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("disk-health".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::health::read());
                ctx.request_repaint();
            });
        self.at = Some(Instant::now());
        if spawned.is_ok() {
            self.reading = Some(rx);
        }
    }

    pub fn reading(&self) -> bool {
        self.reading.is_some()
    }

    pub fn all(&self) -> &[Health] {
        self.read.as_deref().map_or(&[], Vec::as_slice)
    }

    pub fn of(&self, number: u32) -> Option<&Health> {
        self.all().iter().find(|h| h.number == number)
    }

    /// When the last read began.
    pub fn when(&self) -> Option<Instant> {
        self.at
    }
}

pub fn color(v: Verdict) -> egui::Color32 {
    match v {
        Verdict::Good => theme::GOOD,
        Verdict::Watch => theme::ELEVATED_TEXT,
        Verdict::Bad => theme::BAD,
        Verdict::Unknown => theme::TEXT_DIM,
    }
}

/// The verdict and the wear in a few words: "Good · 4 % worn".
pub fn short(h: &Health) -> String {
    match h.wear_used {
        Some(w) if h.verdict != Verdict::Unknown => format!("{}  ·  {w} % worn", h.verdict.label()),
        _ => h.verdict.label().to_string(),
    }
}

/// A disk's health in full, for its Performance view.
pub fn section(ui: &mut Ui, health: &mut DiskHealth, number: u32) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Health").color(theme::TEXT_WEAK));
        if let Some(h) = health.of(number) {
            ui.label(widgets::strong(h.verdict.label(), theme::BODY).color(color(h.verdict)));
        }
        if health.reading() {
            ui.spinner();
        } else if ui
            .small_button("Read again")
            .on_hover_text("Ask the drives again; one that sleeps is not woken")
            .clicked()
        {
            health.start(ui.ctx());
        }
        if let Some(at) = health.when().filter(|_| !health.reading()) {
            ui.label(
                RichText::new(format!("read {} ago", fmt::span(at.elapsed())))
                    .small()
                    .color(theme::TEXT_DIM),
            );
        }
    });
    let Some(h) = health.of(number) else {
        if !health.reading() {
            ui.label(
                RichText::new("Not read yet.")
                    .small()
                    .color(theme::TEXT_WEAK),
            );
        }
        return;
    };
    if let Some(note) = &h.note {
        ui.label(RichText::new(note).small().color(theme::TEXT_WEAK));
    }
    for r in &h.reasons {
        ui.label(RichText::new(r).color(color(h.verdict)));
    }
    let hours = |n: u64| {
        if n >= 24 * 365 {
            format!(
                "{} h ({:.1} years)",
                fmt::count(n),
                n as f64 / (24.0 * 365.0)
            )
        } else {
            format!("{} h", fmt::count(n))
        }
    };
    let facts: Vec<(&str, String)> = [
        ("Wear used", h.wear_used.map(|w| format!("{w} %"))),
        ("Temperature", h.temperature.map(|t| format!("{t} °C"))),
        ("Powered on", h.power_on_hours.map(hours)),
        ("Power cycles", h.power_cycles.map(fmt::count)),
        (
            if h.protocol == "NVMe" {
                "Unsafe shutdowns"
            } else {
                "Power losses"
            },
            h.unsafe_shutdowns.map(fmt::count),
        ),
        ("Written", h.written.map(fmt::bytes)),
        ("Read", h.read.map(fmt::bytes)),
        (
            "Spare left",
            h.spare
                .map(|(left, warn)| format!("{left} % (warns under {warn} %)")),
        ),
        ("Errors not corrected", h.media_errors.map(fmt::count)),
        ("Sectors moved aside", h.reallocated.map(fmt::count)),
        ("Sectors waiting", h.pending.map(fmt::count)),
        ("Unreadable sectors", h.uncorrectable.map(fmt::count)),
    ]
    .into_iter()
    .filter_map(|(k, v)| Some((k, v?)))
    .collect();
    egui::Grid::new(("disk-health", number))
        .num_columns(4)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for (i, (k, v)) in facts.iter().enumerate() {
                widgets::text(
                    ui,
                    150.0,
                    Align::Min,
                    RichText::new(*k).color(theme::TEXT_WEAK),
                );
                widgets::text(ui, 170.0, Align::Min, v.clone());
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    if !h.attributes.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(format!("SMART attributes ({})", h.attributes.len()))
                .small()
                .color(theme::TEXT_WEAK),
        )
        .id_salt(("smart", number))
        .show(ui, |ui| {
            let columns = [
                ("ID", 40.0, Align::Max),
                ("Attribute", 200.0, Align::Min),
                ("Value", 60.0, Align::Max),
                ("Worst", 60.0, Align::Max),
                ("Limit", 60.0, Align::Max),
                ("Raw", 140.0, Align::Max),
            ];
            widgets::header(ui, &columns);
            for (i, a) in h.attributes.iter().enumerate() {
                let past = a.threshold > 0 && a.current > 0 && a.current <= a.threshold;
                widgets::row(ui, i, |ui| {
                    widgets::text(ui, columns[0].1, Align::Max, a.id.to_string());
                    widgets::text(ui, columns[1].1, Align::Min, a.name());
                    widgets::text(
                        ui,
                        columns[2].1,
                        Align::Max,
                        RichText::new(a.current.to_string()).color(if past {
                            theme::BAD
                        } else {
                            theme::TEXT
                        }),
                    );
                    widgets::text(ui, columns[3].1, Align::Max, a.worst.to_string());
                    widgets::text(
                        ui,
                        columns[4].1,
                        Align::Max,
                        if a.threshold > 0 {
                            a.threshold.to_string()
                        } else {
                            String::new()
                        },
                    );
                    widgets::text(ui, columns[5].1, Align::Max, a.raw.to_string());
                });
            }
        });
    }
}
