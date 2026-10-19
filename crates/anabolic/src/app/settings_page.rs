//! The Settings page (Ctrl+,): every preference in one column of sections,
//! each setting with what it does in words, and what this copy of Anabolic
//! is (version, rights, where it keeps its files).

use egui::{Align, Layout, Ui, vec2};

use super::*;
use crate::settings::Hotkey;

/// The page's column: wide enough for a setting and its control, narrow
/// enough to read the descriptions.
const PAGE_WIDTH: f32 = 780.0;
/// The column the controls line up in.
const CONTROL_WIDTH: f32 = 250.0;

/// What the hotkey setting says it does.
#[cfg(windows)]
const HOTKEY_HINT: &str = "From any program, while Anabolic runs: in the notification area too, \
                           when it keeps running there. Windows' Task Manager and Ctrl+Shift+Esc \
                           stay as they are.";
#[cfg(target_os = "linux")]
const HOTKEY_HINT: &str = "From any program, while Anabolic runs: in the notification area too, \
                           when it keeps running there. The desktop asks once whether Anabolic \
                           may have the key, and lets you pick another in its shortcut settings.";

/// A section: more room above the heading than below, and a hairline.
pub(super) fn heading(ui: &mut Ui, title: &str) {
    ui.add_space(26.0);
    ui.label(widgets::strong(title, theme::SUBTITLE));
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::BORDER);
    ui.add_space(10.0);
}

/// One setting: its name and what it does on the left, wrapped to the
/// column; its control on the right, in a column of its own.
pub(super) fn row(ui: &mut Ui, name: &str, about: &str, control: impl FnOnce(&mut Ui)) {
    let text_width = (ui.available_width() - CONTROL_WIDTH - 20.0).max(260.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(text_width);
            ui.label(RichText::new(name).color(theme::TEXT));
            if !about.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(about)
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                    )
                    .wrap(),
                );
            }
        });
        ui.add_space(20.0);
        ui.allocate_ui_with_layout(
            vec2(CONTROL_WIDTH, 0.0),
            Layout::top_down(Align::Min),
            control,
        );
    });
    ui.add_space(12.0);
}

/// A warning under a control, in the color of things to mind.
fn caution(ui: &mut Ui, text: impl Into<String>) {
    ui.add(
        egui::Label::new(
            RichText::new(text.into())
                .size(theme::CAPTION)
                .color(theme::ELEVATED_TEXT),
        )
        .wrap(),
    );
}

