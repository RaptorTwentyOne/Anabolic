// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Profile tab: where the process spends its processor time, sampled
//! for a few seconds by the system's profiler. By function (its own time
//! and with what it calls), by module, by thread, and as a call tree.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anabolic_core::ProcKey;
use anabolic_core::profile::{Frame, Profile};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use crate::table::{Columns, Spec};
use crate::{theme, widgets};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Show {
    #[default]
    Functions,
    Modules,
    Threads,
    Tree,
}

struct Running {
    key: ProcKey,
    started: Instant,
    seconds: u32,
    stop: Arc<AtomicBool>,
    rx: Receiver<Result<Profile, String>>,
}

#[derive(Default)]
pub struct ProfileView {
    seconds: u32,
    running: Option<Running>,
    result: Option<(ProcKey, Result<Arc<Profile>, String>)>,
    show: Show,
    /// Open nodes of the call tree.
    open: HashSet<u32>,
}

const LENGTHS: [u32; 4] = [2, 5, 10, 30];

impl ProfileView {
    fn start(&mut self, key: ProcKey, ctx: &egui::Context) {
        let seconds = self.length();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = bounded(1);
        let (flag, ctx) = (stop.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("profile".into())
            .spawn(move || {
                let result = anabolic_core::profile::profile(key.pid, seconds as f32, &|| {
                    flag.load(Ordering::Relaxed)
                });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.running = Some(Running {
                    key,
                    started: Instant::now(),
                    seconds,
                    stop,
                    rx,
                });
            }
            Err(e) => self.result = Some((key, Err(format!("could not start: {e}")))),
        }
    }

    fn length(&self) -> u32 {
        if self.seconds == 0 { 5 } else { self.seconds }
    }

    fn poll(&mut self) {
        let Some(r) = &self.running else { return };
        if let Ok(result) = r.rx.try_recv() {
            let key = r.key;
            self.running = None;
            self.open.clear();
            if let Ok(p) = &result {
                self.open_hot_path(p);
            }
            self.result = Some((key, result.map(Arc::new)));
        }
    }

    /// Opens the tree along its largest calls while they hold most of it.
    fn open_hot_path(&mut self, p: &Profile) {
        let mut at = 0usize;
        while let Some(&c) = p.tree[at].children.first() {
            self.open.insert(at as u32);
            if p.tree[c as usize].total * 4 < p.samples {
                break;
            }
            at = c as usize;
        }
    }
}

fn frame_text(f: &Frame, hovered: bool) -> RichText {
    let text = RichText::new(f.label());
    if hovered {
        text.color(theme::ACCENT)
    } else if f.kernel || f.function.is_empty() {
        text.color(theme::TEXT_WEAK)
    } else {
        text.color(theme::TEXT)
    }
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

/// A share as a bar under its percentage.
fn share(ui: &mut Ui, pct: f32, color: egui::Color32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, widgets::ROW), Sense::hover());
    let bar = egui::Rect::from_min_size(
        rect.left_center() - egui::vec2(0.0, 4.0),
        egui::vec2((width - 46.0).max(8.0) * (pct / 100.0).clamp(0.0, 1.0), 8.0),
    );
    ui.painter().rect_filled(bar, 2.0, color);
    ui.painter().text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        format!("{pct:.1} %"),
        egui::FontId::proportional(theme::META),
        theme::TEXT,
    );
}

pub fn tab(ui: &mut Ui, view: &mut ProfileView, key: ProcKey, alive: bool) {
    view.poll();
    ui.horizontal(|ui| match &view.running {
        Some(r) if r.key == key => {
            ui.spinner();
            let left =
                Duration::from_secs(u64::from(r.seconds)).saturating_sub(r.started.elapsed());
            ui.label(if left.is_zero() {
                "Naming what was sampled…".to_string()
            } else {
                format!("Sampling: {} s left", left.as_secs() + 1)
            });
            if ui.button("Stop").clicked() {
                r.stop.store(true, Ordering::Relaxed);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        Some(_) => {
            ui.label(RichText::new("Another process is being sampled.").color(theme::TEXT_WEAK));
        }
        None => {
            ui.label("Sample for");
            let mut seconds = view.length();
            egui::ComboBox::from_id_salt("profile-seconds")
                .selected_text(format!("{seconds} s"))
                .width(64.0)
                .show_ui(ui, |ui| {
                    for s in LENGTHS {
                        ui.selectable_value(&mut seconds, s, format!("{s} s"));
                    }
                });
            view.seconds = seconds;
            if ui
                .add_enabled(
                    alive,
                    egui::Button::new(widgets::strong("Start", theme::BODY)),
                )
                .clicked()
            {
                view.start(key, ui.ctx());
            }
            ui.label(
                RichText::new(
                    "The system's own profiler samples it a thousand times a second; the \
                         process is not opened. Names come from the symbols on this PC, or the \
                         exports.",
                )
                .small()
                .color(theme::TEXT_DIM),
            );
        }
    });
    let Some((for_key, result)) = &view.result else {
        return;
    };
    if *for_key != key {
        return;
    }
    let p = match result {
        Err(e) => {
            ui.label(RichText::new(e).color(theme::BAD));
            return;
        }
        Ok(p) => p.clone(),
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} samples in {:.1} s  ·  {} threads",
                p.samples,
                p.took.as_secs_f32(),
                p.threads.len()
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        for note in &p.notes {
            ui.label(RichText::new(note).small().color(theme::ELEVATED_TEXT));
        }
    });
    ui.horizontal(|ui| {
        for (s, label) in [
            (Show::Functions, "Functions"),
            (Show::Tree, "Call tree"),
            (Show::Modules, "Modules"),
            (Show::Threads, "Threads"),
        ] {
            ui.selectable_value(&mut view.show, s, label);
        }
    });
    if p.samples == 0 {
        return;
    }
    match view.show {
        Show::Functions => functions(ui, &p),
        Show::Modules => modules(ui, &p),
        Show::Threads => threads(ui, &p),
        Show::Tree => tree(ui, &p, &mut view.open),
    }
}

