// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Disk space page: what fills a drive or a folder, as a tree of
//! folders, the file types and the largest files above a treemap of the
//! whole.
//!
//! Nothing here waits: the drive is read on a thread of its own, the
//! treemap is laid out and drawn on another, and the window shows the last
//! finished picture (stretched while a new one is drawn). What the mouse
//! points at is looked up in the picture's index, one entry per pixel, so
//! a frame costs the same for ten files and ten million.

mod clean;
mod delete;
mod files;
mod lists;
mod map;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::space::clean::Suggestion;
use anabolic_core::space::{self, ExtTotal, Phase, Progress, ROOT, Target, Tree};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Ui};

use serde::{Deserialize, Serialize};

use crate::proc_table::UiCmd;
use crate::{fmt, theme, widgets};

/// What the page keeps between runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SpaceSettings {
    /// Flat pieces instead of cushions.
    pub flat: bool,
    /// Duplicates are looked for among files of at least this many MB.
    pub duplicates_from_mb: u32,
    /// A summary of each scan is kept, to compare with later ones.
    pub keep_scans: bool,
}

impl Default for SpaceSettings {
    fn default() -> Self {
        SpaceSettings {
            flat: false,
            duplicates_from_mb: 1,
            keep_scans: true,
        }
    }
}

/// The colours of the extensions that take the most room, in that order.
const PALETTE: [[u8; 3]; 14] = [
    [74, 136, 214],
    [222, 132, 60],
    [92, 178, 100],
    [192, 90, 172],
    [210, 186, 68],
    [64, 176, 182],
    [208, 84, 84],
    [138, 110, 214],
    [160, 196, 76],
    [216, 118, 156],
    [100, 150, 176],
    [186, 148, 96],
    [120, 120, 200],
    [176, 112, 76],
];
/// Every other extension.
const OTHER: [u8; 3] = [104, 110, 124];
/// A folder too small to split, when nothing better is known.
const FOLDER: [u8; 3] = [86, 90, 100];

fn color32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A finished scan with what the page shows of it.
pub struct Data {
    pub tree: Arc<Tree>,
    /// Bumped whenever the tree changes, so the map is drawn again.
    pub version: u64,
    pub exts: Vec<ExtTotal>,
    /// Each extension's colour, by extension number.
    pub colors: Arc<Vec<[u8; 3]>>,
    pub largest: Vec<u32>,
    /// The folder the map shows.
    pub zoom: u32,
    pub selected: Option<u32>,
    /// An extension whose files the map picks out.
    pub highlight: Option<u32>,
    /// A menu or key asked to delete: (node, to the Recycle Bin).
    pub ask_delete: Option<(u32, bool)>,
    /// What can be cleaned up.
    pub clean: Vec<Suggestion>,
    /// The Clean up tab asked to delete this suggestion's places.
    pub ask_clean: Option<usize>,
    /// Open the folder list down to this node.
    pub reveal: Option<u32>,
}

impl Data {
    fn new(s: Scanned) -> Data {
        Data {
            tree: Arc::new(s.tree),
            version: 0,
            exts: s.exts,
            colors: Arc::new(s.colors),
            largest: s.largest,
            zoom: ROOT,
            selected: None,
            highlight: None,
            ask_delete: None,
            clean: s.clean,
            ask_clean: None,
            reveal: None,
        }
    }

    /// The share of `n`'s folder (or of everything for the root).
    fn share(&self, n: u32) -> f32 {
        let node = &self.tree.node(n);
        let whole = if node.parent == space::NONE {
            node.size
        } else {
            self.tree.node(node.parent).size
        };
        if whole == 0 {
            0.0
        } else {
            node.size as f32 / whole as f32
        }
    }

    fn path(&self, n: u32) -> String {
        self.tree.path_of(n).to_string_lossy().into_owned()
    }
}

/// What the scan thread hands over.
struct Scanned {
    tree: Tree,
    exts: Vec<ExtTotal>,
    colors: Vec<[u8; 3]>,
    largest: Vec<u32>,
    clean: Vec<Suggestion>,
}

/// The work that follows a scan, done on its thread too.
fn prepare(tree: Tree) -> Scanned {
    let exts = tree.by_extension();
    let mut colors = vec![OTHER; tree.ext_count()];
    for (rank, e) in exts.iter().take(PALETTE.len()).enumerate() {
        colors[e.ext as usize] = PALETTE[rank];
    }
    let largest = tree.largest_files(1000);
    let clean = space::clean::suggest(&tree);
    Scanned {
        tree,
        exts,
        colors,
        largest,
        clean,
    }
}

struct Scanning {
    path: PathBuf,
    progress: Arc<Progress>,
    rx: Receiver<Result<Scanned, String>>,
    started: Instant,
}

