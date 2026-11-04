// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The treemap view. A thread of its own lays the map out and draws it at
//! the screen's own pixel size; asking again (a resize, a zoom, another
//! highlight) makes it drop the picture it is drawing for the newest. The
//! window shows the last finished picture, stretched if the size changed,
//! or zoomed into the folder asked for until that folder's own is ready,
//! and finds what is under the mouse in the picture's index.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anabolic_core::space::treemap::{self, Cell, Kind, Map};
use anabolic_core::space::{NONE, ROOT, Tree};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Ui};

use super::lists::{Lists, node_menu};
use super::{Data, FOLDER};
use crate::proc_table::UiCmd;
use crate::{fmt, theme, widgets};

/// What a picture was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ask {
    version: u64,
    root: u32,
    width: u32,
    height: u32,
    flat: bool,
    highlight: Option<u32>,
}

struct Job {
    generation: u64,
    ask: Ask,
    tree: Arc<Tree>,
    colors: Arc<Vec<[u8; 3]>>,
}

struct Done {
    generation: u64,
    ask: Ask,
    map: Map,
    image: egui::ColorImage,
}

struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    latest: Arc<AtomicU64>,
}

/// The colour of a folder too small to split: that of the file it is
/// mostly made of, found by going down the largest child, a little darker.
fn folder_color(tree: &Tree, colors: &[[u8; 3]], folder: u32, skip: usize) -> [u8; 3] {
    let mut at = folder;
    let mut first = true;
    for _ in 0..64 {
        let n = &tree.node(at);
        if !n.is_dir() {
            return colors[n.ext as usize].map(|v| (u16::from(v) * 4 / 5) as u8);
        }
        // A folder's piece of small files starts after those drawn.
        let pick = if first {
            skip.min(n.count.saturating_sub(1) as usize)
        } else {
            0
        };
        first = false;
        if n.count == 0 {
            break;
        }
        at = n.first + pick as u32;
    }
    FOLDER
}

/// Darker and greyer: what the highlight is not.
fn dim(c: [u8; 3]) -> [u8; 3] {
    let grey = (u16::from(c[0]) + u16::from(c[1]) + u16::from(c[2])) / 3;
    c.map(|v| ((u16::from(v) + grey * 2) / 6) as u8)
}

