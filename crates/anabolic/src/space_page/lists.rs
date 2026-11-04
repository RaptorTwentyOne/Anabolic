// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The lists above the map: the folders as a tree, the file types, and the
//! largest files. Only the rows on screen are drawn; the folder tree keeps
//! the rows of what is open, rebuilt when a folder opens or closes.

use std::collections::HashSet;

use anabolic_core::space::{DIR, EXTRA_LINK, Node, REMOVED, ROOT, Tree};
use anabolic_core::sysinfo::local_time;
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use super::{Data, color32};
use crate::proc_table::{UiCmd, item};
use crate::table::{Columns, Spec};
use crate::{fmt, theme, widgets};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Folders,
    Types,
    Largest,
    Duplicates,
    Clean,
}

#[derive(Default)]
pub struct Lists {
    pub tab: Tab,
    /// "Save as CSV" was pressed on this tab.
    pub export: Option<Tab>,
    open: HashSet<u32>,
    /// The folder tree's rows: (node, depth).
    rows: Vec<(u32, u16)>,
    stale: bool,
    /// The version of the tree the rows were made from.
    made_for: u64,
    /// Scroll this row into view next frame.
    scroll_to: Option<usize>,
    /// Clean-up suggestions shown with all their places.
    open_clean: HashSet<usize>,
}

impl Lists {
    pub fn reset(&mut self) {
        self.open.clear();
        self.open.insert(ROOT);
        self.stale = true;
        self.scroll_to = None;
    }

    /// Opens the folders down to `node` and scrolls to it.
    pub fn reveal(&mut self, tree: &Tree, node: u32) {
        let chain = tree.ancestry(node);
        for &a in &chain[..chain.len().saturating_sub(1)] {
            self.open.insert(a);
        }
        self.tab = Tab::Folders;
        self.rebuild(tree);
        self.scroll_to = self.rows.iter().position(|&(n, _)| n == node);
    }

    fn rebuild(&mut self, tree: &Tree) {
        self.rows.clear();
        let mut stack = vec![(ROOT, 0u16)];
        while let Some((n, depth)) = stack.pop() {
            self.rows.push((n, depth));
            let node = &tree.node(n);
            if self.open.contains(&n) {
                for c in node.children().rev() {
                    if tree.node(c).flags & REMOVED == 0 {
                        stack.push((c, depth + 1));
                    }
                }
            }
        }
        self.stale = false;
    }
}

const FOLDER_TITLES: [(&str, bool); 6] = [
    ("Name", false),
    ("Size", true),
    ("Of its folder", false),
    ("On disk", true),
    ("Files", true),
    ("Modified", false),
];
const FOLDER_COLUMNS: [Spec; 6] = [
    Spec::new(380.0).clip(),
    Spec::new(84.0),
    Spec::new(120.0),
    Spec::new(84.0),
    Spec::new(84.0),
    Spec::new(130.0),
];
const TYPE_TITLES: [(&str, bool); 5] = [
    ("Type", false),
    ("Size", true),
    ("Of everything", false),
    ("On disk", true),
    ("Files", true),
];
const TYPE_COLUMNS: [Spec; 5] = [
    Spec::new(200.0).clip(),
    Spec::new(90.0),
    Spec::new(140.0),
    Spec::new(90.0),
    Spec::new(90.0),
];
const LARGEST_TITLES: [(&str, bool); 4] = [
    ("Name", false),
    ("Size", true),
    ("Modified", false),
    ("Folder", false),
];
const LARGEST_COLUMNS: [Spec; 4] = [
    Spec::new(280.0).clip(),
    Spec::new(90.0),
    Spec::new(130.0),
    Spec::new(400.0).clip(),
];

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

/// A share as a bar with its percentage.
fn bar(ui: &mut Ui, share: f32, color: egui::Color32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, widgets::ROW), Sense::hover());
    let track = egui::Rect::from_min_size(
        rect.left_center() - egui::vec2(0.0, 5.0),
        egui::vec2((width - 44.0).max(10.0), 10.0),
    );
    let p = ui.painter();
    p.rect_filled(track, 2.0, theme::BUTTON);
    p.rect_filled(
        egui::Rect::from_min_size(
            track.min,
            egui::vec2(track.width() * share.clamp(0.0, 1.0), track.height()),
        ),
        2.0,
        color,
    );
    p.text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        format!("{:.1} %", share * 100.0),
        egui::FontId::proportional(theme::META),
        theme::TEXT_WEAK,
    );
}

