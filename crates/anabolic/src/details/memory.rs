// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Memory tab: a process' address space in the manner of VMMap, what
//! each allocation is and how much of it is committed and in RAM, by kind
//! and one by one. Reading it again later shows what grew since. Read on a
//! thread of its own, after the user agreed to the process being opened.

use std::sync::Arc;

use anabolic_core::ProcessInfo;
use anabolic_core::memmap::{Allocation, Kind, MemoryMap, growth};
use anabolic_core::model::ProcKey;
use anabolic_core::sysinfo::local_time;
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, Sense, Ui, vec2};

use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{fmt, theme, widgets};

#[derive(Default)]
pub struct MemoryView {
    reading: Option<(ProcKey, Receiver<Result<MemoryMap, String>>)>,
    current: Option<(ProcKey, Result<Arc<MemoryMap>, String>)>,
    /// The reading before the current one, for what grew.
    before: Option<Arc<MemoryMap>>,
    /// Only this kind in the table.
    kind: Option<Kind>,
    filter: String,
}

impl MemoryView {
    /// Reads `key`'s memory map: the user agreed to it being opened.
    pub fn start(&mut self, key: ProcKey, ctx: &egui::Context) {
        if self.reading.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("memory-map".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::memmap::read(key));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.reading = Some((key, rx));
        }
    }

    fn poll(&mut self) {
        let Some((key, rx)) = &self.reading else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        let key = *key;
        self.reading = None;
        // The last good reading of the same process is the one to compare
        // with.
        let previous = match self.current.take() {
            Some((k, Ok(map))) if k == key => Some(map),
            _ => None,
        };
        if previous.is_none() {
            self.before = None;
            self.kind = None;
        } else {
            self.before = previous;
        }
        self.current = Some((key, result.map(Arc::new)));
    }
}

fn color(kind: Kind) -> Color32 {
    match kind {
        Kind::Image => theme::KERNEL,
        Kind::MappedFile => theme::DISK_READ,
        Kind::Shareable => theme::NET_RX,
        Kind::Heap => theme::MEMORY,
        Kind::Stack => theme::GPU,
        Kind::PrivateData => theme::COMMIT,
        Kind::Unusable => theme::TEXT_DIM,
    }
}

fn signed(bytes: i64) -> String {
    if bytes >= 0 {
        format!("+{}", fmt::bytes(bytes as u64))
    } else {
        format!("−{}", fmt::bytes(bytes.unsigned_abs()))
    }
}

pub fn tab(ui: &mut Ui, view: &mut MemoryView, p: &ProcessInfo, out: &mut Vec<UiCmd>) {
    view.poll();
    let reading = view.reading.as_ref().is_some_and(|(k, _)| *k == p.key);
    let current = view
        .current
        .as_ref()
        .filter(|(k, _)| *k == p.key)
        .map(|(_, r)| r.clone());
    let Some(current) = current else {
        if reading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading its memory…").color(theme::TEXT_WEAK));
            });
            return;
        }
        ui.add_space(4.0);
        ui.add(
            Label::new(
                RichText::new(
                    "What each part of its memory is: its program and DLLs, mapped files, \
                     heaps, thread stacks and private data, with how much of each is committed \
                     and how much is in RAM. Reading it again later shows what grew. Anabolic \
                     opens the process to read this; nothing runs in it.",
                )
                .color(theme::TEXT_WEAK),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        if ui.button("Read its memory").clicked() {
            out.push(UiCmd::ConfirmMemoryMap(p.key));
        }
        return;
    };
    let map = match current {
        Ok(map) => map,
        Err(e) => {
            ui.label(
                RichText::new(format!("Its memory could not be read: {e}"))
                    .color(theme::ELEVATED_TEXT),
            );
            if ui.button("Try again").clicked() {
                view.start(p.key, ui.ctx());
            }
            return;
        }
    };

    let total = map.total();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} committed  ·  {} in RAM, {} of it its own  ·  {} allocations  ·  read at {} \
                 in {}",
                fmt::bytes(total.committed),
                fmt::bytes(total.working_set),
                fmt::bytes(total.private_ws),
                total.count,
                local_time(map.at)
                    .map(|t| t.chars().skip(11).take(8).collect::<String>())
                    .unwrap_or_default(),
                fmt::millis(map.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if reading {
                ui.spinner();
            } else if ui
                .button("Read again")
                .on_hover_text("And show what grew since this reading")
                .clicked()
            {
                view.start(p.key, ui.ctx());
            }
        });
    });
    ui.add_space(4.0);
    bar(ui, &map);
    ui.add_space(4.0);
    kinds(ui, view, &map);

    if let Some(before) = &view.before {
        let g = growth(before, &map);
        ui.add_space(6.0);
        let mut parts: Vec<String> = g
            .by_kind
            .iter()
            .map(|(k, d)| format!("{} {}", k.label(), signed(*d)))
            .collect();
        if parts.is_empty() {
            parts.push("nothing committed changed".into());
        }
        ui.label(
            RichText::new(format!(
                "Since the reading {:.0} s before: {}",
                g.seconds,
                parts.join(", ")
            ))
            .color(theme::TEXT),
        );
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if let Some(k) = view.kind
            && ui
                .button(format!("{}  {}", k.label(), theme::icon::CLOSE))
                .on_hover_text("Every kind again")
                .clicked()
        {
            view.kind = None;
        }
        widgets::clearable(ui, &mut view.filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter: what, address, protection")
                .desired_width(260.0)
        });
    });
    ui.add_space(4.0);

    let grew: std::collections::HashMap<u64, i64> = view
        .before
        .as_ref()
        .map(|b| {
            let mut was = std::collections::HashMap::new();
            for a in &b.allocations {
                was.insert(a.base, a.committed);
            }
            map.allocations
                .iter()
                .map(|a| {
                    (
                        a.base,
                        a.committed as i64 - *was.get(&a.base).unwrap_or(&0) as i64,
                    )
                })
                .filter(|(_, d)| *d != 0)
                .collect()
        })
        .unwrap_or_default();
    let f = view.filter.trim().to_lowercase();
    let mut rows: Vec<&Allocation> = map
        .allocations
        .iter()
        .filter(|a| a.kind != Kind::Unusable)
        .filter(|a| view.kind.is_none_or(|k| a.kind == k))
        .filter(|a| {
            f.is_empty()
                || a.detail.to_lowercase().contains(&f)
                || a.protection.to_lowercase().contains(&f)
                || format!("{:x}", a.base).contains(&f)
        })
        .collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.committed));
    table(ui, &rows, &grew);
}

