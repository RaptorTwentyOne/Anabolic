// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Duplicates tab: files with the same contents, the most room wasted
//! first. The search reads files, which takes a while on a large drive: it
//! runs in the background at the lowest priority, says how far it is, and
//! can be stopped.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::space::dupes::{self, Group, Progress};
use anabolic_core::space::{NONE, Tree};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use super::Data;
use super::lists::node_menu;
use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{fmt, theme, widgets};

const TITLES: [(&str, bool); 4] = [
    ("Name", false),
    ("Size", true),
    ("Modified", false),
    ("Folder", false),
];
const COLUMNS: [Spec; 4] = [
    Spec::new(300.0).clip(),
    Spec::new(110.0),
    Spec::new(130.0),
    Spec::new(400.0).clip(),
];

struct Running {
    progress: Arc<Progress>,
    rx: Receiver<Result<Vec<Group>, String>>,
    started: Instant,
}

#[derive(Default)]
pub struct Dupes {
    running: Option<Running>,
    error: Option<String>,
    /// The rows shown: (group, node), NONE for a group's own line.
    rows: Vec<(u32, u32)>,
    /// The tree's version and the result the rows were made from.
    made_for: Option<(u64, usize)>,
}

impl Dupes {
    fn start(&mut self, ctx: &egui::Context, tree: Arc<Tree>, min: u64) {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = bounded(1);
        let (p, ctx) = (progress.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("space-dupes".into())
            .spawn(move || {
                // The tree is let go of before the long reading starts, so
                // a deletion meanwhile need not wait for it.
                let files = dupes::candidates(&tree, min);
                drop(tree);
                let _ = tx.send(dupes::find(files, &p));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.error = None;
                self.running = Some(Running {
                    progress,
                    rx,
                    started: Instant::now(),
                });
            }
            Err(e) => self.error = Some(format!("The search could not start: {e}")),
        }
    }

    /// Picks up a finished search.
    pub fn poll(&mut self, ctx: &egui::Context, data: &mut Data) {
        let Some(r) = &self.running else {
            return;
        };
        match r.rx.try_recv() {
            Ok(Ok(groups)) => {
                data.dupes = Some(groups);
                self.made_for = None;
                self.running = None;
            }
            Ok(Err(e)) => {
                if e != anabolic_core::space::CANCELLED {
                    self.error = Some(e);
                }
                self.running = None;
            }
            Err(_) => ctx.request_repaint_after(Duration::from_millis(200)),
        }
    }

    pub fn stop(&mut self) {
        if let Some(r) = &self.running {
            r.progress.cancel();
        }
    }

    fn rebuild(&mut self, data: &Data) {
        let Some(groups) = &data.dupes else {
            return;
        };
        let key = (data.version, groups.len());
        if self.made_for == Some(key) {
            return;
        }
        self.rows.clear();
        for (g, group) in groups.iter().enumerate() {
            let left: Vec<u32> = group
                .nodes
                .iter()
                .copied()
                .filter(|&n| !data.tree.is_gone(n))
                .collect();
            // Once all but one are deleted it is no longer a duplicate.
            if left.len() < 2 {
                continue;
            }
            self.rows.push((g as u32, NONE));
            self.rows.extend(left.into_iter().map(|n| (g as u32, n)));
        }
        self.made_for = Some(key);
    }
}

fn date(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    anabolic_core::sysinfo::local_time(secs * 10_000_000 + 116_444_736_000_000_000)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

pub fn show(
    ui: &mut Ui,
    dupes: &mut Dupes,
    data: &mut Data,
    min_mb: &mut u32,
    out: &mut Vec<UiCmd>,
) {
    let ctx = ui.ctx().clone();
    if let Some(r) = &dupes.running {
        use std::sync::atomic::Ordering::Relaxed;
        let p = &r.progress;
        let (read, total) = (p.read.load(Relaxed), p.total.load(Relaxed));
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!(
                "Comparing {} files that share a size: {} of {} read",
                fmt::count(p.candidates.load(Relaxed) as u64),
                fmt::bytes(read),
                fmt::bytes(total)
            ));
            ui.label(
                RichText::new(fmt::duration(r.started.elapsed()))
                    .small()
                    .color(theme::TEXT_WEAK),
            );
            if ui.button("Stop").clicked() {
                p.cancel();
            }
        });
        if total > 0 {
            ui.add(egui::ProgressBar::new(read as f32 / total as f32).desired_height(4.0));
        }
        ui.label(
            RichText::new(
                "It reads at the lowest priority there is, so what you do goes first; the \
                 page stays usable meanwhile.",
            )
            .small()
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let mut start = false;
    ui.horizontal(|ui| {
        ui.label("Files of at least");
        ui.add(
            egui::DragValue::new(min_mb)
                .range(0..=100_000)
                .suffix(" MB")
                .speed(1.0),
        );
        let label = if data.dupes.is_some() {
            "Look again"
        } else {
            "Find duplicates"
        };
        start = ui.button(label).clicked();
        if let Some(e) = &dupes.error {
            ui.label(RichText::new(e).color(theme::BAD));
        }
    });
    if start {
        let min = u64::from(*min_mb) << 20;
        dupes.start(&ctx, data.tree.clone(), min);
        return;
    }
    if data.dupes.is_none() {
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Files of the same size are compared, first by their start, middle and end, \
                 then whole. On a large drive this reads a lot and takes minutes; it runs in \
                 the background. Files kept in the cloud and not on the disk are left out, \
                 and so are a file's second names (hard links), which take no room twice.",
            )
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    dupes.rebuild(data);
    let groups = data.dupes.clone().unwrap_or_default();
    let shown: Vec<u32> = dupes
        .rows
        .iter()
        .filter(|r| r.1 == NONE)
        .map(|r| r.0)
        .collect();
    let wasted: u64 = shown
        .iter()
        .map(|&g| {
            let group = &groups[g as usize];
            let left = group
                .nodes
                .iter()
                .filter(|&&n| !data.tree.is_gone(n))
                .count() as u64;
            group.size * left.saturating_sub(1)
        })
        .sum();
    ui.label(
        RichText::new(format!(
            "{} sets of the same file; keeping one of each frees {}",
            fmt::count(shown.len() as u64),
            fmt::bytes(wasted)
        ))
        .color(theme::TEXT_WEAK),
    );
    if dupes.rows.is_empty() {
        return;
    }
    let tree = data.tree.clone();
    Columns::load(ui.ctx(), "space-dupes", &COLUMNS).show(ui, |table, columns| {
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
                body.rows(widgets::ROW, dupes.rows.len(), |mut row| {
                    let (g, n) = dupes.rows[row.index()];
                    let group = &groups[g as usize];
                    if n == NONE {
                        let copies = group.nodes.iter().filter(|&&n| !tree.is_gone(n)).count();
                        for &c in columns.order() {
                            row.col(|ui| match c {
                                0 => {
                                    ui.label(widgets::strong(
                                        format!("{copies} copies of {}", fmt::bytes(group.size)),
                                        theme::BODY,
                                    ));
                                }
                                1 => right(
                                    ui,
                                    RichText::new(format!(
                                        "{} more",
                                        fmt::bytes(group.size * (copies as u64 - 1))
                                    ))
                                    .color(theme::ELEVATED_TEXT),
                                ),
                                _ => {}
                            });
                        }
                        return;
                    }
                    let node = tree.node(n);
                    row.set_selected(data.selected == Some(n));
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                ui.add_space(14.0);
                                ui.add(Label::new(tree.name(n)).truncate().selectable(false));
                            }
                            1 => right(ui, fmt::bytes(node.size)),
                            2 => {
                                ui.label(
                                    RichText::new(date(node.modified)).color(theme::TEXT_WEAK),
                                );
                            }
                            _ => {
                                ui.add(
                                    Label::new(
                                        RichText::new(tree.path_of(node.parent).to_string_lossy())
                                            .color(theme::TEXT_DIM),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                            }
                        });
                    }
                    let response = row.response();
                    if response.clicked() {
                        data.selected = Some(n);
                    }
                    if response.double_clicked() {
                        data.reveal = Some(n);
                    }
                    response.context_menu(|ui| node_menu(ui, data, n, out));
                });
            })
    });
}