#[derive(Default)]
pub struct SpacePage {
    targets: Option<Vec<Target>>,
    targets_rx: Option<Receiver<Vec<Target>>>,
    /// A folder typed in.
    folder: String,
    scanning: Option<Scanning>,
    error: Option<String>,
    pub data: Option<Data>,
    /// Show the drives again over a finished scan.
    choosing: bool,
    lists: lists::Lists,
    map: map::MapView,
    /// From `--space PATH`: scan it when the page first shows.
    pub start: Option<PathBuf>,
    pub deleter: delete::Deleter,
    /// The folder picker, while it is open.
    picking: Option<Receiver<Option<PathBuf>>>,
    /// A CSV being written.
    export: Option<Receiver<Result<PathBuf, String>>>,
    /// The last CSV written, or why it was not.
    exported: Option<Result<PathBuf, String>>,
}

impl SpacePage {
    fn begin(&mut self, path: PathBuf, ctx: &egui::Context) {
        if let Some(s) = &self.scanning {
            s.progress.cancel();
        }
        let progress = Arc::new(Progress::default());
        let (tx, rx) = bounded(1);
        let (p, ctx, at) = (progress.clone(), ctx.clone(), path.clone());
        let spawned = std::thread::Builder::new()
            .name("space-scan".into())
            .spawn(move || {
                let result = space::scan(&at, &p).map(prepare);
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.error = Some(format!("The scan could not start: {e}"));
            return;
        }
        self.scanning = Some(Scanning {
            path,
            progress,
            rx,
            started: Instant::now(),
        });
        self.error = None;
        self.choosing = false;
    }

    /// Picks up what the threads finished.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.targets_rx
            && let Ok(t) = rx.try_recv()
        {
            self.targets = Some(t);
            self.targets_rx = None;
        }
        if self.targets.is_none() && self.targets_rx.is_none() {
            let (tx, rx) = bounded(1);
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new()
                .name("space-drives".into())
                .spawn(move || {
                    let _ = tx.send(space::targets());
                    ctx.request_repaint();
                });
            self.targets_rx = Some(rx);
        }
        let Some(s) = &self.scanning else {
            return;
        };
        match s.rx.try_recv() {
            Ok(Ok(scanned)) => {
                let old = self.data.replace(Data::new(scanned));
                // A tree of millions of nodes is let go of elsewhere.
                if let Some(old) = old {
                    let _ = std::thread::Builder::new()
                        .name("space-drop".into())
                        .spawn(move || drop(old));
                }
                self.lists.reset();
                self.map.reset();
                self.scanning = None;
            }
            Ok(Err(e)) => {
                if e != space::CANCELLED {
                    self.error = Some(e);
                }
                self.scanning = None;
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.error = Some("The scan stopped without an answer.".into());
                self.scanning = None;
            }
        }
    }
}