/// The committed memory as one bar, by kind.
fn bar(ui: &mut Ui, map: &MemoryMap) {
    let total = map.total().committed.max(1);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 12.0), Sense::hover());
    let mut x = rect.left();
    for k in Kind::ALL {
        let t = map.totals(k);
        if t.committed == 0 {
            continue;
        }
        let w = (t.committed as f32 / total as f32 * width).max(1.0);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height())),
            0.0,
            color(k),
        );
        x += w;
    }
}

/// One line per kind; a click narrows the table to it.
fn kinds(ui: &mut Ui, view: &mut MemoryView, map: &MemoryMap) {
    egui::Grid::new(("memory-kinds", map.key.pid))
        .num_columns(5)
        .spacing([18.0, 3.0])
        .show(ui, |ui| {
            for title in ["", "Committed", "In RAM", "Its own in RAM", "Allocations"] {
                ui.label(RichText::new(title).small().color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for k in Kind::ALL {
                let t = map.totals(k);
                if t.count == 0 || k == Kind::Unusable {
                    continue;
                }
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, color(k));
                    let label = if view.kind == Some(k) {
                        widgets::strong(k.label(), theme::BODY)
                    } else {
                        RichText::new(k.label())
                    };
                    if ui
                        .add(Label::new(label).sense(Sense::click()))
                        .on_hover_text("Show only these")
                        .clicked()
                    {
                        view.kind = if view.kind == Some(k) { None } else { Some(k) };
                    }
                });
                ui.label(fmt::bytes(t.committed));
                ui.label(fmt::bytes(t.working_set));
                ui.label(fmt::bytes(t.private_ws));
                ui.label(t.count.to_string());
                ui.end_row();
            }
        });
}

const TITLES: [(&str, bool); 8] = [
    ("Address", false),
    ("Kind", false),
    ("What", false),
    ("Committed", true),
    ("In RAM", true),
    ("Its own", true),
    ("Grew", true),
    ("Protection", false),
];
const COLUMNS: [Spec; 8] = [
    Spec::new(118.0),
    Spec::new(110.0).clip(),
    Spec::new(330.0).clip(),
    Spec::new(84.0),
    Spec::new(84.0),
    Spec::new(84.0),
    Spec::new(84.0),
    Spec::new(150.0).clip(),
];

fn table(ui: &mut Ui, rows: &[&Allocation], grew: &std::collections::HashMap<u64, i64>) {
    let right = |ui: &mut Ui, text: RichText| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(Label::new(text).selectable(false));
        });
    };
    Columns::load(ui.ctx(), "memory-map", &COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, rows.len(), |mut row| {
                    let a = rows[row.index()];
                    let marked = a.detail.contains("xecutable");
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                ui.label(
                                    RichText::new(format!("{:012X}", a.base))
                                        .monospace()
                                        .color(theme::TEXT_WEAK),
                                );
                            }
                            1 => {
                                ui.add(
                                    Label::new(RichText::new(a.kind.label()).color(color(a.kind)))
                                        .truncate(),
                                );
                            }
                            2 => {
                                let text = RichText::new(&a.detail).color(if marked {
                                    theme::ELEVATED_TEXT
                                } else {
                                    theme::TEXT
                                });
                                ui.add(Label::new(text).truncate())
                                    .on_hover_text(format!("{} block(s)", a.blocks));
                            }
                            3 => right(ui, RichText::new(fmt::bytes(a.committed))),
                            4 => right(ui, RichText::new(fmt::bytes(a.working_set))),
                            5 => right(
                                ui,
                                RichText::new(fmt::bytes(a.private_ws)).color(theme::TEXT_WEAK),
                            ),
                            6 => {
                                if let Some(d) = grew.get(&a.base) {
                                    right(
                                        ui,
                                        RichText::new(signed(*d)).color(if *d > 0 {
                                            theme::ELEVATED_TEXT
                                        } else {
                                            theme::TEXT_WEAK
                                        }),
                                    );
                                }
                            }
                            _ => {
                                ui.add(
                                    Label::new(
                                        RichText::new(&a.protection).color(theme::TEXT_WEAK),
                                    )
                                    .truncate(),
                                );
                            }
                        });
                    }
                });
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_changes() {
        assert_eq!(signed(2048), format!("+{}", fmt::bytes(2048)));
        assert!(signed(-2048).starts_with('−'));
    }
}
