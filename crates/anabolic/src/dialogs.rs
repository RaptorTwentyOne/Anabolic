//! Modal confirmations and transient notifications.

use std::time::{Duration, Instant};

use anabolic_core::actions::Target;
use eframe::egui::{self, Align2, Frame, Id, Key, Margin, Modal, RichText, Stroke, vec2};

use crate::theme;
use crate::widgets;

pub struct KillDialog {
    pub title: String,
    /// What is being terminated, e.g. "chrome.exe (PID 1234)".
    pub subject: String,
    /// Extra line, e.g. "and 12 descendant processes".
    pub detail: Option<String>,
    /// Children before parents.
    pub targets: Vec<Target>,
    pub critical: Vec<String>,
    pub dont_ask: bool,
}

pub enum KillChoice {
    Pending,
    Confirm { dont_ask_again: bool },
    Cancel,
}

impl KillDialog {
    pub fn show(&mut self, ctx: &egui::Context) -> KillChoice {
        let mut choice = KillChoice::Pending;
        let response = Modal::new(Id::new("kill-dialog")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(widgets::strong(&self.title, theme::SUBTITLE));
            ui.add_space(6.0);
            ui.label(&self.subject);
            if let Some(detail) = &self.detail {
                ui.label(RichText::new(detail).color(theme::TEXT_WEAK));
            }
            ui.label(RichText::new("Unsaved data in these processes will be lost.").color(theme::TEXT_WEAK));
            if !self.critical.is_empty() {
                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(theme::BAD_FILL)
                    .stroke(Stroke::new(1.0, theme::BAD))
                    .inner_margin(Margin::same(8))
                    .corner_radius(4)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} is a critical system process. Terminating it will crash Windows (bugcheck).",
                                self.critical.join(", ")
                            ))
                            .color(theme::BAD),
                        );
                    });
            }
            ui.add_space(10.0);
            if self.critical.is_empty() {
                ui.checkbox(&mut self.dont_ask, "Don't ask again");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let kill = egui::Button::new(widgets::strong("Terminate", theme::BODY).color(theme::TEXT)).fill(theme::BAD_STRONG);
                if ui.add(kill).clicked() || (self.critical.is_empty() && ui.input(|i| i.key_pressed(Key::Enter))) {
                    choice = KillChoice::Confirm { dont_ask_again: self.dont_ask && self.critical.is_empty() };
                }
                if ui.button("Cancel").clicked() {
                    choice = KillChoice::Cancel;
                }
            });
        });
        if matches!(choice, KillChoice::Pending) && response.should_close() {
            choice = KillChoice::Cancel;
        }
        choice
    }
}

pub struct AffinityDialog {
    pub target: Target,
    pub mask: u64,
    pub system_mask: u64,
}

pub enum AffinityChoice {
    Pending,
    Apply(u64),
    Cancel,
}

impl AffinityDialog {
    pub fn show(&mut self, ctx: &egui::Context) -> AffinityChoice {
        let mut choice = AffinityChoice::Pending;
        let cpus: Vec<u32> = (0..64)
            .filter(|i| self.system_mask & (1 << i) != 0)
            .collect();
        let response = Modal::new(Id::new("affinity-dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(widgets::strong("Processor affinity", theme::SUBTITLE));
            ui.label(
                RichText::new(format!(
                    "{} (PID {})",
                    self.target.name, self.target.key.pid
                ))
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(8.0);
            egui::Grid::new("affinity-grid")
                .num_columns(8)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for (n, &cpu) in cpus.iter().enumerate() {
                        let mut on = self.mask & (1 << cpu) != 0;
                        if ui.checkbox(&mut on, format!("{cpu}")).changed() {
                            self.mask ^= 1 << cpu;
                        }
                        if n % 8 == 7 {
                            ui.end_row();
                        }
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    self.mask = self.system_mask;
                }
                if ui.small_button("None").clicked() {
                    self.mask = 0;
                }
                if ui.small_button("Even (no SMT siblings)").clicked() {
                    self.mask = self.system_mask & 0x5555_5555_5555_5555;
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.mask != 0, egui::Button::new("Apply"))
                    .clicked()
                {
                    choice = AffinityChoice::Apply(self.mask);
                }
                if ui.button("Cancel").clicked() {
                    choice = AffinityChoice::Cancel;
                }
            });
        });
        if matches!(choice, AffinityChoice::Pending) && response.should_close() {
            choice = AffinityChoice::Cancel;
        }
        choice
    }
}

/// A plain yes/no question.
pub struct ConfirmDialog {
    pub title: String,
    pub lines: Vec<String>,
    pub confirm: String,
}

pub enum ConfirmChoice {
    Pending,
    Confirm,
    Cancel,
}

impl ConfirmDialog {
    pub fn show(&self, ctx: &egui::Context) -> ConfirmChoice {
        let mut choice = ConfirmChoice::Pending;
        let response = Modal::new(Id::new("confirm-dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(widgets::strong(&self.title, theme::SUBTITLE));
            ui.add_space(6.0);
            for line in &self.lines {
                ui.label(RichText::new(line).color(theme::TEXT_WEAK));
                ui.add_space(2.0);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .button(widgets::strong(&self.confirm, theme::BODY))
                    .clicked()
                {
                    choice = ConfirmChoice::Confirm;
                }
                if ui.button("Cancel").clicked() {
                    choice = ConfirmChoice::Cancel;
                }
            });
        });
        if matches!(choice, ConfirmChoice::Pending) && response.should_close() {
            choice = ConfirmChoice::Cancel;
        }
        choice
    }
}

const TOAST_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Toasts {
    items: Vec<(Instant, String, bool)>,
}

impl Toasts {
    pub fn info(&mut self, text: impl Into<String>) {
        self.items.push((Instant::now(), text.into(), false));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.items.push((Instant::now(), text.into(), true));
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.items.retain(|(t, _, error)| {
            now - *t
                < if *error {
                    TOAST_LIFETIME * 2
                } else {
                    TOAST_LIFETIME
                }
        });
        if self.items.is_empty() {
            return;
        }
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -34.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for (_, text, error) in &self.items {
                    Frame::new()
                        .fill(theme::RAISED)
                        .stroke(Stroke::new(
                            1.0,
                            if *error { theme::BAD } else { theme::BORDER },
                        ))
                        .inner_margin(Margin::symmetric(12, 8))
                        .corner_radius(6)
                        .show(ui, |ui| {
                            ui.set_max_width(460.0);
                            ui.label(RichText::new(text).color(if *error {
                                theme::BAD
                            } else {
                                theme::TEXT
                            }));
                        });
                    ui.add_space(6.0);
                }
            });
        // Wake up again to expire the oldest one.
        if let Some((t, _, error)) = self.items.first() {
            let life = if *error {
                TOAST_LIFETIME * 2
            } else {
                TOAST_LIFETIME
            };
            ctx.request_repaint_after(life.saturating_sub(now - *t));
        }
    }
}