pub fn show(
    ui: &mut Ui,
    page: &mut SpacePage,
    settings: &mut SpaceSettings,
    elevated: bool,
    out: &mut Vec<UiCmd>,
) {
    let ctx = ui.ctx().clone();
    if let Some(path) = page.start.take() {
        page.begin(path, &ctx);
    }
    page.poll(&ctx);
    if let Some(rx) = &page.picking
        && let Ok(picked) = rx.try_recv()
    {
        page.picking = None;
        if let Some(path) = picked {
            page.folder = path.to_string_lossy().into_owned();
            page.begin(path, &ctx);
        }
    }
    if let Some(rx) = &page.export {
        match rx.try_recv() {
            Ok(done) => {
                page.exported = Some(done);
                page.export = None;
            }
            Err(_) => ctx.request_repaint_after(Duration::from_millis(100)),
        }
    }

    if let Some(s) = &page.scanning {
        progress(ui, s);
        ui.add_space(6.0);
    }
    if let Some(e) = &page.error {
        ui.label(RichText::new(e).color(theme::BAD));
        ui.add_space(6.0);
    }
    if page.data.is_none() || page.choosing {
        if page.scanning.is_none() || page.data.is_some() {
            chooser(ui, page, elevated, &ctx);
        }
        return;
    }
    summary(ui, page, &ctx);
    page.deleter.status(ui);
    match &page.exported {
        Some(Ok(path)) => {
            let path = path.clone();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Saved {}", path.display())).color(theme::TEXT_WEAK),
                );
                if widgets::link(ui, "Show it").clicked() {
                    out.push(UiCmd::OpenLocation(path.to_string_lossy().into_owned()));
                }
            });
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("Not saved: {e}")).color(theme::BAD));
        }
        None => {}
    }
    ui.add_space(4.0);
    let data = page.data.as_mut().expect("checked above");
    page.deleter.frame(&ctx, data);
    // Delete and Shift+Delete, as in Explorer.
    if !ctx.egui_wants_keyboard_input()
        && let Some(n) = data.selected
    {
        let (delete, shift) = ui.input(|i| (i.key_pressed(egui::Key::Delete), i.modifiers.shift));
        if delete {
            data.ask_delete = Some((n, !shift));
        }
    }
    let height = ui.available_height();
    egui::Panel::top("space-lists")
        .resizable(true)
        .default_size((height * 0.42).max(120.0))
        .min_size(90.0)
        .frame(egui::Frame::NONE)
        .show(ui, |ui| lists::show(ui, &mut page.lists, data, out));
    page.map.flat = settings.flat;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        ui.add_space(4.0);
        map::show(ui, &mut page.map, &mut page.lists, data, out);
    });
    settings.flat = page.map.flat;
    if let Some(tab) = page.lists.export.take()
        && page.export.is_none()
    {
        let tree = data.tree.clone();
        page.exported = None;
        page.export = Some(match tab {
            lists::Tab::Folders => files::save("folders", tree, files::folders),
            lists::Tab::Types => {
                let exts = data.exts.clone();
                files::save("types", tree, move |t| files::types(t, &exts))
            }
            lists::Tab::Largest => {
                let largest = data.largest.clone();
                files::save("largest", tree, move |t| files::largest(t, &largest))
            }
            lists::Tab::Clean => {
                let clean = data.clean.clone();
                files::save("cleanup", tree, move |t| files::clean(t, &clean))
            }
        });
    }
    if let Some((n, recycle)) = data.ask_delete.take() {
        page.deleter.ask(data, n, recycle);
    }
    if let Some(i) = data.ask_clean.take()
        && let Some(s) = data.clean.get(i).cloned()
    {
        page.deleter.ask_suggestion(data, &s);
    }
    if let Some(n) = data.reveal.take() {
        page.lists.reveal(&data.tree, n);
    }
}

fn progress(ui: &mut Ui, s: &Scanning) {
    use std::sync::atomic::Ordering::Relaxed;
    let p = &s.progress;
    ui.horizontal(|ui| {
        ui.spinner();
        let table = p.table_total.load(Relaxed);
        let what = if p.phase() == Phase::Building {
            "Putting the tree together".to_string()
        } else if table > 0 {
            format!(
                "Reading the master file table: {} of {}",
                fmt::bytes(p.table_read.load(Relaxed)),
                fmt::bytes(table)
            )
        } else {
            format!(
                "Reading folders: {} folders, {} files, {}",
                fmt::count(p.folders.load(Relaxed)),
                fmt::count(p.files.load(Relaxed)),
                fmt::bytes(p.bytes.load(Relaxed))
            )
        };
        ui.label(RichText::new(format!("{}  ·  {what}", s.path.display())).color(theme::TEXT));
        ui.label(
            RichText::new(fmt::duration(s.started.elapsed()))
                .small()
                .color(theme::TEXT_WEAK),
        );
        if ui.button("Stop").clicked() {
            p.cancel();
        }
    });
    let table = p.table_total.load(Relaxed);
    if table > 0 && p.phase() == Phase::Reading {
        let done = p.table_read.load(Relaxed) as f32 / table as f32;
        ui.add(egui::ProgressBar::new(done).desired_height(4.0));
    }
}

/// The drives, and a folder to type in.
fn chooser(ui: &mut Ui, page: &mut SpacePage, elevated: bool, ctx: &egui::Context) {
    ui.label(widgets::strong("What fills your drives", theme::SUBTITLE));
    ui.add_space(2.0);
    let fast = if cfg!(windows) && !elevated {
        "Folders are read one by one. As administrator, Anabolic reads an NTFS drive's \
         master file table instead, which takes seconds for millions of files."
    } else if cfg!(windows) {
        "NTFS drives are read from their master file table, in seconds; others folder by folder."
    } else {
        "Folders are read on several threads at once; a file system mounted inside is shown, \
         not entered."
    };
    ui.label(RichText::new(fast).color(theme::TEXT_WEAK));
    ui.add_space(10.0);
    match &page.targets {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Looking for drives…").color(theme::TEXT_WEAK));
            });
        }
        Some(targets) => {
            let mut chosen = None;
            ui.horizontal_wrapped(|ui| {
                for t in targets {
                    if drive_card(ui, t).clicked() {
                        chosen = Some(t.path.clone());
                    }
                }
            });
            if let Some(path) = chosen {
                page.begin(path, ctx);
            }
        }
    }
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.label("Folder");
        let edit = ui.add(
            egui::TextEdit::singleline(&mut page.folder)
                .hint_text(if cfg!(windows) {
                    r"C:\Users\you\Downloads"
                } else {
                    "/home/you/Downloads"
                })
                .desired_width(420.0),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let go = ui
            .add_enabled(!page.folder.trim().is_empty(), egui::Button::new("Scan"))
            .clicked();
        if ui
            .add_enabled(page.picking.is_none(), egui::Button::new("Choose…"))
            .clicked()
        {
            page.picking = Some(files::pick_folder(page.deleter.owner));
        }
        if (go || enter) && !page.folder.trim().is_empty() {
            let path = PathBuf::from(page.folder.trim());
            page.begin(path, ctx);
        }
        if page.data.is_some() && ui.button("Back to the last scan").clicked() {
            page.choosing = false;
        }
        if ui
            .button("Look again")
            .on_hover_text("Ask every drive again")
            .clicked()
        {
            page.targets = None;
        }
    });
}

