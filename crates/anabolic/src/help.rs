//! F1: the keyboard shortcuts, and what the colours and marks mean, in one
//! place instead of scattered over tooltips.

use eframe::egui::{
    self, Align, Color32, Frame, Grid, Id, Label, Layout, Margin, Modal, RichText, Sense, Ui, vec2,
};

use crate::{theme, widgets};

const SHORTCUTS: [(&str, &[(&str, &str)]); 3] = [
    (
        "Everywhere",
        &[
            (
                "Ctrl+1 … 9, 0",
                "Processes, Performance, History, Rules, Services, Startup, Stutters, Network, \
                 Activity, MCP",
            ),
            (
                "Alt+← →",
                "Back and forward through the pages and processes visited; the mouse's back and \
                 forward buttons too",
            ),
            ("Ctrl+H", "Find which process holds a file or handle"),
            ("Space", "Pause or resume the view"),
            ("F5", "Refresh now"),
            ("F1", "This page"),
            ("Ctrl+,", "Settings"),
            (
                "Ctrl+Alt+Esc",
                "From any program: Anabolic to the front (other keys in Settings)",
            ),
        ],
    ),
    (
        "Process list",
        &[
            ("Ctrl+F", "Filter by name, PID, path or user"),
            ("Esc", "Clear the filter, then the selection"),
            ("Ctrl+T", "Tree, list or apps"),
            ("↑ ↓ PgUp PgDn", "Move the selection"),
            ("← →", "Collapse or expand"),
            ("Del", "End the process (or the app)"),
            ("Shift+Del", "End the process and its children"),
            ("Shift+F10", "The row's menu"),
        ],
    ),
    (
        "Tables in the details",
        &[
            ("Click a title", "Sort by it; again for the other way"),
            ("Right click", "What can be done with the row"),
            ("Esc in a filter", "Clear it"),
        ],
    ),
];

fn key(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
        .inner_margin(Margin::symmetric(6, 1))
        .corner_radius(3)
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(theme::META).color(theme::TEXT));
        });
}

fn shortcuts(ui: &mut Ui) {
    for (section, rows) in SHORTCUTS {
        ui.label(widgets::strong(section, theme::BODY));
        ui.add_space(2.0);
        Grid::new(("help-keys", section))
            .num_columns(2)
            .spacing([10.0, 5.0])
            .min_col_width(110.0)
            .show(ui, |ui| {
                for (k, what) in rows {
                    key(ui, k);
                    ui.add(Label::new(RichText::new(*what).color(theme::TEXT_WEAK)).wrap());
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
    }
}

/// A sample of a mark, drawn in the cell before its meaning.
enum Sample {
    Name(Color32, bool),
    Row(Color32),
    Heat(f32),
    Icon(&'static str, Color32),
    Text(&'static str, Color32),
}

fn sample(ui: &mut Ui, s: &Sample) {
    let size = vec2(96.0, 18.0);
    match s {
        Sample::Name(color, italic) => {
            let mut text = RichText::new("program.exe").color(*color);
            if *italic {
                text = text.italics();
            }
            ui.add_sized(size, Label::new(text));
        }
        Sample::Row(color) => {
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect_filled(rect, 2.0, *color);
        }
        Sample::Heat(level) => {
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect_filled(rect, 0.0, theme::heat(*level));
            ui.painter().text(
                rect.right_center() - vec2(6.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                if *level > 0.5 { "48.20" } else { "6.10" },
                egui::FontId::proportional(theme::BODY),
                theme::TEXT,
            );
        }
        Sample::Icon(glyph, color) => {
            ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                ui.label(widgets::icon(glyph, 12.0).color(*color));
            });
        }
        Sample::Text(text, color) => {
            ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                ui.label(RichText::new(*text).color(*color));
            });
        }
    }
}

fn legend(ui: &mut Ui) {
    let sections: [(&str, Vec<(Sample, &str)>); 3] = [
        (
            "Names",
            vec![
                (Sample::Name(theme::TEXT, false), "An ordinary program"),
                (
                    Sample::Name(theme::SERVICE_TEXT, false),
                    if cfg!(windows) {
                        "A service (runs in session 0)"
                    } else {
                        "A service (runs outside any login session)"
                    },
                ),
                (
                    Sample::Name(theme::ELEVATED_TEXT, false),
                    if cfg!(windows) {
                        "Runs as administrator"
                    } else {
                        "Runs as root, or with capabilities"
                    },
                ),
                (Sample::Name(theme::SUSPENDED_TEXT, true), "Suspended"),
                (
                    Sample::Icon(theme::icon::WARNING, theme::BAD),
                    "Not responding: a window took no message for 5 s",
                ),
                if cfg!(windows) {
                    (
                        Sample::Text("ending", theme::ELEVATED_TEXT),
                        "Windows is ending it but cannot finish; the details say why",
                    )
                } else {
                    (
                        Sample::Text("zombie", theme::ELEVATED_TEXT),
                        "It has ended, but its parent has not collected it; the details say which",
                    )
                },
            ],
        ),
        (
            "Rows and cells",
            vec![
                (Sample::Row(theme::NEW_ROW), "Just started"),
                (
                    Sample::Row(theme::EXITED_ROW),
                    "Just exited; stays a moment so it is seen to go",
                ),
                (Sample::Heat(0.3), "A real share of the machine…"),
                (
                    Sample::Heat(0.85),
                    "…and a big one. CPU against a quarter of all cores, memory against a quarter \
                     of RAM, transfers from 1 MB/s",
                ),
            ],
        ),
        (
            "Empty and missing",
            vec![
                (
                    Sample::Text("", theme::TEXT),
                    "Blank: zero, or nothing to show",
                ),
                (
                    Sample::Text("—", theme::TEXT_DIM),
                    if cfg!(windows) {
                        "Windows did not let Anabolic read it; as administrator it reads most"
                    } else {
                        "Linux did not let Anabolic read it; with full access it reads most"
                    },
                ),
                (
                    Sample::Icon(theme::icon::LOCK, theme::TEXT_DIM),
                    crate::platform::NEEDS_ELEVATION,
                ),
            ],
        ),
    ];
    for (section, rows) in &sections {
        ui.label(widgets::strong(*section, theme::BODY));
        ui.add_space(2.0);
        Grid::new(("help-legend", *section))
            .num_columns(2)
            .spacing([10.0, 5.0])
            .show(ui, |ui| {
                for (s, what) in rows {
                    sample(ui, s);
                    ui.add(Label::new(RichText::new(*what).color(theme::TEXT_WEAK)).wrap());
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
    }
}

/// The help page as a dialog; `open` turns false when it is closed.
pub fn show(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    let mut close = false;
    let response = Modal::new(Id::new("help")).show(ctx, |ui| {
        ui.set_width(820.0);
        ui.horizontal(|ui| {
            ui.label(widgets::strong("Shortcuts and colours", theme::SUBTITLE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(widgets::icon(theme::icon::CLOSE, 12.0))
                    .on_hover_text("Close (Esc)")
                    .clicked()
                {
                    close = true;
                }
            });
        });
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            shortcuts(&mut cols[0]);
            legend(&mut cols[1]);
        });
    });
    if close || response.should_close() {
        *open = false;
    }
}
