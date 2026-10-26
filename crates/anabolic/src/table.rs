// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Sorting and filtering for the tables: a click on a column title sorts by
//! it (again: the other way round), and a filter box keeps the rows that
//! contain every word typed. Dragging a title moves its column (see
//! `Columns`).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use eframe::egui::scroll_area::{ScrollAreaOutput, ScrollBarVisibility};
use eframe::egui::{
    self, Align, Context, CursorIcon, Id, Key, Label, LayerId, Layout, Popup, PopupCloseBehavior,
    Rect, Response, RichText, Sense, Stroke, TextEdit, Ui,
};
use egui_extras::{Column, TableBuilder, TableRow};

use crate::settings::TableLayout;
use crate::{theme, widgets};

#[derive(Clone, Default)]
pub struct TableView {
    /// Column and whether descending; None keeps the source's order.
    pub sort: Option<(usize, bool)>,
    pub filter: String,
    /// `filter` in lower case, split into words.
    words: Vec<String>,
}

/// Sort and filter state of the details tables, for the session.
pub struct DetailTables {
    pub threads: TableView,
    pub modules: TableView,
    pub network: TableView,
    /// Names of the remote addresses in the network tab.
    pub names: crate::netnames::Names,
    /// The connection whose side pane is open.
    pub connection: Option<(anabolic_core::nettrace::Proto, std::net::IpAddr, u16, u16)>,
}

impl Default for DetailTables {
    fn default() -> Self {
        Self {
            // The busiest threads and connections first, as before.
            threads: TableView::sorted(1, true),
            modules: TableView::default(),
            network: TableView::default(),
            names: Default::default(),
            connection: None,
        }
    }
}

impl TableView {
    pub fn sorted(column: usize, descending: bool) -> Self {
        Self {
            sort: Some((column, descending)),
            ..Default::default()
        }
    }

    /// A column title that sorts by its column when clicked, with an arrow
    /// beside it while it does. Numbers sort largest first at first, text
    /// A to Z. Returns true when the order changed.
    pub fn header(&mut self, ui: &mut Ui, column: usize, title: &str, numeric: bool) -> bool {
        let sorted = self.sort.filter(|(c, _)| *c == column);
        let text = widgets::strong(title, theme::META).color(if sorted.is_some() {
            theme::TEXT
        } else {
            theme::TEXT_WEAK
        });
        let arrow = sorted.map(|(_, descending)| {
            let glyph = if descending {
                theme::icon::CHEVRON_DOWN
            } else {
                theme::icon::CHEVRON_UP
            };
            widgets::icon(glyph, 9.0).color(theme::ACCENT)
        });
        // The whole cell is the button, not only the letters: the title
        // `Columns::header` made, which takes the drags that move it too.
        let id = title_id(ui, column);
        let response = ui
            .ctx()
            .read_response(id)
            .unwrap_or_else(|| ui.interact(ui.max_rect(), id, Sense::click()));
        let layout = if numeric {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center)
        };
        ui.with_layout(layout, |ui| {
            ui.add(Label::new(text).truncate().selectable(false));
            if let Some(arrow) = arrow {
                ui.label(arrow);
            }
        });
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            self.sort = Some(match sorted {
                Some((_, descending)) => (column, !descending),
                None => (column, numeric),
            });
            return true;
        }
        false
    }

    /// The filter box; `shown` and `total` say how many rows pass. Esc
    /// clears it. Returns true when the filter changed.
    pub fn filter_box(&mut self, ui: &mut Ui, hint: &str, shown: usize, total: usize) -> bool {
        let edit = ui.add(
            TextEdit::singleline(&mut self.filter)
                .hint_text(hint)
                .desired_width(220.0),
        );
        let mut changed = edit.changed();
        if edit.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) && !self.filter.is_empty() {
            self.filter.clear();
            changed = true;
        }
        if !self.filter.is_empty() {
            if ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Clear the filter (Esc)")
                .clicked()
            {
                self.filter.clear();
                changed = true;
            }
            ui.label(
                egui::RichText::new(format!("{shown} of {total}"))
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
            );
        }
        if changed {
            self.words = self
                .filter
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
        }
        changed
    }

    /// Sets the filter text, as typing it would.
    pub fn set_filter(&mut self, text: &str) {
        self.filter = text.to_string();
        self.words = text.split_whitespace().map(str::to_lowercase).collect();
    }

    /// `--detail-sort` on the command line: "2" sorts by the third column,
    /// "2d" the other way round.
    pub fn parse_sort(text: &str) -> Option<(usize, bool)> {
        let descending = text.ends_with('d');
        let column = text.trim_end_matches('d').parse().ok()?;
        Some((column, descending))
    }

    pub fn filtering(&self) -> bool {
        !self.words.is_empty()
    }

    /// Whether every word of the filter is in one of `texts`.
    pub fn matches(&self, texts: &[&str]) -> bool {
        if self.words.is_empty() {
            return true;
        }
        let texts: Vec<String> = texts.iter().map(|t| t.to_lowercase()).collect();
        self.words
            .iter()
            .all(|w| texts.iter().any(|t| t.contains(w.as_str())))
    }

    /// Sorts `rows` by the chosen column; `compare` compares two rows by a
    /// column, ascending.
    pub fn order<T>(&self, rows: &mut [T], compare: impl Fn(&T, &T, usize) -> Ordering) {
        let Some((column, descending)) = self.sort else {
            return;
        };
        rows.sort_by(|a, b| {
            let o = compare(a, b, column);
            if descending { o.reverse() } else { o }
        });
    }
}

