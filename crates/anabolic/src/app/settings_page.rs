// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

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

/// What the sign-in setting says it does.
#[cfg(windows)]
const SIGN_IN_HINT: &str = "Anabolic starts in the notification area as you sign in, so the \
                            history and your rules run from the start. On an administrator's \
                            account a task in Task Scheduler (folder Anabolic) starts it \
                            elevated, without asking.";
#[cfg(target_os = "linux")]
const SIGN_IN_HINT: &str = "Anabolic starts in the notification area as you sign in, so the \
                            history and your rules run from the start: an entry in \
                            ~/.config/autostart, which every desktop reads.";

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
                self.warning_section(ui);
                self.ports_section(ui);
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
        row(
            ui,
            "Show CPU use on the icon",
            "The notification area icon fills a bar up its right edge as the CPU gets busy, amber \
             from 90 %. While the icon is there: when Anabolic keeps running in the notification \
             area, or waits to tell you a process exited.",
            |ui| {
                ui.checkbox(&mut self.settings.tray_cpu, "");
            },
        );
        row(ui, "Start when I sign in", SIGN_IN_HINT, |ui| {
            use anabolic_core::signin::SignIn;
            match self.sign_in.clone() {
                // Not known yet, or a change on its way.
                None => {
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, ""));
                }
                Some(SignIn::Unavailable(why)) => {
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, ""));
                    caution(ui, why);
                }
                Some(state) => {
                    let mut on = state == SignIn::On;
                    if ui.checkbox(&mut on, "").changed() {
                        self.sign_in = None;
                        self.actions.run(Action::SignIn(Some(on)));
                    }
                }
            }
        });
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
    }

    fn warning_section(&mut self, ui: &mut Ui) {
        heading(ui, "Warnings");
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
        row(
            ui,
            "Warn about a program in many copies",
            "When one program runs this many times over from the same starter (a script or a \
             build gone wrong), a bar says who started them and can end them all; with the window \
             closed, a notification. Browsers and the system's service hosts run many processes \
             by design and are left out.",
            |ui| {
                ui.horizontal(|ui| {
                    let mut on = self.settings.runaway_at != 0;
                    if ui.checkbox(&mut on, "").changed() {
                        self.settings.runaway_at = if on { crate::runaway::DEFAULT_AT } else { 0 };
                    }
                    if on {
                        ui.add(
                            egui::DragValue::new(&mut self.settings.runaway_at)
                                .range(10..=1000)
                                .suffix(" copies"),
                        );
                    }
                });
            },
        );
        if !self.settings.runaway_ignored.is_empty() {
            let mut forget = None;
            row(
                ui,
                "Never warned about",
                "Programs you said not to warn about.",
                |ui| {
                    for (i, name) in self.settings.runaway_ignored.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(name.as_str());
                            if ui
                                .small_button(widgets::icon(theme::icon::CLOSE, 9.0))
                                .on_hover_text("Warn about it again")
                                .clicked()
                            {
                                forget = Some(i);
                            }
                        });
                    }
                },
            );
            if let Some(i) = forget {
                self.settings.runaway_ignored.remove(i);
            }
        }
    }

    fn ports_section(&mut self, ui: &mut Ui) {
        heading(ui, "Development ports");
        let mut forget = None;
        row(
            ui,
            "Ports you watch",
            "Each shows as free, or with the program that holds it, on Network > Development \
             ports and on the Overview, where Free… ends what holds it. A name says what the port \
             is for, in place of the tool Anabolic would guess.",
            |ui| {
                for (i, w) in self.settings.watched_ports.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        widgets::text(
                            ui,
                            52.0,
                            Align::Max,
                            widgets::strong(format!(":{}", w.port), theme::BODY),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut w.name)
                                .hint_text("name")
                                .char_limit(crate::settings::PORT_NAME)
                                .desired_width(120.0),
                        );
                        if ui
                            .small_button(widgets::icon(theme::icon::CLOSE, 9.0))
                            .on_hover_text("Stop watching it")
                            .clicked()
                        {
                            forget = Some(i);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    let (port, name) = &mut self.port_entry;
                    ui.add(
                        egui::TextEdit::singleline(port)
                            .hint_text("port")
                            .desired_width(52.0),
                    );
                    port.retain(|c| c.is_ascii_digit());
                    port.truncate(5);
                    ui.add(
                        egui::TextEdit::singleline(name)
                            .hint_text("name (optional)")
                            .char_limit(crate::settings::PORT_NAME)
                            .desired_width(120.0),
                    );
                    let parsed = port.parse::<u16>().ok().filter(|&p| p != 0);
                    if ui
                        .add_enabled(parsed.is_some(), egui::Button::new("Add"))
                        .clicked()
                        && let Some(port) = parsed
                    {
                        let ports = &mut self.settings.watched_ports;
                        ports.retain(|w| w.port != port);
                        ports.push(crate::settings::PortWatch {
                            port,
                            name: self.port_entry.1.trim().to_string(),
                        });
                        ports.sort_by_key(|w| w.port);
                        self.port_entry = Default::default();
                    }
                });
            },
        );
        if let Some(i) = forget {
            self.settings.watched_ports.remove(i);
        }
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
             when the machine does not. Copyright (c) 2026 Coşkun Bağrıaçık, open source under \
             MIT or Apache-2.0, at your option: reuse is welcome, with this notice kept and \
             Anabolic named.",
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
