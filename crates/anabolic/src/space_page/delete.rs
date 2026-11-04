// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Deleting from the Disk space page: to the Recycle Bin (the desktop's
//! trash on Linux) or for good, always after a question, never the folder
//! that was scanned, NTFS's own files or the system's. The deletion runs on
//! a thread of its own; once it is done the tree takes the size out of
//! every folder above, so nothing has to be scanned again.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anabolic_core::space::{self, ExtTotal, META, NONE, ORPHANS, ROOT};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Id, Key, Margin, Modal, RichText, Stroke, Ui};

use super::Data;
use crate::{fmt, theme, widgets};

const TRASH: &str = if cfg!(windows) {
    "the Recycle Bin"
} else {
    "the trash"
};

struct Running {
    node: u32,
    name: String,
    size: u64,
    recycle: bool,
    rx: Receiver<Result<(), String>>,
}

#[derive(Default)]
pub struct Deleter {
    /// The question on screen: (node, to the Recycle Bin).
    confirm: Option<(u32, bool)>,
    running: Option<Running>,
    /// Deleted, waiting for the tree to be free to change.
    pending: Vec<u32>,
    exts: Option<Receiver<(u64, Vec<ExtTotal>)>>,
    /// The last outcome: (text, whether it failed).
    notice: Option<(String, bool)>,
    /// The window, which owns Windows' own questions.
    pub owner: isize,
}

impl Deleter {
    /// Asked from a menu or a key: refused at once, or asked about.
    pub fn ask(&mut self, data: &Data, node: u32, recycle: bool) {
        let refusal = if self.running.is_some() {
            Some("One deletion at a time: the last one is still going.".to_string())
        } else if node == ROOT {
            Some(
                "The folder that was scanned is not deleted from here: choose what is in it."
                    .into(),
            )
        } else if data.tree.flags(node) & META != 0 {
            Some("NTFS keeps that file for itself.".into())
        } else if data.tree.flags(node) & ORPHANS != 0 {
            Some(
                "That is not a folder on the drive, only files the table lists without one.".into(),
            )
        } else if data.tree.is_gone(node) {
            Some("Already deleted.".into())
        } else {
            space::protected(&data.tree.path_of(node)).map(str::to_string)
        };
        match refusal {
            Some(why) => self.notice = Some((why, true)),
            None => self.confirm = Some((node, recycle)),
        }
    }

    /// A line under the scan's summary: what is being deleted, or how the
    /// last deletion went.
    pub fn status(&self, ui: &mut Ui) {
        if let Some(r) = &self.running {
            ui.horizontal(|ui| {
                ui.spinner();
                let how = if r.recycle {
                    format!("Moving {} to {TRASH}…", r.name)
                } else {
                    format!("Deleting {}…", r.name)
                };
                ui.label(RichText::new(how).color(theme::TEXT_WEAK));
            });
        } else if let Some((text, failed)) = &self.notice {
            let color = if *failed { theme::BAD } else { theme::GOOD };
            ui.label(RichText::new(text).color(color));
        }
    }

