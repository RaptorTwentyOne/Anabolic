// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Deleting from the Disk space page: to the Recycle Bin (the desktop's
//! trash on Linux) or for good, always after a question, never the folder
//! that was scanned, NTFS's own files or the system's. One file or folder,
//! or a clean-up suggestion's many at once. The deletion runs on a thread
//! of its own; once it is done each path is asked whether it is gone, and
//! the tree takes those out of every folder above, so nothing has to be
//! scanned again.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anabolic_core::space::clean::{Level, Suggestion};
use anabolic_core::space::{self, ExtTotal, META, NONE, ORPHANS, ROOT, Tree};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Id, Key, Margin, Modal, RichText, Stroke, Ui};

use super::Data;
use crate::{fmt, theme, widgets};

const TRASH: &str = if cfg!(windows) {
    "the Recycle Bin"
} else {
    "the trash"
};

/// What one question is about.
#[derive(Clone)]
struct Job {
    /// What it is called in the question and the notice.
    name: String,
    nodes: Vec<u32>,
    recycle: bool,
    /// The Recycle Bin is emptied with its own command.
    bin: bool,
}

struct Running {
    job: Job,
    /// (the nodes now gone, why some are not)
    rx: Receiver<(Vec<u32>, Option<String>)>,
}

/// What a background count hands back after a deletion.
type Recount = (u64, Vec<ExtTotal>, Vec<Suggestion>);

#[derive(Default)]
pub struct Deleter {
    confirm: Option<Job>,
    running: Option<Running>,
    /// Deleted, waiting for the tree to be free to change.
    pending: Vec<u32>,
    recount: Option<Receiver<Recount>>,
    /// The last outcome: (text, whether it failed).
    notice: Option<(String, bool)>,
    /// The window, which owns Windows' own questions.
    pub owner: isize,
}

/// Why a node is not deleted from here, if it is not.
fn refusal(tree: &Tree, node: u32) -> Option<String> {
    if node == ROOT {
        Some("The folder that was scanned is not deleted from here: choose what is in it.".into())
    } else if tree.flags(node) & META != 0 {
        Some("NTFS keeps that file for itself.".into())
    } else if tree.flags(node) & ORPHANS != 0 {
        Some("That is not a folder on the drive, only files the table lists without one.".into())
    } else if tree.is_gone(node) {
        Some("Already deleted.".into())
    } else {
        space::protected(&tree.path_of(node)).map(str::to_string)
    }
}

impl Deleter {
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Asked from a menu or a key: refused at once, or asked about.
    pub fn ask(&mut self, data: &Data, node: u32, recycle: bool) {
        let why = if self.busy() {
            Some("One deletion at a time: the last one is still going.".to_string())
        } else {
            refusal(&data.tree, node)
        };
        match why {
            Some(why) => self.notice = Some((why, true)),
            None => {
                self.confirm = Some(Job {
                    name: data.tree.name(node).to_string(),
                    nodes: vec![node],
                    recycle,
                    bin: false,
                });
            }
        }
    }

    /// A clean-up suggestion's places, or what is inside them.
    pub fn ask_suggestion(&mut self, data: &Data, s: &Suggestion) {
        if self.busy() {
            self.notice = Some((
                "One deletion at a time: the last one is still going.".into(),
                true,
            ));
            return;
        }
        let tree = &data.tree;
        let mut nodes: Vec<u32> = if s.bin {
            // What is in each person's part of the Recycle Bin.
            s.nodes
                .iter()
                .flat_map(|&n| tree.children(n))
                .flat_map(|n| tree.children(n))
                .collect()
        } else if s.contents {
            s.nodes.iter().flat_map(|&n| tree.children(n)).collect()
        } else {
            s.nodes.clone()
        };
        nodes.retain(|&n| {
            tree.size(n) > 0 && !tree.is_gone(n) && (s.bin || refusal(tree, n).is_none())
        });
        if nodes.is_empty() {
            self.notice = Some(("Nothing there to delete any more.".into(), true));
            return;
        }
        self.confirm = Some(Job {
            name: s.title.to_string(),
            nodes,
            // Caches go for good: the Recycle Bin would only keep them.
            recycle: s.level == Level::Review && !s.bin,
            bin: s.bin,
        });
    }