impl App {
    pub(super) fn settings_page(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(PAGE_WIDTH);
                ui.label(widgets::strong("Settings", theme::TITLE));
                ui.label(RichText::new("Changes take effect at once.").color(theme::TEXT_WEAK));
                if self.session_only {
                    ui.add_space(6.0);
                    caution(
                        ui,
                        "Anabolic was started with options on its command line: changes made \
                         now last for this session only and are not saved.",
                    );
                }
                self.window_section(ui);
                self.process_section(ui);
                self.history_section(ui);
                self.stutter_section(ui);
                self.dump_section(ui);
                self.assistant_section(ui);
                self.updates_section(ui);
                self.about_section(ui);
                ui.add_space(24.0);
            });
    }

    fn window_section(&mut self, ui: &mut Ui) {
        heading(ui, "Window");
        row(ui, "Bring to front with", HOTKEY_HINT, |ui| {
            egui::ComboBox::from_id_salt("settings-hotkey")
                .selected_text(self.settings.hotkey.label())
                .width(180.0)
                .show_ui(ui, |ui| {
                    for key in Hotkey::ALL {
                        ui.selectable_value(&mut self.settings.hotkey, key, key.label());
                    }
                });
            if let Some(e) = &self.hotkey_error {
                caution(
                    ui,
                    format!("{}: {e}. Pick another.", self.settings.hotkey.label()),
                );
            }
        });
        row(
            ui,
            "Keep running in the notification area when closed",
            "Closing the window leaves Anabolic running: it keeps recording the history, \
             applying your rules and answering its hotkey. Its icon opens it again; right-click \
             it to quit.",
            |ui| {
                ui.checkbox(&mut self.settings.close_to_tray, "");
                if self.settings.close_to_tray && self.tray_failed {
                    caution(
                        ui,
                        "There is no notification area here, so closing still quits.",
                    );
                }
            },
        );
    }

    fn process_section(&mut self, ui: &mut Ui) {
        heading(ui, "Processes");
        row(
            ui,
            "Refresh every",
            "How often every number is read. Faster costs a little more CPU; the top bar has the \
             same choice.",
            |ui| {
                let label = INTERVALS
                    .iter()
                    .find(|(ms, _)| *ms == self.settings.interval_ms)
                    .map_or("custom", |(_, l)| *l);
                egui::ComboBox::from_id_salt("settings-interval")
                    .selected_text(label)
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for (ms, label) in INTERVALS {
                            if ui
                                .selectable_value(&mut self.settings.interval_ms, ms, label)
                                .changed()
                            {
                                self.collector
                                    .send(Command::SetInterval(Duration::from_millis(ms)));
                            }
                        }
                    });
            },
        );
        row(
            ui,
            "Ask before terminating processes",
            "Ending a process loses its unsaved work: a question first, unless you turn it off.",
            |ui| {
                ui.checkbox(&mut self.settings.confirm_kill, "");
            },
        );
        row(
            ui,
            "Memory warning at",
            "A bar names the program behind it when this much memory is in use.",
            |ui| {
                ui.add(
                    egui::Slider::new(&mut self.settings.memory_alert_pct, 50.0..=99.0)
                        .suffix(" %")
                        .integer(),
                );
            },
        );
    }

    fn history_section(&mut self, ui: &mut Ui) {
        heading(ui, "Graphs and history");
        row(
            ui,
            "Graphs show",
            "Fit stretches each graph over what was collected so far. A fixed span keeps its \
             length from the start, new values flowing in from the right.",
            |ui| {
                egui::ComboBox::from_id_salt("settings-span")
                    .selected_text(self.settings.graph_span.label())
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for span in GraphSpan::ALL {
                            ui.selectable_value(&mut self.settings.graph_span, span, span.label());
                        }
                    });
            },
        );
        row(
            ui,
            "Keep a history",
            "Every 10 seconds while Anabolic runs: the machine's totals and the processes that \
             led, for the History page (Ctrl+3) and \"what happened at 14:32\". Process names \
             and command lines are kept, on this PC only.",
            |ui| {
                ui.horizontal(|ui| {
                    let mut changed = ui.checkbox(&mut self.settings.record_history, "").changed();
                    let days = ui.add_enabled(
                        self.settings.record_history,
                        egui::DragValue::new(&mut self.settings.history_days)
                            .range(1..=90)
                            .suffix(" days"),
                    );
                    // Not on every step of a drag: each change restarts the recorder.
                    changed |= days.drag_stopped() || (days.changed() && !days.dragged());
                    if changed {
                        self.collector
                            .send(Command::Record(recorder_config(&self.settings)));
                    }
                });
            },
        );
        if let Some(dir) = crate::data_dir() {
            let shown = dir.display().to_string();
            row(ui, "Where Anabolic keeps its files", &shown, |ui| {
                if ui.button(crate::platform::SHOW_IN_FILE_MANAGER).clicked() {
                    self.actions.run(Action::OpenFileLocation(shown.clone()));
                }
            });
        }
    }

    fn stutter_section(&mut self, ui: &mut Ui) {
        heading(ui, "Stutters");
        row(
            ui,
            "Keep watching with the Stutters page closed",
            "Frame times and driver latency are traced all the time, so a game played with the \
             window closed is measured too. Costs a little CPU while it runs.",
            |ui| {
                ui.checkbox(&mut self.settings.watch_stutters, "");
            },
        );
    }

    fn dump_section(&mut self, ui: &mut Ui) {
        heading(ui, "Memory dumps");
        #[cfg(windows)]
        let (what, why, now) = (
            "Allow dumps of lsass.exe",
            "Off by default. lsass holds the sign-in secrets of this PC: the password hashes and \
             Kerberos tickets of everyone signed in. Reading its memory is exactly what \
             credential-stealing tools do, so Microsoft Defender and other antivirus programs \
             treat any program that tries as an attack: they stop the dump and may quarantine \
             Anabolic. Turn it on only to debug lsass on a machine you look after, and keep the \
             file safe. MCP clients can never dump it; lsaiso.exe and the processes Windows \
             protects cannot be dumped at all.",
            "lsass now shows \"Create dump file\" in its menu.",
        );
        #[cfg(target_os = "linux")]
        let (what, why, now) = (
            "Allow dumps of the keyrings",
            "Off by default. KWallet, GNOME Keyring, ssh-agent and gpg-agent hold the session's \
             passwords and keys, unlocked: a dump of one holds them in the clear. Turn it on only \
             to debug one of them, and keep the file safe. MCP clients can never dump them.",
            "The keyrings now show \"Create dump file\" in their menu.",
        );
        row(ui, what, why, |ui| {
            ui.checkbox(&mut self.settings.dump_sign_in_secrets, "");
            if self.settings.dump_sign_in_secrets {
                caution(ui, now);
            }
        });
    }

    fn assistant_section(&mut self, ui: &mut Ui) {
        heading(ui, "AI assistants");
        let status = match &self.mcp.status {
            mcp::McpStatus::Listening(addr) => {
                format!("The MCP server listens at http://{addr}/mcp.")
            }
            mcp::McpStatus::Failed(e) => format!("The MCP server could not start: {e}"),
            mcp::McpStatus::Off => "The MCP server is off.".to_string(),
        };
        row(
            ui,
            "MCP server",
            &format!(
                "{status} It lets Claude and other Model Context Protocol clients read what \
                 Anabolic sees and do what you allow, kind by kind."
            ),
            |ui| {
                if ui.button("Open the MCP page").clicked() {
                    self.settings.tab = Tab::Mcp;
                }
            },
        );
    }

    fn about_section(&mut self, ui: &mut Ui) {
        heading(ui, "About");
        row(
            ui,
            &format!("Anabolic {}", env!("CARGO_PKG_VERSION")),
            "Performance monitoring and diagnostics for Windows and Linux that stays responsive \
             when the machine does not. MIT or Apache-2.0, at your option.",
            |ui| {
                if ui
                    .add(egui::Button::new("Shortcuts and colours").shortcut_text("F1"))
                    .clicked()
                {
                    self.help_open = true;
                }
            },
        );
        let (rights, about) = if self.elevated {
            (
                crate::platform::RUNNING_ELEVATED,
                "Every process, disk counters and owners are visible, and kernel traces can run.",
            )
        } else if cfg!(windows) {
            (
                crate::platform::RUNNING_PLAIN,
                "Protected processes, disk counters, some owners and the kernel traces are \
                 missing.",
            )
        } else {
            (
                crate::platform::RUNNING_PLAIN,
                "Other users' processes keep their open files, threads and disk traffic to \
                 themselves, and the kernel traces cannot run. Granting full access gives this \
                 executable the capabilities it needs (pkexec asks for your password once).",
            )
        };
        row(ui, rights, about, |ui| {
            if self.elevated {
                ui.label(widgets::icon(theme::icon::SHIELD, 14.0).color(theme::GOOD));
            } else if ui
                .add_enabled(
                    !self.relaunching,
                    egui::Button::new(
                        RichText::new(crate::platform::ELEVATE).color(theme::ELEVATED_TEXT),
                    ),
                )
                .on_disabled_hover_text(crate::platform::ELEVATING)
                .clicked()
            {
                self.relaunching = true;
                self.actions.run(Action::RelaunchElevated);
            }
        });
    }
}