/// Column `column`'s title, in its header cell `ui`: one widget for the
/// clicks that sort by it and the drags that move it, since egui gives a
/// click to nothing under a widget that only drags.
fn title_id(ui: &Ui, column: usize) -> Id {
    ui.id().with(("title", column))
}

/// Text order that ignores case.
pub fn text_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// How a column starts: its width, whether what does not fit is cut off
/// (or widens the column), and how narrow it may get. Whichever column
/// shows last fills the rest of the width.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    width: f32,
    clip: bool,
    min: f32,
}

impl Spec {
    pub const fn new(width: f32) -> Self {
        Self {
            width,
            clip: false,
            min: 0.0,
        }
    }

    pub const fn clip(mut self) -> Self {
        self.clip = true;
        self
    }

    pub const fn at_least(mut self, min: f32) -> Self {
        self.min = min;
        self
    }
}

/// Every table's layout: what the settings had, and what changed since.
#[derive(Clone, Default)]
struct Layouts {
    tables: BTreeMap<String, TableLayout>,
    /// The order each table was laid out in last.
    laid: BTreeMap<&'static str, Vec<usize>>,
    loaded: bool,
    /// Not yet back in the settings.
    changed: bool,
}

fn layouts<R>(ctx: &Context, f: impl FnOnce(&mut Layouts) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default(Id::new("table layouts"))))
}

/// Hands the tables the layouts the settings keep, the first time; after
/// that, gives the settings back what was changed. Once a frame.
pub fn sync(ctx: &Context, saved: &mut BTreeMap<String, TableLayout>) {
    layouts(ctx, |l| {
        if !l.loaded {
            l.tables = saved.clone();
            l.loaded = true;
        } else if std::mem::take(&mut l.changed) {
            saved.clone_from(&l.tables);
        }
    });
}

/// A saved order made whole for `count` columns: each column once, and a
/// column the order does not know of (new in this version) after the one
/// it comes after.
fn arrange(saved: &[u8], count: usize) -> Vec<usize> {
    let mut seen = vec![false; count];
    let mut order = Vec::with_capacity(count);
    for &c in saved {
        let c = c as usize;
        if c < count && !std::mem::replace(&mut seen[c], true) {
            order.push(c);
        }
    }
    for (c, _) in seen.iter().enumerate().filter(|(_, seen)| !**seen) {
        let at = match c {
            0 => 0,
            _ => order
                .iter()
                .position(|&o| o == c - 1)
                .map_or(order.len(), |i| i + 1),
        };
        order.insert(at, c);
    }
    order
}

/// `order` with the column at place `from` put down before the one at
/// place `to` (`order.len()`: after the last).
fn moved(order: &[usize], from: usize, to: usize) -> Vec<usize> {
    let mut order = order.to_vec();
    let c = order.remove(from);
    order.insert(if to > from { to - 1 } else { to }, c);
    order
}