    /// A line under the scan's summary: what is being deleted, or how the
    /// last deletion went.
    pub fn status(&self, ui: &mut Ui) {
        if let Some(r) = &self.running {
            ui.horizontal(|ui| {
                ui.spinner();
                let how = if r.job.bin {
                    format!("Emptying {TRASH}…")
                } else if r.job.recycle {
                    format!("Moving {} to {TRASH}…", r.job.name)
                } else {
                    format!("Deleting {}…", r.job.name)
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
        if let Some(job) = &self.confirm {
            match question(ctx, data, job) {
                Some(true) => {
                    let job = self.confirm.take().expect("asked");
                    self.start(ctx, data, job);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
        if let Some(r) = &self.running {
            match r.rx.try_recv() {
                Ok((gone, why)) => {
                    let freed: u64 = gone.iter().map(|&n| data.tree.size(n)).sum();
                    let job = &r.job;
                    let all = gone.len() == job.nodes.len();
                    let what = if job.nodes.len() > 1 {
                        format!(
                            "{} ({} items)",
                            job.name,
                            fmt::count(job.nodes.len() as u64)
                        )
                    } else {
                        job.name.clone()
                    };
                    let done = if job.bin {
                        format!("Emptied {TRASH}")
                    } else if job.recycle {
                        format!("Moved {what} to {TRASH}")
                    } else {
                        format!("Deleted {what}")
                    };
                    self.notice = Some(if gone.is_empty() {
                        (why.unwrap_or_else(|| "Nothing was deleted.".into()), true)
                    } else if all {
                        (format!("{done}: {} freed.", fmt::bytes(freed)), false)
                    } else {
                        (
                            format!(
                                "{done}: {} freed; {} of {} could not be deleted ({}).",
                                fmt::bytes(freed),
                                fmt::count((job.nodes.len() - gone.len()) as u64),
                                fmt::count(job.nodes.len() as u64),
                                why.unwrap_or_else(|| "in use, or no right to".into())
                                    .trim_end_matches('.')
                            ),
                            true,
                        )
                    });
                    self.pending.extend(gone);
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
            // the one counting it again: it changes once neither has it.
            match Arc::get_mut(&mut data.tree) {
                Some(tree) => {
                    for n in self.pending.drain(..) {
                        if tree.is_gone(n) {
                            continue;
                        }
                        tree.remove(n);
                        if data.selected.is_some_and(|s| tree.is_gone(s)) {
                            data.selected = Some(tree.parent(n)).filter(|&p| p != NONE);
                        }
                        while data.zoom != ROOT && tree.is_gone(data.zoom) {
                            data.zoom = tree.parent(data.zoom);
                        }
                    }
                    data.version += 1;
                    self.recount = None;
                    self.recount(ctx, data);
                }
                None => ctx.request_repaint_after(Duration::from_millis(50)),
            }
        }
        if let Some(rx) = &self.recount
            && let Ok((version, exts, clean)) = rx.try_recv()
        {
            if version == data.version {
                data.exts = exts;
                data.clean = clean;
            }
            self.recount = None;
        }
    }

    /// The file types and the clean-up counted again, on a thread, after
    /// the tree changed.
    pub fn recount(&mut self, ctx: &egui::Context, data: &Data) {
        if self.recount.is_some() {
            return;
        }
        let (tx, rx) = bounded(1);
        let (tree, version, ctx) = (data.tree.clone(), data.version, ctx.clone());
        let _ = std::thread::Builder::new()
            .name("space-recount".into())
            .spawn(move || {
                let exts = tree.by_extension();
                let clean = space::clean::suggest(&tree);
                let _ = tx.send((version, exts, clean));
                ctx.request_repaint();
            });
        self.recount = Some(rx);
    }

    fn start(&mut self, ctx: &egui::Context, data: &Data, job: Job) {
        let paths: Vec<(u32, PathBuf)> = job
            .nodes
            .iter()
            .map(|&n| (n, data.tree.path_of(n)))
            .collect();
        let root = data.tree.path.clone();
        let (tx, rx) = bounded(1);
        let (owner, ctx, recycle, bin) = (self.owner, ctx.clone(), job.recycle, job.bin);
        let spawned = std::thread::Builder::new()
            .name("space-delete".into())
            .spawn(move || {
                let done = if bin {
                    space::empty_recycle_bin(&root, owner)
                } else {
                    let only: Vec<PathBuf> = paths.iter().map(|(_, p)| p.clone()).collect();
                    space::delete_all(&only, recycle, owner)
                };
                let gone = paths
                    .iter()
                    .filter(|(_, p)| std::fs::symlink_metadata(p).is_err())
                    .map(|(n, _)| *n)
                    .collect();
                let _ = tx.send((gone, done.err()));
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.notice = Some((format!("Could not start deleting: {e}"), true));
            return;
        }
        self.notice = None;
        self.running = Some(Running { job, rx });
    }
}

/// Asks before deleting: Some(true) to go ahead, Some(false) not to,
/// None while the question stands.
fn question(ctx: &egui::Context, data: &Data, job: &Job) -> Option<bool> {
    let tree = &data.tree;
    let size: u64 = job.nodes.iter().map(|&n| tree.size(n)).sum();
    let mut answer = None;
    let response = Modal::new(Id::new("space-delete")).show(ctx, |ui| {
        ui.set_width(520.0);
        let title = if job.bin {
            format!("Empty {TRASH}?")
        } else if job.recycle {
            format!("Move to {TRASH}?")
        } else {
            "Delete for good?".to_string()
        };
        ui.label(widgets::strong(title, theme::SUBTITLE));
        ui.add_space(6.0);
        if job.nodes.len() == 1 {
            let n = tree.node(job.nodes[0]);
            ui.label(RichText::new(data.path(job.nodes[0])).monospace());
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
        } else {
            ui.label(format!(
                "{}: {} items, {}",
                job.name,
                fmt::count(job.nodes.len() as u64),
                fmt::bytes(size)
            ));
            for &n in job.nodes.iter().take(5) {
                ui.label(
                    RichText::new(format!("{}  {}", fmt::bytes(tree.size(n)), data.path(n)))
                        .small()
                        .monospace()
                        .color(theme::TEXT_WEAK),
                );
            }
            if job.nodes.len() > 5 {
                ui.label(
                    RichText::new(format!(
                        "and {} more",
                        fmt::count(job.nodes.len() as u64 - 5)
                    ))
                    .small()
                    .color(theme::TEXT_WEAK),
                );
            }
        }
        ui.add_space(6.0);
        if job.recycle {
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
                    let text = if job.bin {
                        "What is in it is deleted for good.".to_string()
                    } else {
                        format!("It does not go to {TRASH}: it cannot be brought back.")
                    };
                    ui.label(RichText::new(text).color(theme::BAD));
                });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let (text, fill) = if job.bin {
                (format!("Empty {TRASH}"), theme::BAD_STRONG)
            } else if job.recycle {
                (format!("Move to {TRASH}"), theme::BUTTON)
            } else {
                ("Delete for good".to_string(), theme::BAD_STRONG)
            };
            let go =
                egui::Button::new(widgets::strong(text, theme::BODY).color(theme::TEXT)).fill(fill);
            // Enter answers only the question that can be undone.
            if ui.add(go).clicked() || (job.recycle && ui.input(|i| i.key_pressed(Key::Enter))) {
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
