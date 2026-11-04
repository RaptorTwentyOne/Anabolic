// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Compare tab: this scan against an earlier one of the same folder,
//! as a tree of what grew and shrank, the largest change first. Each scan
//! leaves an outline (its folders of a megabyte or more) in Anabolic's
//! data folder; reading and comparing them happens on a thread of its own.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anabolic_core::space::Tree;
use anabolic_core::space::outline::{self, Diff};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use super::Data;
use crate::table::{Columns, Spec};
use crate::{fmt, theme, widgets};

const TITLES: [(&str, bool); 4] = [
    ("Folder", false),
    ("Then", true),
    ("Now", true),
    ("Change", true),
];
const COLUMNS: [Spec; 4] = [
    Spec::new(420.0).clip(),
    Spec::new(100.0),
    Spec::new(100.0),
    Spec::new(110.0),
];

/// Where a scan's outlines are kept.
pub fn outlines_dir() -> Option<PathBuf> {
    crate::data_dir().map(|d| d.join("space"))
}

#[derive(Default)]
pub struct Compare {
    /// The earlier outlines of this folder: (when, file, size).
    kept: Option<Vec<(i64, PathBuf, u64)>>,
    listing: Option<Receiver<Vec<(i64, PathBuf, u64)>>>,
    /// The one compared with.
    chosen: Option<i64>,
    working: Option<Receiver<Option<Vec<Diff>>>>,
    diff: Option<Vec<Diff>>,
    open: HashSet<u32>,
    rows: Vec<(u32, u16)>,
    stale: bool,
}

fn date(secs: i64) -> String {
    anabolic_core::sysinfo::local_time(secs * 10_000_000 + 116_444_736_000_000_000)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

fn signed(change: i64) -> String {
    match change {
        0 => "0".into(),
        c if c > 0 => format!("+{}", fmt::bytes(c as u64)),
        c => format!("−{}", fmt::bytes(c.unsigned_abs())),
    }
}

impl Compare {
    /// The tree of changes on screen, for saving.
    pub fn diff(&self) -> Option<Vec<Diff>> {
        self.diff.clone()
    }

    fn list(&mut self, ctx: &egui::Context, tree: &Tree, scanned_at: i64) {
        let (tx, rx) = bounded(1);
        let (path, ctx) = (tree.path.clone(), ctx.clone());
        let _ = std::thread::Builder::new()
            .name("space-outlines".into())
            .spawn(move || {
                let mut kept = outlines_dir()
                    .map(|d| outline::list(&d, &path))
                    .unwrap_or_default();
                kept.retain(|(at, _, _)| *at != scanned_at);
                let _ = tx.send(kept);
                ctx.request_repaint();
            });
        self.listing = Some(rx);
    }

    fn start(&mut self, ctx: &egui::Context, tree: Arc<Tree>, scanned_at: i64, file: PathBuf) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("space-compare".into())
            .spawn(move || {
                let now = outline::outline(&tree, scanned_at);
                drop(tree);
                let diff = outline::load(&file).map(|then| outline::compare(&then, &now));
                let _ = tx.send(diff);
                ctx.request_repaint();
            });
        self.working = Some(rx);
        self.diff = None;
        self.open.clear();
        self.open.insert(0);
        self.stale = true;
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let Some(diff) = &self.diff else {
            return;
        };
        let mut stack = vec![(0u32, 0u16)];
        while let Some((i, depth)) = stack.pop() {
            self.rows.push((i, depth));
            if self.open.contains(&i) {
                for &c in diff[i as usize].children.iter().rev() {
                    stack.push((c, depth + 1));
                }
            }
        }
        self.stale = false;
    }
}