fn drive_card(ui: &mut Ui, t: &Target) -> egui::Response {
    let size = egui::vec2(236.0, 66.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter_at(rect);
    let fill = if response.hovered() {
        theme::HOVER
    } else {
        theme::RAISED
    };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink2(egui::vec2(10.0, 8.0));
    let path = t.path.to_string_lossy();
    let title = if t.label.is_empty() {
        path.to_string()
    } else {
        format!("{path}  {}", t.label)
    };
    painter.text(
        inner.left_top(),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(theme::BODY),
        theme::TEXT,
    );
    let used = t.total.saturating_sub(t.free);
    let share = if t.total > 0 {
        used as f32 / t.total as f32
    } else {
        0.0
    };
    let bar = egui::Rect::from_min_size(
        inner.left_top() + egui::vec2(0.0, 22.0),
        egui::vec2(inner.width(), 6.0),
    );
    painter.rect_filled(bar, 2.0, theme::BUTTON);
    let full = if share > 0.9 { theme::BAD } else { theme::CPU };
    painter.rect_filled(
        egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * share, bar.height())),
        2.0,
        full,
    );
    let line = if t.total > 0 {
        format!(
            "{} free of {}{}",
            fmt::bytes(t.free),
            fmt::bytes(t.total),
            if t.fs.is_empty() {
                String::new()
            } else {
                format!("  ·  {}", t.fs)
            }
        )
    } else {
        t.fs.clone()
    };
    painter.text(
        inner.left_bottom(),
        egui::Align2::LEFT_BOTTOM,
        line,
        egui::FontId::proportional(theme::META),
        theme::TEXT_WEAK,
    );
    let hint = if t.fast {
        "Read from the master file table: seconds"
    } else {
        "Read folder by folder"
    };
    response.on_hover_text(hint)
}

/// "6.7 s", or milliseconds under a second.
fn took(d: Duration) -> String {
    if d >= Duration::from_secs(1) {
        format!("{:.1} s", d.as_secs_f64())
    } else {
        fmt::millis(d)
    }
}

/// One line about the scan shown, with what can be done next.
fn summary(ui: &mut Ui, page: &mut SpacePage, ctx: &egui::Context) {
    let Some(data) = &page.data else {
        return;
    };
    let tree = &data.tree;
    let root = &tree.node(ROOT);
    let mut rescan = false;
    ui.horizontal(|ui| {
        ui.label(widgets::strong(
            tree.path.to_string_lossy(),
            theme::SUBTITLE,
        ));
        ui.add_space(6.0);
        let mut facts = format!(
            "{}  ·  {} files  ·  {} on disk  ·  read {} in {}",
            fmt::bytes(root.size),
            fmt::count(u64::from(root.files)),
            fmt::bytes(root.alloc),
            tree.method.label(),
            took(tree.took),
        );
        if tree.total > 0 {
            facts.push_str(&format!(
                "  ·  drive {} free of {}",
                fmt::bytes(tree.free),
                fmt::bytes(tree.total)
            ));
        }
        if tree.unreadable > 0 {
            facts.push_str(&format!(
                "  ·  {} folders could not be read",
                fmt::count(u64::from(tree.unreadable))
            ));
        }
        ui.label(RichText::new(facts).small().color(theme::TEXT_WEAK));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Other drive or folder").clicked() {
                page.choosing = true;
            }
            if ui
                .add_enabled(page.scanning.is_none(), egui::Button::new("Scan again"))
                .clicked()
            {
                rescan = true;
            }
        });
    });
    if let Some(note) = &tree.note {
        ui.label(RichText::new(note).small().color(theme::ELEVATED_TEXT));
    }
    if rescan {
        let path = tree.path.clone();
        page.begin(path, ctx);
    }
}
