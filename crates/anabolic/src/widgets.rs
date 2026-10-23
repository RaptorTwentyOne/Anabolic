// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Fixed-width building blocks. Values change every refresh, and columns
//! sized to their content would wobble with them.

use eframe::egui::{
    self, Align, Align2, CursorIcon, FontId, Label, Layout, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, Ui, WidgetInfo, WidgetText, WidgetType, pos2, vec2,
};

use crate::theme;

pub const ROW: f32 = 18.0;

/// Lays out `add` in a cell exactly `width` wide; text in it truncates.
pub fn cell<R>(ui: &mut Ui, width: f32, align: Align, add: impl FnOnce(&mut Ui) -> R) -> R {
    let layout = match align {
        Align::Max => Layout::right_to_left(Align::Center),
        _ => Layout::left_to_right(Align::Center),
    };
    ui.allocate_ui_with_layout(vec2(width, ROW), layout, |ui| {
        ui.set_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        add(ui)
    })
    .inner
}

pub fn text(ui: &mut Ui, width: f32, align: Align, text: impl Into<WidgetText>) {
    cell(ui, width, align, |ui| {
        ui.add(Label::new(text).truncate());
    });
}

/// Text that behaves like a link: plain until hovered, then accent and
/// underlined. Unlike a hyperlink it truncates (the full text shows on hover).
pub fn link(ui: &mut Ui, text: &str) -> egui::Response {
    let id = ui.next_auto_id();
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let rich = if hovered {
        RichText::new(text).color(theme::ACCENT).underline()
    } else {
        RichText::new(text).color(theme::TEXT)
    };
    let response = ui
        .add(Label::new(rich).truncate().sense(Sense::click()))
        .on_hover_cursor(CursorIcon::PointingHand);
    focus_ring(ui, &response);
    response
}

/// Semibold text: titles, table headers, the values that lead a group.
pub fn strong(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(theme::semibold(size))
}

/// A table's column title, alike in every table: right-aligned over numbers
/// so it sits above them.
pub fn column_header(ui: &mut Ui, title: &str, numeric: bool) {
    let text = Label::new(strong(title, theme::META).color(theme::TEXT_WEAK)).truncate();
    if numeric {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(text);
        });
    } else {
        ui.add(text);
    }
}

/// The keyboard focus ring for custom-painted controls.
pub fn focus_ring(ui: &Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            theme::RADIUS_CONTROL,
            Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Outside,
        );
    }
}

/// A row of cells; odd rows get a stripe behind them.
pub fn row(ui: &mut Ui, index: usize, add: impl FnOnce(&mut Ui)) {
    let background = ui.painter().add(Shape::Noop);
    let rect = ui.horizontal(add).response.rect;
    if index % 2 == 1 {
        ui.painter().set(
            background,
            Shape::rect_filled(rect.expand2(vec2(3.0, 0.0)), 2.0, theme::STRIPE),
        );
    }
}

/// Blank rows `from..to`, so a live table keeps its height as entries come
/// and go: a page that changes height jumps under a scrolling user.
pub fn blank_rows(ui: &mut Ui, from: usize, to: usize, width: f32) {
    // No stripes: striped empty rows read as data that is missing.
    for _ in from..to {
        ui.horizontal(|ui| text(ui, width, Align::Min, " "));
    }
}

/// The one table header style: semibold meta size, weak, a rule beneath.
pub fn header(ui: &mut Ui, columns: &[(&str, f32, Align)]) {
    let response = ui.horizontal(|ui| {
        for (title, width, align) in columns {
            text(
                ui,
                *width,
                *align,
                strong(*title, theme::META).color(theme::TEXT_WEAK),
            );
        }
    });
    let rect = response.response.rect;
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() + 1.0,
        Stroke::new(1.0, theme::BORDER),
    );
    ui.add_space(2.0);
}

/// A section heading inside a page: semibold, primary text.
pub fn section(ui: &mut Ui, title: &str) {
    ui.label(strong(title, theme::BODY));
}

const RAIL_ROW: f32 = 36.0;
/// Centre of the icon column, from the rail's left edge.
const RAIL_ICON_X: f32 = 20.0;

/// An entry of the navigation rail: an icon, and its label unless collapsed.
/// The selected one gets a highlight and an accent mark.
pub fn rail_item(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    selected: bool,
    collapsed: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), RAIL_ROW), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));
    let painter = ui.painter();
    if selected || response.hovered() {
        let fill = if selected {
            theme::SELECTION
        } else {
            theme::HOVER
        };
        painter.rect_filled(rect.shrink2(vec2(0.0, 2.0)), theme::RADIUS_PANEL, fill);
    }
    if selected {
        let mark = Rect::from_min_size(
            pos2(rect.left(), rect.top() + 10.0),
            vec2(3.0, RAIL_ROW - 20.0),
        );
        painter.rect_filled(mark, 1.5, theme::ACCENT);
    }
    let color = if selected {
        theme::TEXT
    } else {
        theme::TEXT_WEAK
    };
    painter.text(
        pos2(rect.left() + RAIL_ICON_X, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        theme::icon_font(16.0),
        color,
    );
    if !collapsed {
        let font = if selected {
            theme::semibold(theme::BODY)
        } else {
            FontId::proportional(theme::BODY)
        };
        painter.text(
            pos2(rect.left() + RAIL_ICON_X * 2.0 + 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            color,
        );
    }
    focus_ring(ui, &response);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// An icon-only button the size of a rail entry's icon column.
pub fn rail_icon(ui: &mut Ui, icon: &str, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(RAIL_ICON_X * 2.0, 28.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_PANEL, theme::HOVER);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        theme::icon_font(16.0),
        theme::TEXT_WEAK,
    );
    focus_ring(ui, &response);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// An icon glyph inline with text.
pub fn icon(glyph: &str, size: f32) -> RichText {
    RichText::new(glyph).font(theme::icon_font(size))
}

/// A button caption: an icon, then its label.
pub fn icon_text(ui: &Ui, glyph: &str, label: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let style = ui.style();
    icon(glyph, 12.0).append_to(&mut job, style, egui::FontSelection::Default, Align::Center);
    RichText::new(format!("  {label}")).append_to(
        &mut job,
        style,
        egui::FontSelection::Default,
        Align::Center,
    );
    job
}

/// A small caption above a big value, in a fixed-size block.
pub fn stat(ui: &mut Ui, width: f32, caption: &str, value: impl Into<WidgetText>) {
    ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.label(
            RichText::new(caption)
                .size(theme::CAPTION)
                .color(theme::TEXT_WEAK),
        );
        ui.add(Label::new(value).truncate());
    });
}