pub fn show(ui: &mut Ui, cmp: &mut Compare, data: &mut Data, keep: &mut bool) {
    let ctx = ui.ctx().clone();
    if cmp.kept.is_none() && cmp.listing.is_none() {
        cmp.list(&ctx, &data.tree, data.scanned_at);
    }
    if let Some(rx) = &cmp.listing {
        match rx.try_recv() {
            Ok(kept) => {
                // The scan before this one, unless another was chosen.
                if cmp.chosen.is_none()
                    && let Some((at, file, _)) = kept.first().cloned()
                {
                    cmp.chosen = Some(at);
                    cmp.start(&ctx, data.tree.clone(), data.scanned_at, file);
                }
                cmp.kept = Some(kept);
                cmp.listing = None;
            }
            Err(_) => ctx.request_repaint_after(Duration::from_millis(100)),
        }
    }
    if let Some(rx) = &cmp.working {
        match rx.try_recv() {
            Ok(diff) => {
                cmp.diff = diff;
                cmp.working = None;
                cmp.stale = true;
            }
            Err(_) => ctx.request_repaint_after(Duration::from_millis(100)),
        }
    }
    let mut pick = None;
    ui.horizontal(|ui| {
        ui.label("Compared with the scan of");
        let kept = cmp.kept.clone().unwrap_or_default();
        let text = cmp
            .chosen
            .and_then(|at| kept.iter().find(|k| k.0 == at))
            .map_or_else(
                || "choose one".to_string(),
                |(at, _, size)| format!("{}  ({})", date(*at), fmt::bytes(*size)),
            );
        egui::ComboBox::from_id_salt("space-compare-with")
            .selected_text(text)
            .width(240.0)
            .show_ui(ui, |ui| {
                for (at, file, size) in &kept {
                    if ui
                        .selectable_label(
                            cmp.chosen == Some(*at),
                            format!("{}  ({})", date(*at), fmt::bytes(*size)),
                        )
                        .clicked()
                    {
                        pick = Some((*at, file.clone()));
                    }
                }
            });
        if cmp.working.is_some() {
            ui.spinner();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.checkbox(keep, "Keep an outline of each scan")
                .on_hover_text(
                    "The folders of a megabyte or more and their sizes, in Anabolic's data \
                     folder, to compare a later scan with. The last 40 of each folder are kept.",
                );
        });
    });
    if let Some((at, file)) = pick {
        cmp.chosen = Some(at);
        cmp.start(&ctx, data.tree.clone(), data.scanned_at, file);
    }
    match &cmp.kept {
        Some(kept) if kept.is_empty() => {
            ui.add_space(6.0);
            let text = if *keep {
                "No earlier scan of this folder yet: the next scan will be compared with this one."
            } else {
                "No earlier scan of this folder is kept. Turn on keeping outlines, and the next \
                 scan can be compared with this one."
            };
            ui.label(RichText::new(text).color(theme::TEXT_WEAK));
            return;
        }
        None => return,
        _ => {}
    }
    if cmp.stale {
        cmp.rebuild();
    }
    let Some(diff) = &cmp.diff else {
        if cmp.chosen.is_some() && cmp.working.is_none() {
            ui.label(RichText::new("That outline could not be read.").color(theme::BAD));
        }
        return;
    };
    let grew: i64 = diff[0]
        .children
        .iter()
        .map(|&c| diff[c as usize].change())
        .filter(|&c| c > 0)
        .sum();
    let shrank: i64 = diff[0]
        .children
        .iter()
        .map(|&c| diff[c as usize].change())
        .filter(|&c| c < 0)
        .sum();
    ui.label(
        RichText::new(format!(
            "{} in all: {} grew, {} shrank. Folders under a megabyte then or now show as \"—\".",
            signed(diff[0].change()),
            fmt::bytes(grew as u64),
            fmt::bytes(shrank.unsigned_abs())
        ))
        .color(theme::TEXT_WEAK),
    );
    let mut toggle = None;
    Columns::load(ui.ctx(), "space-compare", &COLUMNS).show(ui, |table, columns| {
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
                body.rows(widgets::ROW, cmp.rows.len(), |mut row| {
                    let (i, depth) = cmp.rows[row.index()];
                    let d = &diff[i as usize];
                    let size = |v: Option<u64>| v.map_or("—".to_string(), fmt::bytes);
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                ui.add_space(f32::from(depth) * 14.0);
                                if d.children.is_empty() {
                                    ui.add_space(12.0);
                                } else {
                                    let arrow = if cmp.open.contains(&i) { "⏷" } else { "⏵" };
                                    if ui
                                        .add(
                                            Label::new(
                                                RichText::new(arrow).color(theme::TEXT_WEAK),
                                            )
                                            .sense(Sense::click())
                                            .selectable(false),
                                        )
                                        .clicked()
                                    {
                                        toggle = Some(i);
                                    }
                                }
                                ui.add(Label::new(&d.name).truncate().selectable(false));
                            }
                            1 => right(ui, RichText::new(size(d.then)).color(theme::TEXT_WEAK)),
                            2 => right(ui, size(d.now)),
                            _ => {
                                let change = d.change();
                                let color = if change > 0 {
                                    theme::ELEVATED_TEXT
                                } else if change < 0 {
                                    theme::GOOD
                                } else {
                                    theme::TEXT_DIM
                                };
                                right(ui, RichText::new(signed(change)).color(color));
                            }
                        });
                    }
                    if row.response().double_clicked() && !d.children.is_empty() {
                        toggle = Some(i);
                    }
                });
            })
    });
    if let Some(i) = toggle {
        if !cmp.open.remove(&i) {
            cmp.open.insert(i);
        }
        cmp.stale = true;
    }
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}
