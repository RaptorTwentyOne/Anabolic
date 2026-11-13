// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Performance > Devices > Interrupts: how each PCI device interrupts the
//! processor (by message or by the shared wire), what its hardware can do,
//! what it will do after a restart, and which processors handle it, in
//! the manner of the MSI utilities; set from the pane, after a question.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::interrupts::{Change, Device, cores_words};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{panes, theme, widgets};

/// Read again this often while shown: a change made here shows at once.
const WHILE_SHOWN: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct InterruptsView {
    read: Option<Result<Arc<Vec<Device>>, String>>,
    reading: Option<Receiver<Result<Vec<Device>, String>>>,
    read_at: Option<Instant>,
    selected: Option<String>,
    /// The processor picked in the pane; None is Windows' choice.
    core: Option<u32>,
}

impl InterruptsView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("interrupts".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::interrupts::read());
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.reading = Some(rx);
            self.read_at = Some(Instant::now());
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.reading
            && let Ok(result) = rx.try_recv()
        {
            self.read = Some(result.map(Arc::new));
            self.reading = None;
        }
        if self.reading.is_none() && self.read_at.is_none_or(|t| t.elapsed() >= WHILE_SHOWN) {
            self.start(ctx);
        }
    }
}

fn next_words(d: &Device) -> String {
    match (d.messages_next(), d.pending()) {
        (true, true) => "MSI after a restart".into(),
        (false, true) => "Line-based after a restart".into(),
        _ => "The same".into(),
    }
}

const TITLES: [(&str, bool); 6] = [
    ("Device", false),
    ("Kind", false),
    ("Now", false),
    ("Next start", false),
    ("Hardware can", false),
    ("Processors", false),
];
const COLUMNS: [Spec; 6] = [
    Spec::new(300.0).clip(),
    Spec::new(80.0).clip(),
    Spec::new(130.0).clip(),
    Spec::new(170.0).clip(),
    Spec::new(260.0).clip(),
    Spec::new(110.0).clip(),
];

pub fn show(ui: &mut Ui, view: &mut InterruptsView, out: &mut Vec<UiCmd>) {
    view.tick(ui.ctx());
    ui.add(
        Label::new(
            RichText::new(
                "How each device interrupts the processor, as the MSI utilities show it. By \
                 message (MSI, MSI-X) each device and queue has an interrupt of its own; line-based \
                 ones share a wire and are checked in turn. Most drivers already use messages. A \
                 change takes effect at the next restart.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    ui.add_space(4.0);
    let devices = match &view.read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(theme::BAD));
            return;
        }
        Some(Ok(d)) => d.clone(),
    };
    if let Some(d) = view
        .selected
        .as_ref()
        .and_then(|id| devices.iter().find(|d| &d.id == id))
    {
        let mut close = false;
        panes::side(ui, ("irq-pane", &d.id), |ui| {
            close = pane(ui, d, &mut view.core, out);
        });
        if close {
            view.selected = None;
        }
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let mut pick = None;
        Columns::load(ui.ctx(), "interrupts", &COLUMNS).show(ui, |table, columns| {
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
                    body.rows(widgets::ROW, devices.len(), |mut row| {
                        let d = &devices[row.index()];
                        row.set_selected(view.selected.as_ref() == Some(&d.id));
                        for &c in columns.order() {
                            row.col(|ui| {
                                let text = match c {
                                    0 => RichText::new(&d.name),
                                    1 => RichText::new(d.kind.label()).color(theme::TEXT_WEAK),
                                    2 => RichText::new(d.now_words()).color(
                                        if d.messages_now.is_some() {
                                            theme::TEXT
                                        } else {
                                            theme::ELEVATED_TEXT
                                        },
                                    ),
                                    3 => RichText::new(next_words(d)).color(if d.pending() {
                                        theme::ELEVATED_TEXT
                                    } else {
                                        theme::TEXT_DIM
                                    }),
                                    4 => RichText::new(d.can_words()).color(theme::TEXT_WEAK),
                                    _ => RichText::new(d.cores.map_or("Any".into(), cores_words))
                                        .color(theme::TEXT_WEAK),
                                };
                                ui.add(Label::new(text).truncate());
                            });
                        }
                        if row.response().clicked() {
                            pick = Some(d.id.clone());
                        }
                    });
                })
        });
        if let Some(p) = pick {
            view.selected = if view.selected.as_ref() == Some(&p) {
                None
            } else {
                view.core = devices
                    .iter()
                    .find(|d| d.id == p)
                    .and_then(|d| d.cores)
                    .filter(|m| m.count_ones() == 1)
                    .map(|m| m.trailing_zeros());
                Some(p)
            };
        }
    });
}

/// One device: what it does, and the two settings; true when closed.
fn pane(ui: &mut Ui, d: &Device, core: &mut Option<u32>, out: &mut Vec<UiCmd>) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.add(Label::new(widgets::strong(&d.name, theme::SUBTITLE)).wrap());
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            close = ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Close")
                .clicked();
        });
    });
    ui.label(RichText::new(d.kind.label()).color(theme::TEXT_WEAK));
    ui.add_space(6.0);
    panes::facts(ui, ("irq-facts", &d.id), |ui| {
        panes::fact(ui, "Now", d.now_words());
        panes::fact(ui, "Next start", next_words(d));
        panes::fact(ui, "Hardware can", d.can_words());
        panes::fact(
            ui,
            "Processors",
            d.cores.map_or("Any: Windows chooses".into(), cores_words),
        );
        panes::fact(ui, "Device", d.id.clone());
    });
    ui.add_space(10.0);
    widgets::section(ui, "Interrupts by");
    if d.changeable() {
        ui.horizontal(|ui| {
            let msi = d.messages_next();
            if ui.add_enabled(!msi, egui::Button::new("Use MSI")).clicked() {
                out.push(confirm(d, Change::Messages(true)));
            }
            if ui
                .add_enabled(msi, egui::Button::new("Use line-based"))
                .clicked()
            {
                out.push(confirm(d, Change::Messages(false)));
            }
        });
    } else {
        panes::weak(
            ui,
            if d.kind == anabolic_core::interrupts::Kind::Storage {
                "A drive controller: if its driver could not start the new way, Windows could not \
                 start. Anabolic leaves it as it is."
            } else {
                "Its hardware interrupts only by the wire."
            },
        );
    }
    ui.add_space(8.0);
    widgets::section(ui, "Processors");
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("irq-core", &d.id))
            .selected_text(
                core.map_or("Any (Windows chooses)".into(), |c| format!("Processor {c}")),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(core, None, "Any (Windows chooses)");
                for c in 0..cpus.min(64) {
                    ui.selectable_value(core, Some(c), format!("Processor {c}"));
                }
            });
        let want = core.map(|c| 1u64 << c);
        if ui
            .add_enabled(want != d.cores, egui::Button::new("Set"))
            .clicked()
        {
            out.push(confirm(d, Change::Cores(want)));
        }
    });
    panes::weak(
        ui,
        "Gaming guides move a graphics card's interrupts off processor 0, which takes most \
         others. A setting that does not suit the driver is ignored.",
    );
    close
}

fn confirm(d: &Device, change: Change) -> UiCmd {
    UiCmd::ConfirmInterrupts {
        id: d.id.clone(),
        name: d.name.clone(),
        change,
    }
}
