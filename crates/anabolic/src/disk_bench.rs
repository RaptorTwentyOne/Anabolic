// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A physical disk's speed test, in the manner of CrystalDiskMark: on one
//! of its volumes, a test file of the size picked, the four tests read
//! and written, and the results in a table. Started by hand only; the
//! file is gone when the test ends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anabolic_core::disk::DiskStats;
use anabolic_core::diskbench::{self, Progress, Speed};
use eframe::egui::{self, Label, RichText, Ui};

use crate::{theme, widgets};

const MB: u64 = 1 << 20;
const SIZES: [u64; 3] = [256 * MB, 1024 * MB, 4096 * MB];
/// Each test, each way.
const SECONDS: f32 = 5.0;

pub struct BenchView {
    /// The disk the last test ran on, and its progress.
    running: Option<(u32, Arc<Mutex<Progress>>, Arc<AtomicBool>)>,
    pub size: u64,
    /// The volume picked, per disk.
    volume: Option<String>,
}

impl Default for BenchView {
    fn default() -> Self {
        BenchView {
            running: None,
            size: 1024 * MB,
            volume: None,
        }
    }
}

fn speed(s: Option<Speed>, random: bool) -> RichText {
    match s {
        Some(s) if random => RichText::new(format!(
            "{:.0} MB/s  ·  {:.0} IOPS",
            s.bytes_per_sec / MB as f64,
            s.iops
        )),
        Some(s) => RichText::new(format!("{:.0} MB/s", s.bytes_per_sec / MB as f64)),
        None => RichText::new("—").color(theme::TEXT_DIM),
    }
}

pub fn section(ui: &mut Ui, view: &mut BenchView, d: &DiskStats) {
    ui.add_space(14.0);
    let volumes: Vec<&anabolic_core::disk::Volume> =
        d.volumes.iter().filter(|v| v.total > 0).collect();
    ui.horizontal(|ui| {
        widgets::section(ui, "Speed test");
        if volumes.is_empty() {
            return;
        }
        let mine = view.running.as_ref().filter(|(n, _, _)| *n == d.number);
        let busy = mine.is_some_and(|(_, p, _)| !p.lock().is_ok_and(|p| p.done));
        let any_busy = view
            .running
            .as_ref()
            .is_some_and(|(_, p, _)| !p.lock().is_ok_and(|p| p.done));
        if view
            .volume
            .as_ref()
            .is_none_or(|v| !volumes.iter().any(|x| &x.letter == v))
        {
            view.volume = Some(volumes[0].letter.clone());
        }
        let picked = view.volume.clone().unwrap_or_default();
        if volumes.len() > 1 {
            egui::ComboBox::from_id_salt(("bench-volume", d.number))
                .selected_text(&picked)
                .show_ui(ui, |ui| {
                    for v in &volumes {
                        ui.selectable_value(&mut view.volume, Some(v.letter.clone()), &v.letter);
                    }
                });
        }
        egui::ComboBox::from_id_salt(("bench-size", d.number))
            .selected_text(crate::fmt::bytes(view.size))
            .show_ui(ui, |ui| {
                for s in SIZES {
                    ui.selectable_value(&mut view.size, s, crate::fmt::bytes(s));
                }
            });
        let free = volumes
            .iter()
            .find(|v| v.letter == picked)
            .map_or(0, |v| v.free);
        let room = free > view.size + view.size / 4;
        if busy {
            if ui.button("Stop").clicked()
                && let Some((_, _, stop)) = &view.running
            {
                stop.store(true, Ordering::Relaxed);
            }
            ui.spinner();
        } else if ui
            .add_enabled(
                room && !any_busy,
                egui::Button::new(format!("Test {picked}")),
            )
            .on_disabled_hover_text(if any_busy {
                "Another disk's test is running."
            } else {
                "Not enough free space for the test file."
            })
            .clicked()
        {
            let path = diskbench::place(&picked);
            let (progress, stop) = diskbench::start(path, view.size, SECONDS);
            view.running = Some((d.number, progress, stop));
        }
    });
    ui.add(
        Label::new(
            RichText::new(format!(
                "As CrystalDiskMark does: a {} test file, read and written past the system's \
                 cache, {SECONDS:.0} seconds each way per test, about a minute in all. The file \
                 is deleted at the end. What else uses the disk meanwhile lowers the results, \
                 and is slowed by the test.",
                crate::fmt::bytes(view.size)
            ))
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    let Some((_, progress, _)) = view.running.as_ref().filter(|(n, _, _)| *n == d.number) else {
        return;
    };
    let p = progress.lock().map(|p| p.clone()).unwrap_or_default();
    if !p.done {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
        ui.add(
            egui::ProgressBar::new(p.fraction)
                .text(p.stage.clone())
                .desired_width(420.0),
        );
    }
    if let Some(e) = &p.error {
        ui.label(RichText::new(format!("The test stopped: {e}")).color(theme::BAD));
    }
    if p.rows.is_empty() {
        return;
    }
    ui.add_space(4.0);
    egui::Grid::new(("bench", d.number))
        .num_columns(4)
        .spacing([20.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for title in ["Test", "", "Read", "Write"] {
                ui.label(RichText::new(title).strong().color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for r in &p.rows {
                let random = matches!(r.test, diskbench::Test::Rnd32 | diskbench::Test::Rnd1);
                ui.label(widgets::strong(r.test.label(), theme::BODY));
                ui.label(RichText::new(r.test.words()).color(theme::TEXT_WEAK));
                ui.label(speed(r.read, random).color(theme::DISK_READ));
                ui.label(speed(r.write, random).color(theme::DISK_WRITE));
                ui.end_row();
            }
        });
}