impl Worker {
    fn spawn(ctx: egui::Context) -> Option<Worker> {
        let (jobs, rx) = unbounded::<Job>();
        let (tx, done) = bounded::<Done>(2);
        let latest = Arc::new(AtomicU64::new(0));
        let newest = latest.clone();
        std::thread::Builder::new()
            .name("space-map".into())
            .spawn(move || {
                while let Ok(mut job) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        job = newer;
                    }
                    if job.generation != newest.load(Ordering::Relaxed) {
                        continue;
                    }
                    let style = treemap::Style {
                        cushion: !job.ask.flat,
                        ..treemap::Style::default()
                    };
                    let (tree, colors, highlight) = (&job.tree, &job.colors, job.ask.highlight);
                    let color = |c: &Cell| -> [u8; 3] {
                        let (base, ext) = match c.kind {
                            Kind::File => {
                                let ext = tree.node(c.node).ext;
                                (colors[ext as usize], Some(ext))
                            }
                            Kind::Rest => {
                                let c = folder_color(tree, colors, c.node, c.skip as usize);
                                (c.map(|v| (u16::from(v) * 3 / 4) as u8), None)
                            }
                            _ => (folder_color(tree, colors, c.node, 0), None),
                        };
                        match highlight {
                            Some(h) if ext != Some(h) => dim(base),
                            _ => base,
                        }
                    };
                    let generation = job.generation;
                    let stop = || newest.load(Ordering::Relaxed) != generation;
                    let Some(mut map) = treemap::render(
                        tree,
                        job.ask.root,
                        job.ask.width,
                        job.ask.height,
                        &style,
                        &color,
                        &stop,
                    ) else {
                        continue;
                    };
                    let pixels = std::mem::take(&mut map.pixels)
                        .into_iter()
                        .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                        .collect();
                    let image = egui::ColorImage::new(
                        [job.ask.width as usize, job.ask.height as usize],
                        pixels,
                    );
                    if tx
                        .send(Done {
                            generation,
                            ask: job.ask,
                            map,
                            image,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .ok()?;
        Some(Worker { jobs, done, latest })
    }
}

#[derive(Default)]
pub struct MapView {
    worker: Option<Worker>,
    generation: u64,
    asked: Option<Ask>,
    /// The picture on screen and what it was drawn for.
    shown: Option<(Ask, Arc<Map>)>,
    texture: Option<egui::TextureHandle>,
    flat: bool,
    /// What the open right-click menu is about.
    menu: Option<u32>,
    /// The map's size in pixels and since when, to wait out a resize.
    size: Option<((u32, u32), Instant)>,
}

impl MapView {
    /// A new tree: the old picture means nothing now.
    pub fn reset(&mut self) {
        self.asked = None;
        self.shown = None;
        self.texture = None;
    }

    fn ask(&mut self, ctx: &egui::Context, ask: Ask, data: &Data) {
        if self.asked == Some(ask) {
            return;
        }
        if self.worker.is_none() {
            self.worker = Worker::spawn(ctx.clone());
        }
        let Some(worker) = &self.worker else {
            return;
        };
        self.generation += 1;
        worker.latest.store(self.generation, Ordering::Relaxed);
        let _ = worker.jobs.send(Job {
            generation: self.generation,
            ask,
            tree: data.tree.clone(),
            colors: data.colors.clone(),
        });
        self.asked = Some(ask);
    }

    fn collect(&mut self, ctx: &egui::Context) {
        let Some(worker) = &self.worker else {
            return;
        };
        while let Ok(done) = worker.done.try_recv() {
            if done.generation != self.generation {
                continue;
            }
            let options = egui::TextureOptions::NEAREST;
            match &mut self.texture {
                Some(t) => t.set(done.image, options),
                None => self.texture = Some(ctx.load_texture("space-map", done.image, options)),
            }
            self.shown = Some((done.ask, Arc::new(done.map)));
        }
    }
}

/// From the map's pixels to the screen.
fn to_screen(r: treemap::Rect, map: &Map, screen: Rect) -> Rect {
    let sx = screen.width() / map.width as f32;
    let sy = screen.height() / map.height as f32;
    Rect::from_min_max(
        Pos2::new(screen.min.x + r.x0 * sx, screen.min.y + r.y0 * sy),
        Pos2::new(screen.min.x + r.x1 * sx, screen.min.y + r.y1 * sy),
    )
}

/// The node a cell stands for: a piece of many small files stands for
/// their folder.
fn node_of(cell: &Cell) -> u32 {
    cell.node
}

pub fn show(
    ui: &mut Ui,
    view: &mut MapView,
    lists: &mut Lists,
    data: &mut Data,
    out: &mut Vec<UiCmd>,
) {
    let ctx = ui.ctx().clone();
    view.collect(&ctx);
    let tree = data.tree.clone();
    if data.zoom as usize >= tree.len() || !tree.node(data.zoom).is_dir() {
        data.zoom = ROOT;
    }

    // The way down to what is shown, each step a link back up.
    let mut zoom_to = None;
    ui.horizontal(|ui| {
        let chain = tree.ancestry(data.zoom);
        for (i, &n) in chain.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").color(theme::TEXT_DIM));
            }
            let name = tree.name(n);
            if n == data.zoom {
                ui.label(RichText::new(name).color(theme::TEXT));
            } else if widgets::link(ui, name).clicked() {
                zoom_to = Some(n);
            }
        }
        ui.label(
            RichText::new(format!("  {}", fmt::bytes(tree.node(data.zoom).size)))
                .color(theme::TEXT_WEAK),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .selectable_label(view.flat, "Flat")
                .on_hover_text("Flat pieces instead of cushions")
                .clicked()
            {
                view.flat = !view.flat;
            }
            let up = tree.node(data.zoom).parent;
            if ui
                .add_enabled(data.zoom != ROOT, egui::Button::new("Up"))
                .on_hover_text("The folder above (Backspace)")
                .clicked()
            {
                zoom_to = Some(up);
            }
        });
    });
    let typing = ctx.egui_wants_keyboard_input();
    let back = ui.input(|i| {
        i.pointer.button_pressed(egui::PointerButton::Extra1)
            || (i.key_pressed(egui::Key::Backspace) && !typing)
    });
    if back && data.zoom != ROOT {
        zoom_to = Some(tree.node(data.zoom).parent);
    }

