// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Network > Wi-Fi: the network this PC is on and how it fares, how
//! crowded each channel is, and every access point heard, in the manner
//! of WifiInfoView and inSSIDer. Read again every few seconds while shown.

use std::time::{Duration, Instant};

use anabolic_core::wifi::{AccessPoint, Band, Wifi, advice, crowding};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Label, RichText, Sense, Ui, vec2};

use crate::proc_table::UiCmd;
use crate::{theme, widgets};

const EVERY: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct WifiView {
    read: Option<Result<Wifi, String>>,
    reading: Option<Receiver<Result<Wifi, String>>>,
    read_at: Option<Instant>,
}

impl WifiView {
    fn tick(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.reading
            && let Ok(r) = rx.try_recv()
        {
            self.read = Some(r);
            self.reading = None;
        }
        if self.reading.is_none() && self.read_at.is_none_or(|t| t.elapsed() >= EVERY) {
            let (tx, rx) = bounded(1);
            let ctx = ctx.clone();
            if std::thread::Builder::new()
                .name("wifi".into())
                .spawn(move || {
                    let _ = tx.send(anabolic_core::wifi::read());
                    ctx.request_repaint();
                })
                .is_ok()
            {
                self.reading = Some(rx);
                self.read_at = Some(Instant::now());
            }
        }
        ctx.request_repaint_after(EVERY);
    }
}

fn signal_color(dbm: i32) -> egui::Color32 {
    match dbm {
        d if d >= -60 => theme::GOOD,
        d if d >= -72 => theme::ELEVATED_TEXT,
        _ => theme::BAD,
    }
}

/// One band's channels as bars: taller is more crowded; the one in use
/// in the accent colour.
fn chart(ui: &mut Ui, w: &Wifi, band: Band) {
    let channels = band.good_channels();
    let skip = w.link.as_ref().map_or("", |l| l.bssid.as_str());
    let mine = w
        .link
        .as_ref()
        .filter(|l| l.band == Some(band))
        .map(|l| l.channel);
    let values: Vec<(u32, f32)> = channels
        .iter()
        .map(|&c| (c, crowding(&w.points, band, c, skip)))
        .collect();
    let top = values.iter().map(|v| v.1).fold(1.0f32, f32::max);
    ui.vertical(|ui| {
        ui.label(RichText::new(band.label()).color(theme::TEXT_WEAK));
        let bar = 26.0;
        let (rect, _) = ui.allocate_exact_size(
            vec2(values.len() as f32 * (bar + 6.0), 80.0),
            Sense::hover(),
        );
        let painter = ui.painter();
        for (i, (c, v)) in values.iter().enumerate() {
            let x = rect.left() + i as f32 * (bar + 6.0);
            let h = (v / top) * (rect.height() - 16.0);
            let color = if Some(*c) == mine {
                theme::ACCENT
            } else {
                theme::NET_RX
            };
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, rect.bottom() - 16.0 - h.max(2.0)),
                    egui::pos2(x + bar, rect.bottom() - 16.0),
                ),
                2.0,
                color,
            );
            painter.text(
                egui::pos2(x + bar / 2.0, rect.bottom() - 7.0),
                egui::Align2::CENTER_CENTER,
                c.to_string(),
                egui::FontId::proportional(theme::CAPTION),
                theme::TEXT_WEAK,
            );
        }
    });
}

pub fn show(ui: &mut Ui, view: &mut WifiView, out: &mut Vec<UiCmd>) {
    view.tick(ui.ctx());
    let w = match &view.read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Listening…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(theme::TEXT_WEAK));
            return;
        }
        Some(Ok(w)) => w.clone(),
    };
    ui.label(RichText::new(&w.adapter).color(theme::TEXT_WEAK));
    if let Some(note) = &w.note {
        ui.add(Label::new(RichText::new(note).color(theme::ELEVATED_TEXT)).wrap());
        if note.contains("location")
            && cfg!(windows)
            && ui.button("Open the location settings").clicked()
        {
            out.push(UiCmd::OpenUrl("ms-settings:privacy-location".into()));
        }
    }
    if let Some(l) = &w.link {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(widgets::strong(&l.ssid, theme::SUBTITLE));
            ui.label(
                RichText::new(format!(
                    "{}  ·  channel {}  ·  {} %  ·  {} Mbps down, {} up{}",
                    l.band.map_or("", |b| b.label()),
                    l.channel,
                    l.quality,
                    l.rx_mbps,
                    l.tx_mbps,
                    if l.generation.is_empty() {
                        String::new()
                    } else {
                        format!("  ·  {}", l.generation)
                    }
                ))
                .color(theme::TEXT_WEAK),
            );
        });
        if let Some(a) = advice(&w) {
            ui.add(Label::new(RichText::new(a).color(theme::TEXT)).wrap());
        }
    } else if w.note.is_none() {
        ui.label(RichText::new("Not connected to a Wi-Fi network.").color(theme::TEXT_WEAK));
    }
    if w.points.is_empty() {
        return;
    }
    ui.add_space(8.0);
    widgets::section(ui, "How crowded each channel is");
    ui.horizontal_top(|ui| {
        for band in [Band::Ghz24, Band::Ghz5, Band::Ghz6] {
            if w.points.iter().any(|p| p.band == Some(band)) {
                chart(ui, &w, band);
                ui.add_space(24.0);
            }
        }
    });
    ui.add_space(8.0);
    widgets::section(ui, &format!("Access points heard ({})", w.points.len()));
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("wifi-points")
                .num_columns(7)
                .spacing([16.0, 3.0])
                .striped(true)
                .show(ui, |ui| {
                    for title in [
                        "Network", "Band", "Channel", "Signal", "Kind", "Security", "Address",
                    ] {
                        ui.label(RichText::new(title).strong().color(theme::TEXT_WEAK));
                    }
                    ui.end_row();
                    for p in &w.points {
                        row(ui, p);
                    }
                });
        });
}

fn row(ui: &mut Ui, p: &AccessPoint) {
    let name = if p.ssid.is_empty() {
        RichText::new("(hidden)").color(theme::TEXT_DIM)
    } else if p.connected {
        RichText::new(&p.ssid).color(theme::ACCENT)
    } else {
        RichText::new(&p.ssid)
    };
    ui.label(name);
    ui.label(RichText::new(p.band.map_or("", |b| b.label())).color(theme::TEXT_WEAK));
    ui.label(p.channel.to_string());
    ui.label(
        RichText::new(format!("{} dBm  ·  {} %", p.dbm, p.quality)).color(signal_color(p.dbm)),
    );
    ui.label(RichText::new(&p.generation).color(theme::TEXT_WEAK));
    ui.label(
        RichText::new(&p.security).color(if p.security == "Open" || p.security == "WEP" {
            theme::ELEVATED_TEXT
        } else {
            theme::TEXT_WEAK
        }),
    );
    ui.label(RichText::new(&p.bssid).color(theme::TEXT_DIM));
    ui.end_row();
}
