// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Performance > Checkup: what costs speed without saying so (memory below
//! its rated speed, a card on half its lanes, background recording, a
//! monitor at 60 Hz...), what to change first, each with where to change
//! it; then the settings worth knowing, and what is as it should be. Read
//! in the background now and then, for the Overview, and again on request.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::checkup::{Check, Checkup, Fix, Verdict};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Frame, Label, Layout, Margin, RichText, Stroke, Ui, vec2};

use crate::proc_table::UiCmd;
use crate::{theme, widgets};

/// The first read waits for the window to settle; then again this often.
const FIRST: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(60 * 60);
const WHILE_SHOWN: Duration = Duration::from_secs(5 * 60);

pub struct CheckupView {
    read: Option<Arc<Checkup>>,
    reading: Option<Receiver<Checkup>>,
    read_at: Option<Instant>,
    born: Instant,
    /// The fine ones are folded away until asked for.
    pub show_good: bool,
}

impl Default for CheckupView {
    fn default() -> Self {
        CheckupView {
            read: None,
            reading: None,
            read_at: None,
            born: Instant::now(),
            show_good: false,
        }
    }
}

impl CheckupView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("checkup".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::checkup::read());
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.reading = Some(rx);
            self.read_at = Some(Instant::now());
        }
    }

    /// Reads when due; call every frame, with whether the view is on
    /// screen.
    pub fn tick(&mut self, ctx: &egui::Context, shown: bool) {
        if let Some(rx) = &self.reading
            && let Ok(read) = rx.try_recv()
        {
            self.read = Some(Arc::new(read));
            self.reading = None;
        }
        if self.reading.is_some() {
            return;
        }
        let due = match self.read_at {
            None => shown || self.born.elapsed() >= FIRST,
            Some(at) => at.elapsed() >= if shown { WHILE_SHOWN } else { EVERY },
        };
        if due {
            self.start(ctx);
        }
    }

    pub fn checkup(&self) -> Option<&Arc<Checkup>> {
        self.read.as_ref()
    }
}

/// The Performance page's line for the view.
pub fn summary(checkup: Option<&Checkup>) -> String {
    let Some(c) = checkup else {
        return "Settings that cost speed".into();
    };
    match (c.count(Verdict::Change), c.count(Verdict::Consider)) {
        (0, 0) => "Nothing to change".into(),
        (0, n) => format!("{n} worth a look"),
        (n, 0) => format!("{n} to change"),
        (n, m) => format!("{n} to change  ·  {m} worth a look"),
    }
}

fn color(v: Verdict) -> egui::Color32 {
    match v {
        Verdict::Change => theme::BAD,
        Verdict::Consider => theme::ELEVATED_TEXT,
        Verdict::Info => theme::CPU,
        Verdict::Good => theme::GOOD,
    }
}

/// The way to change it: a button for a settings page, words otherwise.
fn fix(ui: &mut Ui, fix: &Fix, out: &mut Vec<UiCmd>) {
    match fix {
        Fix::Open { label, uri } => {
            if ui
                .button(format!("Open {label}"))
                .on_hover_text(uri)
                .clicked()
            {
                out.push(UiCmd::OpenUrl(uri.clone()));
            }
        }
        Fix::Firmware(text) => how(ui, &format!("in the firmware setup (BIOS/UEFI): {text}")),
        Fix::Hand(text) => how(ui, text),
    }
}

/// A change made by hand, after a word that says so.
fn how(ui: &mut Ui, text: &str) {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::proportional(theme::BODY);
    job.append(
        "To change it: ",
        0.0,
        egui::TextFormat::simple(font.clone(), theme::TEXT_DIM),
    );
    job.append(text, 0.0, egui::TextFormat::simple(font, theme::TEXT));
    ui.add(Label::new(job).wrap());
}