/// Widths without the trailing columns that start as they are.
fn trimmed(mut widths: Vec<f32>) -> Vec<f32> {
    while widths.last() == Some(&0.0) {
        widths.pop();
    }
    widths
}

/// How narrow the last column, which fills what is left, may get before the
/// table scrolls sideways instead.
const FILL_MIN: f32 = 100.0;

/// One table's columns as they were left: in the order their titles were
/// dragged into, at the widths they were given. Dragging a title moves its
/// column; both are kept by the table's name, and with the settings.
pub struct Columns<'a> {
    ctx: Context,
    name: &'static str,
    spec: &'a [Spec],
    /// Column numbers (places in `spec`), in the order they show.
    order: Vec<usize>,
    /// The table keeps its order here (not its caller).
    own: bool,
    /// Widths by column number; 0: as `spec` has it.
    widths: Vec<f32>,
    /// The order is not the one the table was laid out in last.
    reset: bool,
    /// The last column, which fills what is left, gets no narrower.
    fill_min: f32,
    /// A title dropped in another place, this frame: the order it makes.
    moved: Option<Vec<usize>>,
}

impl<'a> Columns<'a> {
    /// The columns of the table `name`, in the order saved.
    pub fn load(ctx: &Context, name: &'static str, spec: &'a [Spec]) -> Self {
        let saved = layouts(ctx, |l| l.tables.get(name).map(|t| t.order.clone()));
        let order = arrange(saved.as_deref().unwrap_or_default(), spec.len());
        Self {
            own: true,
            ..Self::ordered(ctx, name, spec, order)
        }
    }

    /// The same, in an order its caller keeps: the process list's is in
    /// its settings, without the columns turned off.
    pub fn ordered(ctx: &Context, name: &'static str, spec: &'a [Spec], order: Vec<usize>) -> Self {
        let (widths, reset) = layouts(ctx, |l| {
            let widths = l.tables.get(name).map(|t| t.widths.clone());
            let reset = l.laid.get(name) != Some(&order);
            if reset {
                l.laid.insert(name, order.clone());
            }
            (widths.unwrap_or_default(), reset)
        });
        Self {
            ctx: ctx.clone(),
            name,
            spec,
            order,
            own: false,
            widths,
            reset,
            fill_min: FILL_MIN,
            moved: None,
        }
    }

    /// The last column, which fills what is left, gets no narrower than
    /// `min`: for a table that scrolls sideways instead.
    pub fn filling_at_least(mut self, min: f32) -> Self {
        self.fill_min = min;
        self
    }

    /// Leaves column `c` out: the table has no use for it here.
    pub fn without(mut self, c: usize) -> Self {
        self.order.retain(|&o| o != c);
        self
    }

    /// Column numbers in the order they show.
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// A title dropped in another place, this frame: the order it makes.
    pub fn moved(&self) -> Option<&[usize]> {
        self.moved.as_deref()
    }

    fn width(&self, c: usize) -> f32 {
        match self.widths.get(c) {
            Some(&w) if w > 0.0 => w,
            _ => self.spec[c].width,
        }
    }

