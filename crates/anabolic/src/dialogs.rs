// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Modal confirmations and transient notifications.

use std::time::{Duration, Instant};

use anabolic_core::actions::Target;
use eframe::egui::{self, Align2, Frame, Id, Key, Margin, Modal, RichText, Stroke, vec2};

use crate::proc_table::UiCmd;
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
            ui.label(
                RichText::new("Unsaved data in these processes will be lost.")
                    .color(theme::TEXT_WEAK),
            );
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
                                "{} is a critical system process. Terminating it will crash {}{}.",
                                self.critical.join(", "),
                                crate::platform::SYSTEM,
                                if cfg!(windows) { " (bugcheck)" } else { "" }
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
                let kill =
                    egui::Button::new(widgets::strong("Terminate", theme::BODY).color(theme::TEXT))
                        .fill(theme::BAD_STRONG);
                if ui.add(kill).clicked()
                    || (self.critical.is_empty() && ui.input(|i| i.key_pressed(Key::Enter)))
                {
                    choice = KillChoice::Confirm {
                        dont_ask_again: self.dont_ask && self.critical.is_empty(),
                    };
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

/// Run new task: a program, document, folder or web address to start.
pub struct RunDialog {
    pub line: String,
    /// With administrator rights (Windows).
    pub elevated: bool,
    /// Put the cursor in the line on the next frame.
    pub focus: bool,
    /// Windows' Open dialog is up.
    pub browsing: bool,
}

pub enum RunChoice {
    Pending,
    Run,
    Browse,
    Cancel,
}

impl RunDialog {
    pub fn new(elevated: bool) -> Self {
        Self {
            line: String::new(),
            elevated,
            focus: true,
            browsing: false,
        }
    }

    /// `history` is what ran lately; `anabolic_elevated` says what the
    /// checkbox changes.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        history: &[String],
        anabolic_elevated: bool,
    ) -> RunChoice {
        let mut choice = RunChoice::Pending;
        let response = Modal::new(Id::new("run-dialog")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.label(widgets::strong("Run new task", theme::SUBTITLE));
            ui.add_space(4.0);
            ui.label(
                RichText::new(if cfg!(windows) {
                    "The name of a program, folder, document or web address, as in Windows' Run \
                     box. Arguments go after the program."
                } else {
                    "A command line, as a terminal would run it, or a folder, document or web \
                     address to open."
                })
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.line)
                        .hint_text(if cfg!(windows) {
                            "notepad, cmd /k dir, C:\\Projects, https://…"
                        } else {
                            "gnome-text-editor, htop, ~/Projects, https://…"
                        })
                        .desired_width(if cfg!(windows) { 330.0 } else { 410.0 }),
                );
                if std::mem::take(&mut self.focus) {
                    field.request_focus();
                }
                if field.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter))
                    && !self.line.trim().is_empty()
                {
                    choice = RunChoice::Run;
                }
                if !history.is_empty() {
                    ui.menu_button(widgets::icon(theme::icon::CHEVRON_DOWN, 10.0), |ui| {
                        ui.label(
                            RichText::new("Lately")
                                .size(theme::META)
                                .color(theme::TEXT_WEAK),
                        );
                        for line in history {
                            if ui.button(line.as_str()).clicked() {
                                self.line = line.clone();
                                self.focus = true;
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("What ran lately");
                }
                if cfg!(windows)
                    && ui
                        .add_enabled(!self.browsing, egui::Button::new("Browse\u{2026}"))
                        .clicked()
                {
                    choice = RunChoice::Browse;
                }
            });
            if cfg!(windows) {
                ui.add_space(6.0);
                ui.checkbox(&mut self.elevated, "Run as administrator")
                    .on_hover_text(if anabolic_elevated {
                        "Unticked, it runs as you do, without administrator rights, as if started \
                         from Explorer."
                    } else {
                        "Windows asks at its UAC prompt first."
                    });
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.line.trim().is_empty(),
                        egui::Button::new(widgets::strong("Run", theme::BODY)),
                    )
                    .clicked()
                {
                    choice = RunChoice::Run;
                }
                if ui.button("Cancel").clicked() {
                    choice = RunChoice::Cancel;
                }
            });
        });
        if matches!(choice, RunChoice::Pending) && response.should_close() && !self.browsing {
            choice = RunChoice::Cancel;
        }
        choice
    }
}

const TOAST_LIFETIME: Duration = Duration::from_secs(5);

struct Toast {
    at: Instant,
    text: String,
    /// What the user can do about it.
    next: Option<String>,
    error: bool,
    /// A button, and what it asks for; taken when clicked.
    action: Option<(String, UiCmd)>,
}

impl Toast {
    /// Errors, and toasts with something to click, stay longer.
    fn life(&self) -> Duration {
        if self.error || self.action.is_some() {
            TOAST_LIFETIME * 2
        } else {
            TOAST_LIFETIME
        }
    }
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    fn push(
        &mut self,
        text: String,
        next: Option<String>,
        error: bool,
        action: Option<(String, UiCmd)>,
    ) {
        self.items.push(Toast {
            at: Instant::now(),
            text,
            next,
            error,
            action,
        });
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(text.into(), None, false, None);
    }

    /// News with a button beside it (say, "Show in folder").
    pub fn info_with(&mut self, text: impl Into<String>, label: &str, cmd: UiCmd) {
        self.push(text.into(), None, false, Some((label.to_string(), cmd)));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(text.into(), None, true, None);
    }

    /// An error and, below it, what the user can do about it.
    pub fn error_with(&mut self, text: impl Into<String>, next: Option<String>) {
        self.push(text.into(), next, true, None);
    }

    /// Draws the toasts; a clicked button's request goes to `out`.
    pub fn show(&mut self, ctx: &egui::Context, out: &mut Vec<UiCmd>) {
        let now = Instant::now();
        self.items.retain(|t| now - t.at < t.life());
        if self.items.is_empty() {
            return;
        }
        let clickable = self.items.iter().any(|t| t.action.is_some());
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -34.0))
            .order(egui::Order::Foreground)
            .interactable(clickable)
            .show(ctx, |ui| {
                for toast in &mut self.items {
                    Frame::new()
                        .fill(theme::RAISED)
                        .stroke(Stroke::new(
                            1.0,
                            if toast.error {
                                theme::BAD
                            } else {
                                theme::BORDER
                            },
                        ))
                        .inner_margin(Margin::symmetric(12, 8))
                        .corner_radius(6)
                        .show(ui, |ui| {
                            ui.set_max_width(460.0);
                            ui.label(RichText::new(&toast.text).color(if toast.error {
                                theme::BAD
                            } else {
                                theme::TEXT
                            }));
                            if let Some(next) = &toast.next {
                                ui.label(RichText::new(next).color(theme::TEXT_WEAK));
                            }
                            if let Some((label, _)) = &toast.action
                                && ui.button(label.as_str()).clicked()
                                && let Some((_, cmd)) = toast.action.take()
                            {
                                out.push(cmd);
                            }
                        });
                    ui.add_space(6.0);
                }
            });
        // Wake up again to expire the oldest one.
        if let Some(t) = self.items.first() {
            ctx.request_repaint_after(t.life().saturating_sub(now - t.at));
        }
    }
}
