//! The MCP page (Ctrl+0): the server that lets AI assistants (Claude and
//! other Model Context Protocol clients) read what Anabolic sees and do what
//! the user allows. Turning it on, what clients may do, the access key and
//! how to connect, and what clients did this session.

use eframe::egui::{self, RichText, Ui};

use crate::app::mcp::{McpState, McpStatus};
use crate::proc_table::UiCmd;
use crate::settings::{McpKind, McpSettings};
use crate::{fmt, theme, widgets};

pub fn show(
    ui: &mut Ui,
    settings: &mut McpSettings,
    mcp: &mut McpState,
    elevated: bool,
    out: &mut Vec<UiCmd>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(widgets::strong("MCP server", theme::TITLE));
            ui.label(
                RichText::new(
                    "Lets AI assistants (Claude, and any other Model Context Protocol client) \
                     read what Anabolic sees and do what you allow below. Only programs on this \
                     PC can reach it, and only with the access key.",
                )
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(10.0);
            card(ui, |ui| server_card(ui, settings, mcp, elevated));
            ui.add_space(8.0);
            card(ui, |ui| permissions_card(ui, settings));
            ui.add_space(8.0);
            card(ui, |ui| connect_card(ui, settings, mcp, out));
            ui.add_space(8.0);
            card(ui, |ui| calls_card(ui, mcp));
        });
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(
        RichText::new(text.into())
            .size(theme::META)
            .color(theme::TEXT_WEAK),
    );
}

fn server_card(ui: &mut Ui, settings: &mut McpSettings, mcp: &mut McpState, elevated: bool) {
    ui.horizontal(|ui| {
        let was = settings.enabled;
        ui.checkbox(
            &mut settings.enabled,
            widgets::strong("Let MCP clients connect", theme::SUBTITLE),
        );
        // The key is made the first time the server is turned on.
        if settings.enabled && !was && settings.token.is_empty() {
            settings.token = anabolic_core::random_hex(32);
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Port");
        ui.add(egui::DragValue::new(&mut settings.port).range(1024..=65535));
        note(ui, "on 127.0.0.1 only");
    });
    ui.add_space(4.0);
    match &mcp.status {
        McpStatus::Off if !mcp.serves => note(
            ui,
            "This is a second Anabolic started with --new: the first one serves MCP clients.",
        ),
        McpStatus::Off => note(ui, "Off: no client can reach Anabolic."),
        McpStatus::Listening(addr) => {
            ui.label(RichText::new(format!("Listening at http://{addr}/mcp")).color(theme::GOOD));
        }
        McpStatus::Failed(e) => {
            ui.label(RichText::new(format!("Could not start: {e}")).color(theme::BAD));
            if ui.button("Try again").clicked() {
                mcp.retry();
            }
        }
    }
    if elevated && settings.enabled {
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Anabolic runs as administrator: what you allow, clients do with administrator \
                 rights.",
            )
            .size(theme::META)
            .color(theme::ELEVATED_TEXT),
        );
    }
}

fn permissions_card(ui: &mut Ui, settings: &mut McpSettings) {
    ui.horizontal(|ui| {
        ui.label(widgets::strong("What clients may do", theme::SUBTITLE));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Everything").clicked() {
                settings.allowed = McpKind::ALL.to_vec();
            }
            if ui.small_button("Reading only").clicked() {
                settings.allowed = vec![McpKind::Read];
            }
        });
    });
    note(
        ui,
        "Each kind is allowed or not; a call of a kind that is off is refused, with where to \
         turn it on. Nothing waits for an answer here: tell the assistant what you agree to.",
    );
    ui.add_space(6.0);
    for kind in McpKind::ALL {
        let mut on = settings.allows(kind);
        if ui.checkbox(&mut on, kind.label()).changed() {
            if on {
                settings.allowed.push(kind);
            } else {
                settings.allowed.retain(|k| *k != kind);
            }
        }
        // Indented under its checkbox, and wrapped to the card's width.
        ui.indent(("mcp-kind", kind.label()), |ui| {
            ui.label(
                RichText::new(kind.detail())
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
            );
        });
        ui.add_space(3.0);
    }
}