fn modified(n: &Node) -> String {
    if n.modified <= 0 {
        return String::new();
    }
    let filetime = n.modified * 10_000_000 + 116_444_736_000_000_000;
    local_time(filetime)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

/// The row menu of a file or folder.
pub fn node_menu(ui: &mut Ui, data: &mut Data, n: u32, out: &mut Vec<UiCmd>) {
    let node = data.tree.node(n);
    if node.is_dir() && node.count > 0 && ui.button("Show in the map").clicked() {
        data.zoom = n;
        ui.close();
    }
    let path = data.path(n);
    item(
        ui,
        out,
        if cfg!(windows) {
            "Open in Explorer"
        } else {
            "Open its folder"
        },
        "",
        UiCmd::OpenLocation(path.clone()),
    );
    item(ui, out, "Copy path", "", UiCmd::Copy(path));
    ui.separator();
    let trash = if cfg!(windows) {
        "Move to the Recycle Bin…"
    } else {
        "Move to the trash…"
    };
    if ui
        .add(egui::Button::new(trash).shortcut_text("Delete"))
        .clicked()
    {
        data.ask_delete = Some((n, true));
        ui.close();
    }
    if ui
        .add(egui::Button::new("Delete for good…").shortcut_text("Shift+Delete"))
        .clicked()
    {
        data.ask_delete = Some((n, false));
        ui.close();
    }
}

pub fn show(ui: &mut Ui, lists: &mut Lists, data: &mut Data, out: &mut Vec<UiCmd>) {
    tabs(ui, lists, data);
    match lists.tab {
        Tab::Folders => folders(ui, lists, data, out),
        Tab::Types => types(ui, data),
        Tab::Largest => largest(ui, lists, data, out),
        Tab::Clean => super::clean::show(ui, data, &mut lists.open_clean),
        // Drawn by the page, which keeps the search.
        Tab::Duplicates => {}
    }
}

/// The row of tabs, with the CSV button and the highlight's note.
pub fn tabs(ui: &mut Ui, lists: &mut Lists, data: &mut Data) {
    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Folders, "Folders"),
            (Tab::Types, "File types"),
            (Tab::Largest, "Largest files"),
            (Tab::Duplicates, "Duplicates"),
            (Tab::Clean, "Clean up"),
        ] {
            if ui.selectable_label(lists.tab == tab, label).clicked() {
                lists.tab = tab;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button("Save as CSV")
                .on_hover_text("Into Anabolic's exports folder")
                .clicked()
            {
                lists.export = Some(lists.tab);
            }
        });
        if let Some(e) = data.highlight {
            ui.add_space(12.0);
            let name = match data.tree.ext_name(e) {
                "" => "files without an extension".to_string(),
                e => format!(".{e} files"),
            };
            ui.label(
                RichText::new(format!("The map picks out {name}"))
                    .small()
                    .color(theme::TEXT_WEAK),
            );
            if ui.small_button("Show all").clicked() {
                data.highlight = None;
            }
        }
    });
    ui.add_space(2.0);
}

