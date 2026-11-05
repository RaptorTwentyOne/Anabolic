// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What changes after the scan: NTFS's change journal says which files
//! were made, written, moved or deleted, a thread of its own reads their
//! sizes, and the tree follows, so the map and the lists stay true without
//! scanning again. The Live tab lists what changed, and who wrote it: the
//! disk trace, on while the tab is open, names the process behind each
//! file it sees written.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::Snapshot;
use anabolic_core::space::{Batch, Change, ChangeKind, Tree, Watch};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use super::Data;
use crate::table::{Columns, Spec};
use crate::{fmt, theme, widgets};

/// How often the map and the lists are redrawn for changes at most: a
/// busy drive changes every second.
const REDRAW: Duration = Duration::from_secs(2);
/// Changes listed at most; the smallest go first.
const MOST: usize = 20_000;

const TITLES: [(&str, bool); 6] = [
    ("Name", false),
    ("Change", true),
    ("Now", true),
    ("Written by", false),
    ("When", false),
    ("Folder", false),
];
const COLUMNS: [Spec; 6] = [
    Spec::new(240.0).clip(),
    Spec::new(100.0),
    Spec::new(90.0),
    Spec::new(140.0).clip(),
    Spec::new(70.0),
    Spec::new(360.0).clip(),
];

/// One changed file.
struct Entry {
    name: String,
    folder: String,
    /// Its size at the scan (0 when made since).
    before: u64,
    now: u64,
    at: i64,
    gone: bool,
}

impl Entry {
    fn change(&self) -> i64 {
        self.now as i64 - self.before as i64
    }
}

#[derive(Default)]
pub struct Live {
    watch: Option<Watch>,
    pending: Vec<Change>,
    /// Files made since the scan, by reference: (the folder they count in,
    /// size, alloc).
    loose: HashMap<u64, (u32, u64, u64)>,
    /// Folders made since: the folder their contents count in.
    loose_dirs: HashMap<u64, u32>,
    entries: HashMap<u64, Entry>,
    /// Who wrote which file, from the disk trace: lower-case path → process.
    writers: HashMap<String, String>,
    lost: Option<String>,
    /// Changes not drawn yet, and when the last were.
    dirty: bool,
    drawn: Option<Instant>,
    rows: Vec<u64>,
    rows_stale: bool,
    /// The tab was on screen last frame.
    pub open: bool,
}

impl Live {
    /// Starts following a new tree's drive, when it was read from its
    /// master file table.
    pub fn start(&mut self, ctx: &egui::Context, tree: &Tree) {
        *self = Live::default();
        let ctx = ctx.clone();
        self.watch = anabolic_core::space::watch(tree, Box::new(move || ctx.request_repaint()));
    }

    pub fn following(&self) -> bool {
        self.watch.is_some()
    }

    /// Picks up what the journal said and puts it in the tree once nothing
    /// else holds it; true when the map and the lists should be redrawn.
    pub fn poll(&mut self, ctx: &egui::Context, data: &mut Data, snap: &Snapshot) -> bool {
        if let Some(w) = &self.watch {
            while let Ok(batch) = w.rx.try_recv() {
                match batch {
                    Batch::Changes(c) => self.pending.extend(c),
                    Batch::Lost(why) => self.lost = Some(why),
                }
            }
        }
        if self.open {
            if self.writers.len() > 50_000 {
                self.writers.clear();
            }
            for disk in &snap.system.disk_activity {
                // System writes back what programs left in the file cache:
                // it says nothing about who wrote it.
                for f in disk
                    .files
                    .iter()
                    .filter(|f| f.io.write_total > 0 && f.pid != 4 && f.pid != 0)
                {
                    self.writers
                        .entry(f.path.to_lowercase())
                        .or_insert_with(|| f.process.to_string());
                }
            }
        }
        if !self.pending.is_empty() {
            match Arc::get_mut(&mut data.tree) {
                Some(tree) => {
                    for c in std::mem::take(&mut self.pending) {
                        self.apply(tree, &c);
                    }
                    self.dirty = true;
                    self.rows_stale = true;
                    if self.entries.len() > MOST {
                        let mut by: Vec<(u64, u64)> = self
                            .entries
                            .iter()
                            .map(|(&k, e)| (k, e.change().unsigned_abs()))
                            .collect();
                        by.sort_unstable_by_key(|&(_, c)| std::cmp::Reverse(c));
                        for (k, _) in by.drain(MOST..) {
                            self.entries.remove(&k);
                        }
                    }
                }
                None => ctx.request_repaint_after(Duration::from_millis(50)),
            }
        }
        if self.dirty {
            if self.drawn.is_none_or(|t| t.elapsed() >= REDRAW) {
                self.dirty = false;
                self.drawn = Some(Instant::now());
                data.version += 1;
                return true;
            }
            ctx.request_repaint_after(REDRAW);
        }
        false
    }