    /// Shows the table: `build` gives the builder its header and body, and
    /// returns what the body returned. Wider than the room, the table
    /// scrolls sideways (Shift and the wheel, or the bar at the bottom);
    /// its own vertical bar is then off to the right, so one shows at the
    /// edge of the room instead.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        build: impl FnOnce(TableBuilder<'_>, &mut Self) -> ScrollAreaOutput<()>,
    ) {
        let id = Id::new((self.name, "wider than the room"));
        let wide = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
        let outer = egui::ScrollArea::horizontal()
            .id_salt((self.name, "sideways"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let builder = self
                    .setup(TableBuilder::new(ui))
                    .scroll_bar_visibility(if wide {
                        ScrollBarVisibility::AlwaysHidden
                    } else {
                        ScrollBarVisibility::VisibleWhenNeeded
                    });
                build(builder, self)
            });
        let now = outer.content_size.x > outer.inner_rect.width() + 1.0;
        ui.data_mut(|d| d.insert_temp(id, now));
        if wide {
            widgets::scroll_bar(ui, outer.inner_rect.right(), &outer.inner);
        }
    }

    /// The table, named, with its columns in order; laid out afresh at
    /// the widths kept when the order is not the one it had.
    fn setup<'b>(&self, builder: TableBuilder<'b>) -> TableBuilder<'b> {
        let mut builder = builder.id_salt(self.name);
        if self.reset {
            builder.reset();
        }
        for (i, &c) in self.order.iter().enumerate() {
            let s = self.spec[c];
            let column = if i + 1 == self.order.len() {
                Column::remainder().at_least(s.min.max(self.fill_min))
            } else {
                Column::initial(self.width(c)).at_least(s.min)
            };
            builder = builder.column(column.clip(s.clip));
        }
        builder
    }

    /// Keeps the widths the columns have (`TableBody::widths`): the table
    /// starts at them when its order changes, and when Anabolic starts.
    pub fn keep(&self, widths: &[f32]) {
        let mut kept = self.widths.clone();
        kept.resize(self.spec.len(), 0.0);
        // The last column fills what is left: its width says nothing.
        let sized = self.order.len().saturating_sub(1);
        for (&c, &w) in self.order.iter().zip(widths).take(sized) {
            kept[c] = if (w - self.spec[c].width).abs() < 0.5 {
                0.0
            } else {
                w.round()
            };
        }
        let kept = trimmed(kept);
        if kept != trimmed(self.widths.clone()) {
            layouts(&self.ctx, |l| {
                l.tables.entry(self.name.to_string()).or_default().widths = kept;
                l.changed = true;
            });
        }
    }

    /// The titles, in order: `cell` draws column `c`'s (`TableView::header`
    /// sorts by it on a click). Gives back each title's column and the
    /// response of the whole cell, which is clicked and dragged.
    pub fn header(
        &mut self,
        h: &mut TableRow<'_, '_>,
        mut cell: impl FnMut(&mut Ui, usize),
    ) -> Vec<(usize, Response)> {
        let mut titles = Vec::with_capacity(self.order.len());
        let mut clip = Rect::NOTHING;
        for &c in &self.order {
            h.col(|ui| {
                // Under what `cell` adds, which only shows.
                let title = ui.interact(ui.max_rect(), title_id(ui, c), Sense::click_and_drag());
                cell(ui, c);
                clip = clip.union(ui.clip_rect());
                titles.push((c, title));
            });
        }
        self.drag(&titles, clip);
        titles
    }

    /// While a title is dragged: where it would go. Dropped elsewhere: its
    /// column goes there.
    fn drag(&mut self, titles: &[(usize, Response)], clip: Rect) {
        let grips: Vec<&Response> = titles.iter().map(|(_, r)| r).collect();
        let Some(from) = grips.iter().position(|g| g.dragged() || g.drag_stopped()) else {
            return;
        };
        let grip = grips[from];
        let Some(pointer) = grip.ctx.pointer_latest_pos() else {
            return;
        };
        // Before the first title whose middle is right of the pointer.
        let to = grips
            .iter()
            .position(|g| pointer.x < g.rect.center().x)
            .unwrap_or(grips.len());
        let stays = to == from || to == from + 1;
        if grip.dragged() {
            grip.ctx.set_cursor_icon(CursorIcon::Grabbing);
            let painter = grip
                .ctx
                .layer_painter(LayerId::new(
                    egui::Order::Foreground,
                    Id::new((self.name, "column drop")),
                ))
                .with_clip_rect(clip);
            painter.rect_filled(grip.rect, 0.0, theme::ACCENT.gamma_multiply(0.16));
            if !stays {
                let x = match to {
                    0 => grips[0].rect.left(),
                    _ if to == grips.len() => grips[to - 1].rect.right(),
                    _ => (grips[to - 1].rect.right() + grips[to].rect.left()) / 2.0,
                };
                painter.vline(x, grip.rect.y_range(), Stroke::new(2.0, theme::ACCENT));
            }
        } else if !stays {
            let order = moved(&self.order, from, to);
            if self.own {
                let saved = order.iter().map(|&c| c as u8).collect();
                layouts(&self.ctx, |l| {
                    l.tables.entry(self.name.to_string()).or_default().order = saved;
                    l.changed = true;
                });
            }
            self.moved = Some(order);
        }
    }

    /// One menu for all the titles: a right click on any opens it. It stays
    /// open while what is in it is clicked, so several columns can be
    /// turned on and off at once.
    pub fn menu(&mut self, titles: &[(usize, Response)], add: impl FnOnce(&mut Ui, &mut Self)) {
        let Some((_, anchor)) = titles
            .iter()
            .find(|(_, r)| r.secondary_clicked())
            .or(titles.first())
        else {
            return;
        };
        Popup::context_menu(anchor)
            .id(Id::new((self.name, "columns menu")))
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| add(ui, self));
    }

    /// The titles and their menu, for a table whose columns are not turned
    /// on and off.
    pub fn titles(&mut self, h: &mut TableRow<'_, '_>, cell: impl FnMut(&mut Ui, usize)) {
        let titles = self.header(h, cell);
        self.menu(&titles, |ui, columns| {
            ui.label(
                RichText::new("Drag a title to move its column")
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
            );
            ui.separator();
            let usual = columns.order.is_sorted() && columns.widths.iter().all(|&w| w == 0.0);
            if ui
                .add_enabled(!usual, egui::Button::new("Reset columns"))
                .clicked()
            {
                columns.forget();
                ui.close();
            }
        });
    }

    /// Back to the usual order and widths.
    pub fn forget(&mut self) {
        layouts(&self.ctx, |l| {
            if l.tables.remove(self.name).is_some() {
                l.changed = true;
            }
            // Laid out afresh next time, even in the same order.
            l.laid.remove(self.name);
        });
        if self.own {
            self.moved = Some((0..self.spec.len()).collect());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_every_word_and_sorts_both_ways() {
        let mut view = TableView {
            filter: "Nt dll".into(),
            ..Default::default()
        };
        view.words = view
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        assert!(view.matches(&["ntdll.dll", "C:\\Windows"]));
        assert!(view.matches(&["NtWait", "kernel32.dll"]));
        assert!(!view.matches(&["kernel32.dll"]));

        let mut rows = vec![3, 1, 2];
        view.sort = Some((0, false));
        view.order(&mut rows, |a, b, _| a.cmp(b));
        assert_eq!(rows, [1, 2, 3]);
        view.sort = Some((0, true));
        view.order(&mut rows, |a, b, _| a.cmp(b));
        assert_eq!(rows, [3, 2, 1]);
        assert_eq!(text_cmp("abc", "ABD"), Ordering::Less);
    }

    #[test]
    fn a_saved_order_keeps_each_column_once_and_takes_in_new_ones() {
        assert_eq!(arrange(&[], 3), [0, 1, 2]);
        assert_eq!(arrange(&[2, 0, 1], 3), [2, 0, 1]);
        // Twice, or no longer there: dropped.
        assert_eq!(arrange(&[1, 1, 7, 0, 2], 3), [1, 0, 2]);
        // Columns 3 and 4 are new: each after the one it comes after.
        assert_eq!(arrange(&[2, 0, 1], 5), [2, 3, 4, 0, 1]);
        assert_eq!(arrange(&[1, 0, 2], 4), [1, 0, 2, 3]);
        assert_eq!(arrange(&[3, 1, 2], 4), [0, 3, 1, 2]);
        assert_eq!(trimmed(vec![0.0, 120.0, 0.0, 0.0]), [0.0, 120.0]);
    }

    #[test]
    fn a_dropped_title_goes_where_it_was_dropped() {
        let order = [0, 1, 2, 3];
        // The first, dropped before the fourth: after the third.
        assert_eq!(moved(&order, 0, 3), [1, 2, 0, 3]);
        // After the last.
        assert_eq!(moved(&order, 1, 4), [0, 2, 3, 1]);
        // Before the first.
        assert_eq!(moved(&order, 3, 0), [3, 0, 1, 2]);
        assert_eq!(moved(&[2, 0, 1], 2, 1), [2, 1, 0]);
    }
}