    /// Each frame: the question, the answer, and the tree catching up.
    pub fn frame(&mut self, ctx: &egui::Context, data: &mut Data) {
        if let Some((node, recycle)) = self.confirm {
            match question(ctx, data, node, recycle) {
                Some(true) => {
                    self.confirm = None;
                    self.start(ctx, data, node, recycle);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
        if let Some(r) = &self.running {
            match r.rx.try_recv() {
                Ok(Ok(())) => {
                    let to = if r.recycle {
                        format!(" to {TRASH}")
                    } else {
                        String::new()
                    };
                    self.notice = Some((
                        format!(
                            "{} {}{to}: {} freed.",
                            if r.recycle { "Moved" } else { "Deleted" },
                            r.name,
                            fmt::bytes(r.size)
                        ),
                        false,
                    ));
                    self.pending.push(r.node);
                    self.running = None;
                }
                Ok(Err(e)) => {
                    self.notice = Some((
                        format!("{e} Scan again to see what is left of {}.", r.name),
                        true,
                    ));
                    self.running = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.running = None,
            }
        }
        if !self.pending.is_empty() {
            // The tree is shared with the thread drawing the map, and with
            // the one counting extensions: it changes once neither has it.
            match Arc::get_mut(&mut data.tree) {
                Some(tree) => {
                    for n in self.pending.drain(..) {
                        tree.remove(n);
                        if data.selected.is_some_and(|s| tree.is_gone(s)) {
                            data.selected = Some(tree.parent(n)).filter(|&p| p != NONE);
                        }
                        while data.zoom != ROOT && tree.is_gone(data.zoom) {
                            data.zoom = tree.parent(data.zoom);
                        }
                    }
                    data.version += 1;
                    let (tx, rx) = bounded(1);
                    let (tree, version, ctx) = (data.tree.clone(), data.version, ctx.clone());
                    let _ = std::thread::Builder::new()
                        .name("space-types".into())
                        .spawn(move || {
                            let _ = tx.send((version, tree.by_extension()));
                            ctx.request_repaint();
                        });
                    self.exts = Some(rx);
                }
                None => ctx.request_repaint_after(Duration::from_millis(50)),
            }
        }
        if let Some(rx) = &self.exts
            && let Ok((version, exts)) = rx.try_recv()
        {
            if version == data.version {
                data.exts = exts;
            }
            self.exts = None;
        }
    }

    fn start(&mut self, ctx: &egui::Context, data: &Data, node: u32, recycle: bool) {
        let path: PathBuf = data.tree.path_of(node);
        let (tx, rx) = bounded(1);
        let (owner, ctx, at) = (self.owner, ctx.clone(), path.clone());
        let spawned = std::thread::Builder::new()
            .name("space-delete".into())
            .spawn(move || {
                let _ = tx.send(space::delete(&at, recycle, owner));
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.notice = Some((format!("Could not start deleting: {e}"), true));
            return;
        }
        self.notice = None;
        self.running = Some(Running {
            node,
            name: data.tree.name(node).to_string(),
            size: data.tree.size(node),
            recycle,
            rx,
        });
    }
}

/// Asks before deleting: Some(true) to go ahead, Some(false) not to,
/// None while the question stands.
fn question(ctx: &egui::Context, data: &Data, node: u32, recycle: bool) -> Option<bool> {
    let n = data.tree.node(node);
    let path = data.path(node);
    let mut answer = None;
    let response = Modal::new(Id::new("space-delete")).show(ctx, |ui| {
        ui.set_width(480.0);
        let title = if recycle {
            format!("Move to {TRASH}?")
        } else {
            "Delete for good?".to_string()
        };
        ui.label(widgets::strong(title, theme::SUBTITLE));
        ui.add_space(6.0);
        ui.label(RichText::new(&path).monospace());
        let what = if n.is_dir() {
            format!(
                "{}  ·  {} files",
                fmt::bytes(n.size),
                fmt::count(u64::from(n.files))
            )
        } else {
            fmt::bytes(n.size)
        };
        ui.label(RichText::new(what).color(theme::TEXT_WEAK));
        ui.add_space(6.0);
        if recycle {
            let note = if cfg!(windows) {
                "It can be brought back from the Recycle Bin. Something too large for it is \
                 deleted for good only after Windows asks."
            } else {
                "It can be brought back from the trash."
            };
            ui.label(RichText::new(note).color(theme::TEXT_WEAK));
        } else {
            egui::Frame::new()
                .fill(theme::BAD_FILL)
                .stroke(Stroke::new(1.0, theme::BAD))
                .inner_margin(Margin::same(8))
                .corner_radius(4)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "It does not go to {TRASH}: it cannot be brought back."
                        ))
                        .color(theme::BAD),
                    );
                });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let (text, fill) = if recycle {
                (format!("Move to {TRASH}"), theme::BUTTON)
            } else {
                ("Delete for good".to_string(), theme::BAD_STRONG)
            };
            let go =
                egui::Button::new(widgets::strong(text, theme::BODY).color(theme::TEXT)).fill(fill);
            // Enter answers only the question that can be undone.
            if ui.add(go).clicked() || (recycle && ui.input(|i| i.key_pressed(Key::Enter))) {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && response.should_close() {
        answer = Some(false);
    }
    answer
}
