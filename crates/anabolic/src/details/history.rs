// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The History tab: a program over the last day or week, whatever its
//! processes and PIDs were: its CPU, memory, disk and network minute by
//! minute, and each day added up. From the flight recorder's program
//! minutes, read on a thread of its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::recorder::{ProgramRecord, local_day, unix_now};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Color32, Label, Pos2, Rect, RichText, Sense, Ui, vec2};

use crate::{fmt, theme, widgets};

const DAY: u64 = 86_400;
/// Read again while the tab stays open: a minute is written each minute.
const AGAIN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Span {
    #[default]
    Day,
    Week,
}

#[derive(Default)]
pub struct HistoryView {
    reading: Option<(String, Receiver<Vec<ProgramRecord>>)>,
    records: Option<(String, Arc<Vec<ProgramRecord>>, Instant)>,
    pub span: Span,
}

impl HistoryView {
    fn start(&mut self, name: &str, ctx: &egui::Context) {
        let Some(dir) = crate::data_dir().map(|d| d.join("history")) else {
            return;
        };
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let wanted = name.to_string();
        let spawned = std::thread::Builder::new()
            .name("program-history".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::recorder::program_history(&dir, &wanted, 8));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.reading = Some((name.to_string(), rx));
        }
    }

    fn poll(&mut self) {
        let Some((name, rx)) = &self.reading else {
            return;
        };
        if let Ok(records) = rx.try_recv() {
            self.records = Some((name.clone(), Arc::new(records), Instant::now()));
            self.reading = None;
        }
    }
}

pub fn tab(ui: &mut Ui, view: &mut HistoryView, name: &str, recording: bool) {
    view.poll();
    let fresh = view
        .records
        .as_ref()
        .filter(|(n, _, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, r, at)| (r.clone(), at.elapsed() < AGAIN));
    let reading = view
        .reading
        .as_ref()
        .is_some_and(|(n, _)| n.eq_ignore_ascii_case(name));
    if !reading && fresh.as_ref().is_none_or(|(_, new)| !new) {
        view.start(name, ui.ctx());
    }
    let Some((records, _)) = fresh else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Reading its history…").color(theme::TEXT_WEAK));
        });
        return;
    };

    let now = unix_now();
    let span = match view.span {
        Span::Day => DAY,
        Span::Week => 7 * DAY,
    };
    let from = now.saturating_sub(span);
    let shown: Vec<&ProgramRecord> = records.iter().filter(|r| r.start >= from).collect();

    ui.horizontal(|ui| {
        ui.label(widgets::strong(name, theme::BODY));
        ui.label(
            RichText::new(match shown.first() {
                Some(first) => format!(
                    "  ·  {} minutes recorded since {}",
                    shown.len(),
                    clock(first.start, view.span == Span::Week)
                ),
                None => String::new(),
            })
            .small()
            .color(theme::TEXT_WEAK),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .selectable_label(view.span == Span::Week, "7 days")
                .clicked()
            {
                view.span = Span::Week;
            }
            if ui
                .selectable_label(view.span == Span::Day, "24 hours")
                .clicked()
            {
                view.span = Span::Day;
            }
        });
    });
    if shown.is_empty() {
        ui.add_space(8.0);
        ui.add(
            Label::new(
                RichText::new(if recording {
                    "Nothing recorded for it in this span yet. Anabolic records each program's \
                     minutes while it runs (from version 0.8 on), and keeps them as long as the \
                     history."
                } else {
                    "The history is off in Settings: nothing is recorded."
                })
                .color(theme::TEXT_WEAK),
            )
            .wrap(),
        );
        return;
    }

    ui.add_space(6.0);
    let cpu = |r: &ProgramRecord| r.cpu_avg as f64;
    let memory = |r: &ProgramRecord| r.private as f64;
    let disk = |r: &ProgramRecord| r.disk.max(r.io) as f64 / r.secs.max(1.0) as f64;
    let net = |r: &ProgramRecord| r.net as f64 / r.secs.max(1.0) as f64;
    chart(ui, &shown, from, now, "CPU", theme::CPU, &cpu, &|v| {
        format!("{v:.1} %")
    });
    chart(
        ui,
        &shown,
        from,
        now,
        "Private memory",
        theme::MEMORY,
        &memory,
        &|v| fmt::bytes(v as u64),
    );
    chart(
        ui,
        &shown,
        from,
        now,
        "Disk and I/O",
        theme::DISK_READ,
        &disk,
        &|v| fmt::rate_always(v),
    );
    chart(
        ui,
        &shown,
        from,
        now,
        "Network",
        theme::NET_RX,
        &net,
        &|v| fmt::rate_always(v),
    );

    ui.add_space(8.0);
    days(ui, &shown);
}