fn folders(ui: &mut Ui, lists: &mut Lists, data: &mut Data, out: &mut Vec<UiCmd>) {
    if lists.stale || lists.made_for != data.version {
        lists.made_for = data.version;
        lists.rebuild(&data.tree);
    }
    let scroll_to = lists.scroll_to.take();
    let tree = data.tree.clone();
    let mut toggle = None;
    Columns::load(ui.ctx(), "space-folders", &FOLDER_COLUMNS).show(ui, |table, columns| {
        let mut table = table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center));
        if let Some(row) = scroll_to {
            table = table.scroll_to_row(row, Some(Align::Center));
        }
        table
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = FOLDER_TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, lists.rows.len(), |mut row| {
                    let (n, depth) = lists.rows[row.index()];
                    let node = tree.node(n);
                    row.set_selected(data.selected == Some(n));
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                ui.add_space(f32::from(depth) * 14.0);
                                if node.is_dir() && node.count > 0 {
                                    let open = lists.open.contains(&n);
                                    let arrow = if open { "⏷" } else { "⏵" };
                                    if ui
                                        .add(
                                            Label::new(
                                                RichText::new(arrow).color(theme::TEXT_WEAK),
                                            )
                                            .sense(Sense::click())
                                            .selectable(false),
                                        )
                                        .clicked()
                                    {
                                        toggle = Some(n);
                                    }
                                } else {
                                    ui.add_space(12.0);
                                }
                                let mut text = RichText::new(tree.name(n));
                                if node.flags & EXTRA_LINK != 0 {
                                    text = text.color(theme::TEXT_DIM);
                                } else if !node.is_dir() {
                                    text = text.color(theme::TEXT_WEAK);
                                }
                                ui.add(Label::new(text).truncate().selectable(false));
                            }
                            1 => right(ui, fmt::bytes(node.size)),
                            2 => {
                                let color = if node.flags & DIR != 0 {
                                    theme::TEXT_DIM
                                } else {
                                    color32(data.colors[node.ext as usize])
                                };
                                bar(ui, data.share(n), color);
                            }
                            3 => right(
                                ui,
                                RichText::new(fmt::bytes(node.alloc)).color(theme::TEXT_WEAK),
                            ),
                            4 => {
                                if node.is_dir() {
                                    right(
                                        ui,
                                        RichText::new(fmt::count(u64::from(node.files)))
                                            .color(theme::TEXT_WEAK),
                                    );
                                }
                            }
                            _ => {
                                ui.label(RichText::new(modified(&node)).color(theme::TEXT_WEAK));
                            }
                        });
                    }
                    let response = row.response();
                    if response.clicked() {
                        data.selected = Some(n);
                    }
                    if response.double_clicked() && node.is_dir() && node.count > 0 {
                        toggle = Some(n);
                    }
                    response.context_menu(|ui| node_menu(ui, data, n, out));
                });
            })
    });
    if let Some(n) = toggle {
        if !lists.open.remove(&n) {
            lists.open.insert(n);
        }
        lists.stale = true;
    }
}

fn types(ui: &mut Ui, data: &mut Data) {
    let total = data.tree.node(ROOT).size.max(1);
    let mut pick = None;
    Columns::load(ui.ctx(), "space-types", &TYPE_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = TYPE_TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, data.exts.len(), |mut row| {
                    let e = data.exts[row.index()];
                    row.set_selected(data.highlight == Some(e.ext));
                    let color = color32(data.colors[e.ext as usize]);
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                                ui.painter().rect_filled(rect, 2.0, color);
                                let name = match data.tree.ext_name(e.ext) {
                                    "" => "(no extension)".to_string(),
                                    x => format!(".{x}"),
                                };
                                ui.add(Label::new(name).truncate().selectable(false));
                            }
                            1 => right(ui, fmt::bytes(e.size)),
                            2 => bar(ui, e.size as f32 / total as f32, color),
                            3 => right(
                                ui,
                                RichText::new(fmt::bytes(e.alloc)).color(theme::TEXT_WEAK),
                            ),
                            _ => right(
                                ui,
                                RichText::new(fmt::count(e.files)).color(theme::TEXT_WEAK),
                            ),
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(e.ext);
                    }
                });
            })
    });
    if let Some(e) = pick {
        data.highlight = if data.highlight == Some(e) {
            None
        } else {
            Some(e)
        };
    }
}

fn largest(ui: &mut Ui, lists: &mut Lists, data: &mut Data, out: &mut Vec<UiCmd>) {
    let tree = data.tree.clone();
    let files: Vec<u32> = data
        .largest
        .iter()
        .copied()
        .filter(|&n| !tree.is_gone(n))
        .collect();
    let mut reveal = None;
    Columns::load(ui.ctx(), "space-largest", &LARGEST_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = LARGEST_TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, files.len(), |mut row| {
                    let n = files[row.index()];
                    let node = tree.node(n);
                    row.set_selected(data.selected == Some(n));
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    color32(data.colors[node.ext as usize]),
                                );
                                ui.add(Label::new(tree.name(n)).truncate().selectable(false));
                            }
                            1 => right(ui, fmt::bytes(node.size)),
                            2 => {
                                ui.label(RichText::new(modified(&node)).color(theme::TEXT_WEAK));
                            }
                            _ => {
                                let folder = tree.path_of(node.parent);
                                ui.add(
                                    Label::new(
                                        RichText::new(folder.to_string_lossy())
                                            .color(theme::TEXT_DIM),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                            }
                        });
                    }
                    let response = row.response();
                    if response.clicked() {
                        data.selected = Some(n);
                    }
                    if response.double_clicked() {
                        reveal = Some(n);
                    }
                    response.context_menu(|ui| node_menu(ui, data, n, out));
                });
            })
    });
    if let Some(n) = reveal {
        lists.reveal(&tree, n);
    }
}