/// One check that asks for something, in a card with its verdict's colour
/// down the side.
fn card(ui: &mut Ui, c: &Check, out: &mut Vec<UiCmd>) {
    let shown = Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(Margin {
            left: 16,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(Label::new(widgets::strong(&c.title, theme::SUBTITLE)).wrap());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(c.area.label())
                            .size(theme::CAPTION)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
            ui.add(Label::new(RichText::new(&c.now).color(color(c.verdict))).wrap());
            if !c.why.is_empty() {
                ui.add_space(2.0);
                ui.add(Label::new(RichText::new(&c.why).color(theme::TEXT_WEAK)).wrap());
            }
            if let Some(f) = &c.fix {
                ui.add_space(6.0);
                fix(ui, f, out);
            }
        });
    let r = shown.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_size(r.min + vec2(1.0, 6.0), vec2(3.0, r.height() - 12.0)),
        1.5,
        color(c.verdict),
    );
    ui.add_space(8.0);
}

/// A check that asks nothing: one line.
fn line(ui: &mut Ui, c: &Check) {
    ui.horizontal_wrapped(|ui| {
        ui.label(widgets::icon(theme::icon::CHECK, 11.0).color(theme::GOOD));
        ui.label(RichText::new(&c.title).color(theme::TEXT));
        ui.label(RichText::new(&c.now).color(theme::TEXT_WEAK));
    });
    if !c.why.is_empty() {
        ui.indent(("good-why", &c.key), |ui| {
            ui.add(
                Label::new(
                    RichText::new(&c.why)
                        .size(theme::META)
                        .color(theme::TEXT_DIM),
                )
                .wrap(),
            );
        });
    }
}

pub fn show(ui: &mut Ui, view: &mut CheckupView, out: &mut Vec<UiCmd>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Checkup").font(theme::brand(theme::TITLE)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let reading = view.reading.is_some();
            if ui
                .add_enabled(!reading, egui::Button::new("Check again"))
                .clicked()
            {
                view.start(ui.ctx());
            }
            if reading {
                ui.spinner();
            } else if let Some(c) = &view.read {
                ui.label(
                    RichText::new(format!(
                        "{} checks in {}",
                        c.checks.len(),
                        crate::fmt::millis(c.took)
                    ))
                    .color(theme::TEXT_WEAK),
                );
            }
        });
    });
    ui.add(
        Label::new(
            RichText::new(
                "What costs speed without saying so: what you would otherwise look up in CPU-Z, \
                 GPU-Z, HWiNFO and Windows' settings one by one. Nothing is changed here; each \
                 says where to change it.",
            )
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    ui.add_space(10.0);
    let Some(c) = view.read.clone() else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Checking…").color(theme::TEXT_WEAK));
        });
        return;
    };
    let width = ui.available_width().min(900.0);
    ui.allocate_ui(vec2(width, 0.0), |ui| {
        let asks: Vec<&Check> = c
            .checks
            .iter()
            .filter(|x| matches!(x.verdict, Verdict::Change | Verdict::Consider))
            .collect();
        if asks.is_empty() {
            ui.label(
                RichText::new("Nothing here costs speed that Anabolic can see.")
                    .size(theme::SUBTITLE)
                    .color(theme::GOOD),
            );
            ui.add_space(10.0);
        }
        for x in &asks {
            card(ui, x, out);
        }
        let info: Vec<&Check> = c
            .checks
            .iter()
            .filter(|x| x.verdict == Verdict::Info)
            .collect();
        if !info.is_empty() {
            ui.add_space(6.0);
            widgets::section(ui, "Good to know");
            ui.label(
                RichText::new("Settings with a trade either way: what each is, and what it means.")
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(6.0);
            for x in info {
                card(ui, x, out);
            }
        }
        let good: Vec<&Check> = c
            .checks
            .iter()
            .filter(|x| x.verdict == Verdict::Good)
            .collect();
        if !good.is_empty() {
            ui.add_space(6.0);
            let title = format!("As it should be ({})", good.len());
            let header = egui::CollapsingHeader::new(RichText::new(title).strong())
                .id_salt("checkup-good")
                .open(Some(view.show_good))
                .show(ui, |ui| {
                    for x in good {
                        line(ui, x);
                        ui.add_space(2.0);
                    }
                });
            if header.header_response.clicked() {
                view.show_good = !view.show_good;
            }
        }
    });
}