    /// The folder new files in `parent` count in: the folder itself, or
    /// the scanned folder a new one is in.
    fn anchor(&self, tree: &Tree, parent: u64) -> Option<u32> {
        tree.node_of_record(parent)
            .filter(|&n| tree.is_dir(n))
            .or_else(|| self.loose_dirs.get(&parent).copied())
    }

    fn note(&mut self, record: u64, name: &str, folder: String, before: u64, c: &Change) {
        let e = self.entries.entry(record).or_insert_with(|| Entry {
            name: name.to_string(),
            folder,
            before,
            now: before,
            at: c.at,
            gone: false,
        });
        e.now = c.size;
        e.at = c.at;
        e.gone = c.kind == ChangeKind::Deleted;
        if !name.is_empty() {
            e.name = name.to_string();
        }
    }

    fn folder_path(&self, tree: &Tree, parent: u64, anchor: u32) -> String {
        let path = tree.path_of(anchor).to_string_lossy().into_owned();
        if tree.node_of_record(parent).is_some() {
            path
        } else {
            // In a folder made since the scan.
            format!("{path}{}…", std::path::MAIN_SEPARATOR)
        }
    }

    fn apply(&mut self, tree: &mut Tree, c: &Change) {
        let node = tree.node_of_record(c.record);
        match c.kind {
            ChangeKind::Written | ChangeKind::Moved if c.dir => {
                // A folder's own size is what is in it: only where it is
                // matters, for what will be made in it.
                if node.is_none()
                    && let Some(anchor) = self.anchor(tree, c.parent)
                {
                    self.loose_dirs.insert(c.record, anchor);
                }
                if let (Some(n), ChangeKind::Moved) = (node, c.kind) {
                    let to = self.anchor(tree, c.parent);
                    if to != Some(tree.parent(n)) {
                        let moved = tree.node(n);
                        tree.remove(n);
                        if let Some(anchor) = to {
                            tree.grow(
                                anchor,
                                moved.size as i64,
                                moved.alloc as i64,
                                moved.files as i32,
                            );
                            self.loose_dirs.insert(c.record, anchor);
                        }
                    }
                }
            }
            ChangeKind::Written | ChangeKind::Moved => {
                if let Some(n) = node {
                    let to = self.anchor(tree, c.parent);
                    if c.kind == ChangeKind::Moved && to != Some(tree.parent(n)) {
                        // Moved out of its folder: gone from there, counted
                        // where it went.
                        let before = tree.size(n);
                        tree.remove(n);
                        if let Some(anchor) = to {
                            tree.grow(anchor, c.size as i64, c.alloc as i64, 1);
                            self.loose.insert(c.record, (anchor, c.size, c.alloc));
                            let folder = self.folder_path(tree, c.parent, anchor);
                            self.note(c.record, &c.name, folder, before, c);
                        }
                        return;
                    }
                    let before = tree.size(n);
                    let folder = tree.path_of(tree.parent(n)).to_string_lossy().into_owned();
                    tree.resize(n, c.size, c.alloc);
                    let name = tree.name(n).to_string();
                    self.note(c.record, &name, folder, before, c);
                } else if let Some(&(anchor, size, alloc)) = self.loose.get(&c.record) {
                    tree.grow(
                        anchor,
                        c.size as i64 - size as i64,
                        c.alloc as i64 - alloc as i64,
                        0,
                    );
                    self.loose.insert(c.record, (anchor, c.size, c.alloc));
                    let folder = self.folder_path(tree, c.parent, anchor);
                    self.note(c.record, &c.name, folder, 0, c);
                } else if let Some(anchor) = self.anchor(tree, c.parent) {
                    tree.grow(anchor, c.size as i64, c.alloc as i64, 1);
                    self.loose.insert(c.record, (anchor, c.size, c.alloc));
                    let folder = self.folder_path(tree, c.parent, anchor);
                    self.note(c.record, &c.name, folder, 0, c);
                }
            }
            ChangeKind::Deleted => {
                if let Some(n) = node {
                    let before = tree.size(n);
                    let folder = tree.path_of(tree.parent(n)).to_string_lossy().into_owned();
                    let name = tree.name(n).to_string();
                    tree.remove(n);
                    if !c.dir {
                        self.note(c.record, &name, folder, before, c);
                    }
                } else if let Some((anchor, size, alloc)) = self.loose.remove(&c.record) {
                    tree.grow(anchor, -(size as i64), -(alloc as i64), -1);
                    let folder = self.folder_path(tree, c.parent, anchor);
                    self.note(c.record, &c.name, folder, 0, c);
                }
                self.loose_dirs.remove(&c.record);
            }
        }
    }

    fn writer(&self, e: &Entry) -> Option<&str> {
        let path = format!("{}{}{}", e.folder, std::path::MAIN_SEPARATOR, e.name);
        self.writers.get(&path.to_lowercase()).map(String::as_str)
    }
}