fn connect_card(ui: &mut Ui, settings: &mut McpSettings, mcp: &mut McpState, out: &mut Vec<UiCmd>) {
    ui.label(widgets::strong("Connecting a client", theme::SUBTITLE));
    if settings.token.is_empty() {
        note(
            ui,
            "The access key is made when the server is first turned on.",
        );
        return;
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Access key");
        let shown = if mcp.show_key {
            settings.token.clone()
        } else {
            "•".repeat(24)
        };
        ui.label(RichText::new(shown).monospace());
        if ui
            .small_button(if mcp.show_key { "Hide" } else { "Show" })
            .clicked()
        {
            mcp.show_key = !mcp.show_key;
        }
        if ui.small_button("Copy").clicked() {
            out.push(UiCmd::Copy(settings.token.clone()));
        }
        if ui
            .small_button("New key")
            .on_hover_text("Clients with the old key are refused until they get the new one.")
            .clicked()
        {
            settings.token = anabolic_core::random_hex(32);
        }
    });
    ui.add_space(8.0);
    let command = crate::mcp::claude_command(settings.port, &settings.token);
    ui.horizontal(|ui| {
        ui.label("Claude Code");
        if ui.small_button("Copy").clicked() {
            out.push(UiCmd::Copy(command.clone()));
        }
    });
    code(ui, &command, mcp.show_key, &settings.token);
    ui.add_space(8.0);
    let json = crate::mcp::json_config(settings.port, &settings.token);
    ui.horizontal(|ui| {
        ui.label("Other clients (Claude Desktop, Cursor, VS Code...)");
        if ui.small_button("Copy").clicked() {
            out.push(UiCmd::Copy(json.clone()));
        }
    });
    code(ui, &json, mcp.show_key, &settings.token);
    note(
        ui,
        "Streamable HTTP at /mcp; every request carries the key as Authorization: Bearer.",
    );
}

/// A configuration snippet, with the key hidden unless it is shown above.
fn code(ui: &mut Ui, text: &str, show_key: bool, key: &str) {
    let text = if show_key {
        text.to_string()
    } else {
        text.replace(key, "<access key>")
    };
    egui::Frame::new()
        .fill(theme::PANEL)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).monospace().size(theme::META));
        });
}

fn calls_card(ui: &mut Ui, mcp: &McpState) {
    ui.label(widgets::strong("This session", theme::SUBTITLE));
    if mcp.clients.is_empty() {
        note(ui, "No client has connected yet.");
    } else {
        for c in &mcp.clients {
            note(
                ui,
                format!(
                    "{} {} connected at {}",
                    c.name,
                    c.version,
                    fmt::clock(c.since, false)
                ),
            );
        }
    }
    ui.add_space(6.0);
    if mcp.calls.is_empty() {
        note(ui, "No calls yet.");
        return;
    }
    egui::Grid::new("mcp-calls")
        .num_columns(5)
        .spacing([14.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for title in ["Time", "Client", "Tool", "Took", "Result"] {
                ui.label(
                    RichText::new(title)
                        .size(theme::META)
                        .color(theme::TEXT_WEAK),
                );
            }
            ui.end_row();
            for call in mcp.calls.iter().take(100) {
                ui.label(RichText::new(fmt::clock(call.at, false)).size(theme::META));
                ui.label(RichText::new(&call.client).size(theme::META));
                ui.label(RichText::new(&call.tool).monospace().size(theme::META));
                ui.label(RichText::new(format!("{} ms", call.took.as_millis())).size(theme::META));
                if call.ok {
                    ui.label(RichText::new("done").size(theme::META).color(theme::GOOD));
                } else {
                    let first = call.note.lines().next().unwrap_or("failed");
                    let short: String = first.chars().take(90).collect();
                    ui.label(RichText::new(short).size(theme::META).color(theme::BAD))
                        .on_hover_text(&call.note);
                }
                ui.end_row();
            }
        });
}