/// "14:32", or "Tue 14:32" over a week.
fn clock(unix: u64, weekday: bool) -> String {
    let filetime = (unix as i64 + 11_644_473_600) * 10_000_000;
    let text = anabolic_core::sysinfo::local_time(filetime).unwrap_or_default();
    if weekday {
        text.chars().skip(5).take(11).collect()
    } else {
        text.chars().skip(11).take(5).collect()
    }
}

/// One measure over the span, in columns of a few pixels: each column's
/// highest minute. Minutes not recorded stay empty.
#[allow(clippy::too_many_arguments)]
fn chart(
    ui: &mut Ui,
    records: &[&ProgramRecord],
    from: u64,
    to: u64,
    title: &str,
    color: Color32,
    value: &dyn Fn(&ProgramRecord) -> f64,
    label: &dyn Fn(f64) -> String,
) {
    let width = ui.available_width();
    let columns = ((width / 3.0) as usize).max(10);
    let per = ((to - from) as f64 / columns as f64).max(1.0);
    let mut peaks: Vec<Option<f64>> = vec![None; columns];
    for r in records {
        let i = (((r.start.saturating_sub(from)) as f64 / per) as usize).min(columns - 1);
        let v = value(r);
        peaks[i] = Some(peaks[i].map_or(v, |p: f64| p.max(v)));
    }
    let top = peaks.iter().flatten().copied().fold(0.0, f64::max);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).small().color(theme::TEXT_WEAK));
        ui.label(
            RichText::new(format!("highest {}", label(top)))
                .small()
                .color(theme::TEXT_DIM),
        );
    });
    let (rect, response) = ui.allocate_exact_size(vec2(width, 54.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::RAISED);
    let max = crate::graph::nice_ceiling(top as f32).max(f32::MIN_POSITIVE) as f64;
    let w = rect.width() / columns as f32;
    for (i, p) in peaks.iter().enumerate() {
        let Some(v) = p else { continue };
        let h = (v / max) as f32 * rect.height();
        let x = rect.left() + i as f32 * w;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x, rect.bottom() - h.max(1.0)),
                Pos2::new(x + w, rect.bottom()),
            ),
            0.0,
            color,
        );
    }
    // Under the pointer: that column's time and value.
    if let Some(pos) = response.hover_pos() {
        let i = (((pos.x - rect.left()) / w) as usize).min(columns - 1);
        let at = from + (i as f64 * per) as u64;
        let text = match peaks[i] {
            Some(v) => format!("{}  ·  {}", clock(at, to - from > DAY), label(v)),
            None => format!("{}  ·  not recorded", clock(at, to - from > DAY)),
        };
        response.on_hover_text_at_pointer(text);
    }
    ui.add_space(4.0);
}

/// Each day added up, newest first.
fn days(ui: &mut Ui, records: &[&ProgramRecord]) {
    let mut by: Vec<(String, f64, u64, u64, u64, u32)> = Vec::new();
    for r in records {
        let day = local_day(r.start).0;
        let cpu_secs = r.cpu_avg as f64 / 100.0 * r.secs as f64;
        match by.iter_mut().find(|d| d.0 == day) {
            Some(d) => {
                d.1 += cpu_secs;
                d.2 = d.2.max(r.private);
                d.3 += r.disk.max(r.io);
                d.4 += r.net;
                d.5 += 1;
            }
            None => by.push((day, cpu_secs, r.private, r.disk.max(r.io), r.net, 1)),
        }
    }
    by.sort_by(|a, b| b.0.cmp(&a.0));
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    egui::Grid::new("program-days")
        .num_columns(6)
        .spacing([18.0, 3.0])
        .show(ui, |ui| {
            for title in [
                "Day",
                "Processor time",
                "Memory at most",
                "Disk and I/O",
                "Network",
                "Minutes",
            ] {
                ui.label(RichText::new(title).small().color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for (day, cpu_secs, private, disk, net, minutes) in by {
                ui.label(day);
                // Seconds of the whole machine, as one core's time.
                ui.label(crate::fmt::span(Duration::from_secs_f64(cpu_secs * cores)))
                    .on_hover_text("Its processor time, counted on one core");
                ui.label(fmt::bytes(private));
                ui.label(fmt::bytes(disk));
                ui.label(fmt::bytes(net));
                ui.label(minutes.to_string());
                ui.end_row();
            }
        });
}