fn clock(secs: i64) -> String {
    anabolic_core::sysinfo::local_time(secs * 10_000_000 + 116_444_736_000_000_000)
        .map(|t| t.get(11..19).unwrap_or("").to_string())
        .unwrap_or_default()
}

fn signed(change: i64) -> String {
    match change {
        0 => "0".into(),
        c if c > 0 => format!("+{}", fmt::bytes(c as u64)),
        c => format!("−{}", fmt::bytes(c.unsigned_abs())),
    }
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

pub fn show(ui: &mut Ui, live: &mut Live, data: &Data) {
    if !live.following() {
        ui.add_space(6.0);
        let why = if cfg!(windows) {
            "Following what changes needs the drive read from its master file table, which \
             Anabolic does as administrator on NTFS. Scan again to see what changed."
        } else {
            "Linux keeps no change journal like NTFS's: scan again to see what changed."
        };
        ui.label(RichText::new(why).color(theme::TEXT_WEAK));
        return;
    }
    let (mut grew, mut shrank, mut made) = (0u64, 0u64, 0usize);
    for e in live.entries.values() {
        match e.change() {
            c if c > 0 => grew += c as u64,
            c => shrank += c.unsigned_abs(),
        }
        if e.before == 0 && !e.gone {
            made += 1;
        }
    }
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "Since the scan at {}: {} more, {} less, in {} files ({} new).",
                clock(data.scanned_at),
                fmt::bytes(grew),
                fmt::bytes(shrank),
                fmt::count(live.entries.len() as u64),
                fmt::count(made as u64),
            ))
            .color(theme::TEXT_WEAK),
        );
        if ui
            .small_button("Clear")
            .on_hover_text("Start the list again from now")
            .clicked()
        {
            live.entries.clear();
            live.rows_stale = true;
        }
    });
    // Who added the most.
    let mut by: HashMap<&str, u64> = HashMap::new();
    for e in live.entries.values().filter(|e| e.change() > 0) {
        if let Some(w) = live.writer(e) {
            *by.entry(w).or_default() += e.change() as u64;
        }
    }
    let mut by: Vec<(&str, u64)> = by.into_iter().collect();
    by.sort_unstable_by_key(|&(_, b)| std::cmp::Reverse(b));
    let writers = if by.is_empty() {
        "Who wrote a file shows here when the disk trace sees the program write it; what Windows \
         writes back from its file cache later has no writer to show."
            .to_string()
    } else {
        by.iter()
            .take(5)
            .map(|(w, b)| format!("{w} +{}", fmt::bytes(*b)))
            .collect::<Vec<_>>()
            .join("  ·  ")
    };
    ui.label(RichText::new(writers).small().color(theme::TEXT));
    if let Some(lost) = &live.lost {
        ui.label(RichText::new(lost).small().color(theme::ELEVATED_TEXT));
    }
    if live.rows_stale {
        live.rows = live.entries.keys().copied().collect();
        let entries = &live.entries;
        live.rows
            .sort_unstable_by_key(|k| std::cmp::Reverse(entries[k].change().unsigned_abs()));
        live.rows_stale = false;
    }
    if live.rows.is_empty() {
        ui.label(
            RichText::new("Nothing has changed in it since the scan.").color(theme::TEXT_WEAK),
        );
        return;
    }
    Columns::load(ui.ctx(), "space-live", &COLUMNS).show(ui, |table, columns| {
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
                body.rows(widgets::ROW, live.rows.len(), |mut row| {
                    let Some(e) = live.entries.get(&live.rows[row.index()]) else {
                        return;
                    };
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                let mut text = RichText::new(&e.name);
                                if e.gone {
                                    text = text.strikethrough().color(theme::TEXT_DIM);
                                }
                                ui.add(Label::new(text).truncate().selectable(false));
                            }
                            1 => {
                                let change = e.change();
                                let color = if change > 0 {
                                    theme::ELEVATED_TEXT
                                } else {
                                    theme::GOOD
                                };
                                right(ui, RichText::new(signed(change)).color(color));
                            }
                            2 => right(
                                ui,
                                RichText::new(if e.gone {
                                    "deleted".to_string()
                                } else {
                                    fmt::bytes(e.now)
                                })
                                .color(theme::TEXT_WEAK),
                            ),
                            3 => {
                                ui.add(
                                    Label::new(live.writer(e).unwrap_or(""))
                                        .truncate()
                                        .selectable(false),
                                );
                            }
                            4 => {
                                ui.label(RichText::new(clock(e.at)).color(theme::TEXT_WEAK));
                            }
                            _ => {
                                ui.add(
                                    Label::new(RichText::new(&e.folder).color(theme::TEXT_DIM))
                                        .truncate()
                                        .selectable(false),
                                );
                            }
                        });
                    }
                });
            })
    });
}