    let status_height = 20.0;
    let mut rect = ui.available_rect_before_wrap();
    rect.max.y -= status_height;
    if rect.width() < 8.0 || rect.height() < 8.0 {
        return;
    }
    let response = ui.allocate_rect(rect, Sense::click());
    let ppp = ctx.pixels_per_point();
    let ask = Ask {
        version: data.version,
        root: data.zoom,
        width: (rect.width() * ppp).round().max(1.0) as u32,
        height: (rect.height() * ppp).round().max(1.0) as u32,
        flat: view.flat,
        highlight: data.highlight,
    };
    // While the window is being resized, the picture is stretched and a
    // new one is asked for once the size has held for a moment: each new
    // picture is a texture upload, which a drag would repeat every frame.
    let now = Instant::now();
    let size = (ask.width, ask.height);
    if view.size.is_none_or(|(s, _)| s != size) {
        view.size = Some((size, now));
    }
    let settled = view
        .size
        .is_none_or(|(_, since)| now - since >= Duration::from_millis(120));
    let only_size = view.asked.is_some_and(|a| {
        Ask {
            width: ask.width,
            height: ask.height,
            ..a
        } == ask
    });
    if settled || !only_size || view.shown.is_none() {
        view.ask(&ctx, ask, data);
    } else {
        ctx.request_repaint_after(Duration::from_millis(130));
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);
    let mut hovered: Option<(u32, Rect)> = None;
    if let (Some((shown, map)), Some(texture)) = (&view.shown, &view.texture) {
        let same_tree = shown.version == data.version;
        // While another folder's picture is drawn, this one zoomed in.
        let mut uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        if same_tree
            && shown.root != data.zoom
            && let Some(c) = map.cell_of(&tree, data.zoom)
            && map.cells[c as usize].node == data.zoom
        {
            let r = map.cells[c as usize].rect;
            uv = Rect::from_min_max(
                Pos2::new(r.x0 / map.width as f32, r.y0 / map.height as f32),
                Pos2::new(r.x1 / map.width as f32, r.y1 / map.height as f32),
            );
        }
        painter.image(texture.id(), rect, uv, Color32::WHITE);

        // What is under the mouse, from the picture's index.
        let current = same_tree && shown.root == data.zoom;
        if current && let Some(pos) = response.hover_pos() {
            let x = ((pos.x - rect.min.x) / rect.width() * map.width as f32) as u32;
            let y = ((pos.y - rect.min.y) / rect.height() * map.height as f32) as u32;
            if let Some(c) = map.cell_at(x, y) {
                let cell = &map.cells[c as usize];
                hovered = Some((node_of(cell), to_screen(cell.rect, map, rect)));
                // Its folder, faintly, so the grouping shows.
                if cell.parent != NONE {
                    let parent = &map.cells[cell.parent as usize];
                    painter.rect_stroke(
                        to_screen(parent.rect, map, rect),
                        0.0,
                        Stroke::new(1.0, Color32::from_white_alpha(70)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        if current
            && let Some(sel) = data.selected
            && let Some(c) = map.cell_of(&tree, sel)
        {
            let r = to_screen(map.cells[c as usize].rect, map, rect);
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
        }
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Drawing…",
            egui::FontId::proportional(theme::BODY),
            theme::TEXT_WEAK,
        );
    }
    if let Some((_, r)) = hovered {
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(1.5, Color32::from_white_alpha(220)),
            egui::StrokeKind::Inside,
        );
    }

    // A click selects, a double click goes one folder in, the right button
    // offers the rest.
    if let Some((n, _)) = hovered {
        if response.clicked() {
            data.selected = Some(n);
            lists.reveal(&tree, n);
        }
        if response.double_clicked() {
            let chain = tree.ancestry(n);
            if let Some(at) = chain.iter().position(|&a| a == data.zoom)
                && let Some(&next) = chain.get(at + 1)
                && tree.node(next).is_dir()
            {
                zoom_to = Some(next);
            }
        }
        if response.secondary_clicked() {
            view.menu = Some(n);
        }
    }
    // The menu covers the map, so it cannot depend on what is under the
    // mouse.
    if let Some(n) = view.menu {
        response.context_menu(|ui| {
            if data.zoom != ROOT && ui.button("Up a folder").clicked() {
                zoom_to = Some(tree.node(data.zoom).parent);
                ui.close();
            }
            if ui.button("Select in the list").clicked() {
                data.selected = Some(n);
                lists.reveal(&tree, n);
                ui.close();
            }
            node_menu(ui, data, n, out);
        });
    }
    if let Some(z) = zoom_to
        && z != NONE
    {
        data.zoom = z;
    }

    // What is under the mouse, in words.
    ui.allocate_ui(egui::vec2(rect.width(), status_height), |ui| {
        ui.horizontal(|ui| match hovered {
            Some((n, _)) => {
                let node = tree.node(n);
                let kind = view
                    .shown
                    .as_ref()
                    .and_then(|(_, m)| {
                        let pos = response.hover_pos()?;
                        let x = ((pos.x - rect.min.x) / rect.width() * m.width as f32) as u32;
                        let y = ((pos.y - rect.min.y) / rect.height() * m.height as f32) as u32;
                        Some(m.cells[m.cell_at(x, y)? as usize].kind)
                    });
                let path = data.path(n);
                let what = if kind == Some(Kind::Rest) {
                    format!("{path}  ·  small files together")
                } else {
                    path
                };
                ui.label(RichText::new(what).color(theme::TEXT));
                ui.label(
                    RichText::new(format!(
                        "{}  ·  {:.1} % of its folder{}",
                        fmt::bytes(node.size),
                        data.share(n) * 100.0,
                        if node.is_dir() {
                            format!("  ·  {} files", fmt::count(u64::from(node.files)))
                        } else {
                            String::new()
                        }
                    ))
                    .color(theme::TEXT_WEAK),
                );
            }
            None => {
                ui.label(
                    RichText::new(
                        "Click to select, double-click to go into a folder, Backspace to go back up",
                    )
                    .small()
                    .color(theme::TEXT_DIM),
                );
            }
        });
    });
}
