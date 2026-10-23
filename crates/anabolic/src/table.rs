// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Sorting and filtering for the tables in the details panel: a click on a
//! column title sorts by it (again: the other way round), and a filter box
//! keeps the rows that contain every word typed.

use std::cmp::Ordering;

use eframe::egui::{self, Align, CursorIcon, Key, Label, Layout, Sense, TextEdit, Ui};

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
        // The whole cell is the button, not only the letters.
        let response = ui.interact(
            ui.max_rect(),
            ui.id().with(("sort", column)),
            Sense::click(),
        );
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

/// Text order that ignores case.
pub fn text_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
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
}
