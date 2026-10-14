//! Fixed-width building blocks. Values change every refresh, and columns
//! sized to their content would wobble with them.

use eframe::egui::{
    self, Align, Align2, CursorIcon, FontId, Label, Layout, Rect, RichText, Sense, Shape, Ui,
    WidgetText, pos2, vec2,
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

/// An accent coloured label that behaves like a link but, unlike one,
/// truncates (the full text shows on hover).
pub fn link(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        Label::new(RichText::new(text).color(theme::ACCENT))
            .truncate()
            .sense(Sense::click()),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
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
    for i in from..to {
        row(ui, i, |ui| text(ui, width, Align::Min, " "));
    }
}

pub fn header(ui: &mut Ui, columns: &[(&str, f32, Align)]) {
    ui.horizontal(|ui| {
        for (title, width, align) in columns {
            text(
                ui,
                *width,
                *align,
                RichText::new(*title).small().color(theme::TEXT_WEAK),
            );
        }
    });
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
    let painter = ui.painter();
    if selected || response.hovered() {
        let fill = if selected {
            theme::SELECTION
        } else {
            theme::RAISED
        };
        painter.rect_filled(rect.shrink2(vec2(0.0, 2.0)), 6.0, fill);
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
        FontId::proportional(17.0),
        color,
    );
    if !collapsed {
        painter.text(
            pos2(rect.left() + RAIL_ICON_X * 2.0 + 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.0),
            color,
        );
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// An icon-only button the size of a rail entry's icon column.
pub fn rail_icon(ui: &mut Ui, icon: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(RAIL_ICON_X * 2.0, 28.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, theme::RAISED);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(17.0),
        theme::TEXT_WEAK,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A small caption above a big value, in a fixed-size block.
pub fn stat(ui: &mut Ui, width: f32, caption: &str, value: impl Into<WidgetText>) {
    ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.label(RichText::new(caption).small().color(theme::TEXT_WEAK));
        ui.add(Label::new(value).truncate());
    });
}