const FUNCTION_COLUMNS: [Spec; 3] = [Spec::new(150.0), Spec::new(150.0), Spec::new(600.0).clip()];

fn functions(ui: &mut Ui, p: &Profile) {
    Columns::load(ui.ctx(), "profile-functions", &FUNCTION_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = [
                        ("Own time", true),
                        ("With its calls", true),
                        ("Function", false),
                    ][c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, p.functions.len(), |mut row| {
                    let h = p.functions[row.index()];
                    let f = &p.frames[h.frame as usize];
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => share(ui, p.share(h.own), theme::CPU),
                            1 => share(ui, p.share(h.total), theme::KERNEL),
                            _ => {
                                ui.add(Label::new(frame_text(f, false)).truncate());
                            }
                        });
                    }
                });
            })
    });
}

fn modules(ui: &mut Ui, p: &Profile) {
    Columns::load(ui.ctx(), "profile-modules", &FUNCTION_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = [
                        ("Own time", true),
                        ("With its calls", true),
                        ("Module", false),
                    ][c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, p.modules.len(), |mut row| {
                    let m = &p.modules[row.index()];
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => share(ui, p.share(m.own), theme::CPU),
                            1 => share(ui, p.share(m.total), theme::KERNEL),
                            _ => {
                                ui.add(Label::new(m.module.as_ref()).truncate());
                            }
                        });
                    }
                });
            })
    });
}

fn threads(ui: &mut Ui, p: &Profile) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let columns = [
                ("Thread", 90.0, Align::Max),
                ("Share of the samples", 260.0, Align::Min),
            ];
            widgets::header(ui, &columns);
            for (i, &(tid, n)) in p.threads.iter().enumerate() {
                widgets::row(ui, i, |ui| {
                    widgets::text(ui, columns[0].1, Align::Max, tid.to_string());
                    widgets::cell(ui, columns[1].1, Align::Min, |ui| {
                        share(ui, p.share(n), theme::CPU)
                    });
                });
            }
        });
}

/// The call tree, from the outermost calls in; the rows of what is open.
fn tree(ui: &mut Ui, p: &Profile, open: &mut HashSet<u32>) {
    let mut rows: Vec<(u32, u16)> = Vec::new();
    let mut stack: Vec<(u32, u16)> = p.tree[0].children.iter().rev().map(|&c| (c, 0)).collect();
    while let Some((n, depth)) = stack.pop() {
        rows.push((n, depth));
        if open.contains(&n) {
            stack.extend(
                p.tree[n as usize]
                    .children
                    .iter()
                    .rev()
                    .map(|&c| (c, depth + 1)),
            );
        }
    }
    let mut toggle = None;
    Columns::load(ui.ctx(), "profile-tree", &FUNCTION_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = [
                        ("With its calls", true),
                        ("Own time", true),
                        ("Call", false),
                    ][c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, rows.len(), |mut row| {
                    let (n, depth) = rows[row.index()];
                    let node = &p.tree[n as usize];
                    let f = &p.frames[node.frame as usize];
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => share(ui, p.share(node.total), theme::KERNEL),
                            1 => {
                                if node.own > 0 {
                                    right(ui, format!("{:.1} %", p.share(node.own)));
                                }
                            }
                            _ => {
                                ui.add_space(f32::from(depth.min(60)) * 12.0);
                                if node.children.is_empty() {
                                    ui.add_space(12.0);
                                } else {
                                    let arrow = if open.contains(&n) { "⏷" } else { "⏵" };
                                    ui.add(
                                        Label::new(RichText::new(arrow).color(theme::TEXT_WEAK))
                                            .selectable(false),
                                    );
                                }
                                ui.add(
                                    Label::new(frame_text(f, false))
                                        .truncate()
                                        .selectable(false),
                                );
                            }
                        });
                    }
                    if row.response().clicked() && !node.children.is_empty() {
                        toggle = Some(n);
                    }
                });
            })
    });
    if let Some(n) = toggle
        && !open.remove(&n)
    {
        open.insert(n);
    }
}
