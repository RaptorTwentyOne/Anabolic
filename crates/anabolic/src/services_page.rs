// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Services page: every service (Windows' services, or systemd's units
//! on Linux, the user's own included), its state, how it starts, and the
//! process it runs in with what that process costs. Start, stop, restart
//! and the startup type from the row's menu or the side pane.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anabolic_core::services::{Service, ServiceList, ServiceOp, StartType, State};
use anabolic_core::{ProcState, ProcessInfo, Snapshot};
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use crate::proc_table::{UiCmd, item};
use crate::table::{TableView, text_cmp};
use crate::{fmt, panes, theme, widgets};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Show {
    #[default]
    All,
    Running,
    Stopped,
}

pub struct ServicesPage {
    pub list: Option<Arc<ServiceList>>,
    pub view: TableView,
    pub show: Show,
    pub selected: Option<Arc<str>>,
    /// Controls Anabolic sent that have not answered yet.
    pub busy: HashSet<Arc<str>>,
}

impl Default for ServicesPage {
    fn default() -> Self {
        Self {
            list: None,
            view: TableView::sorted(0, false),
            show: Show::All,
            selected: None,
            busy: HashSet::new(),
        }
    }
}

impl ServicesPage {
    /// The services running in each process, by name and display name.
    pub fn hosted(&self, pid: u32) -> Vec<(Arc<str>, Arc<str>)> {
        let Some(list) = &self.list else {
            return Vec::new();
        };
        list.services
            .iter()
            .filter(|s| s.pid == pid && pid != 0)
            .map(|s| (s.name.clone(), s.display.clone()))
            .collect()
    }

    /// Opens the page on the services of one process.
    pub fn show_pid(&mut self, pid: u32) {
        self.view.set_filter(&pid.to_string());
        self.show = Show::All;
        self.selected = None;
    }
}

/// The executable a service's command line starts.
#[cfg(windows)]
pub fn executable(binary: &str) -> Option<String> {
    let binary = binary.trim();
    if let Some(rest) = binary.strip_prefix('"') {
        return rest.split('"').next().map(str::to_string);
    }
    let lower = binary.to_lowercase();
    let end = lower.find(".exe").map(|i| i + 4).unwrap_or(binary.len());
    let path = binary[..end].trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// The executable a service's command line starts: its first word (systemd
/// runs commands without a shell, so a path has no spaces unquoted).
#[cfg(target_os = "linux")]
pub fn executable(binary: &str) -> Option<String> {
    let binary = binary.trim();
    let path = match binary.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => binary.split_ascii_whitespace().next()?,
    };
    path.starts_with('/').then(|| path.to_string())
}

fn status_text(s: &Service) -> (String, Color32, Option<String>) {
    match s.state {
        // 1077: never started since boot, not a failure.
        State::Stopped if s.exit_code != 0 && s.exit_code != 1077 => (
            "Stopped (failed)".into(),
            theme::BAD,
            Some(failure(s.exit_code)),
        ),
        State::Stopped => ("Stopped".into(), theme::TEXT_DIM, None),
        State::Running => ("Running".into(), theme::TEXT, None),
        State::Paused => ("Paused".into(), theme::SUSPENDED_TEXT, None),
        pending => (format!("{}…", pending.label()), theme::ELEVATED_TEXT, None),
    }
}

#[cfg(windows)]
fn failure(code: u32) -> String {
    format!(
        "It last stopped with error {code}: {}",
        anabolic_core::services::exit_text(code)
    )
}

#[cfg(target_os = "linux")]
fn failure(code: u32) -> String {
    format!("It failed: {}.", anabolic_core::services::exit_text(code))
}

/// Started by something else as well: an event on Windows (a trigger),
/// a socket, timer, path or D-Bus name on Linux (on demand).
const STARTED_BY_OTHERS: &str = if cfg!(windows) {
    "trigger"
} else {
    "on demand"
};

const TRIGGERED: &str = if cfg!(windows) {
    "Windows also starts it on an event (a device, the network, a policy)."
} else {
    "Also started when something needs it: a socket, a timer, a device or a D-Bus name."
};

/// The account a service runs under.
const ACCOUNT: &str = if cfg!(windows) {
    "Log on as"
} else {
    "Runs as"
};

/// Who may start and stop services.
const RIGHTS: &str = if cfg!(windows) {
    "Starting and stopping most services needs administrator"
} else {
    "System services ask for an administrator's password; your own (user) do not"
};

fn startup_text(s: &Service) -> String {
    match &s.config {
        Some(c) if c.triggered => format!("{} ({STARTED_BY_OTHERS})", c.start.label()),
        Some(c) => c.start.label().to_string(),
        None => String::new(),
    }
}

fn account(s: &Service) -> &str {
    s.config.as_ref().map_or("", |c| fmt::account(&c.account))
}

/// The process a service runs in, and how many services share it.
fn host<'a>(
    s: &Service,
    processes: &HashMap<u32, &'a ProcessInfo>,
    sharing: &HashMap<u32, usize>,
) -> Option<(&'a ProcessInfo, usize)> {
    let p = processes.get(&s.pid)?;
    Some((p, sharing.get(&s.pid).copied().unwrap_or(1)))
}

pub fn service_menu(ui: &mut Ui, s: &Service, busy: bool, out: &mut Vec<UiCmd>) {
    let control = |op| UiCmd::Service {
        name: s.name.clone(),
        display: s.display.clone(),
        op,
    };
    let stopped = s.state == State::Stopped;
    let running = s.state == State::Running;
    let can_start = stopped && !s.user_template && !busy;
    let can_stop = !stopped && s.accepts_stop && !busy;
    ui.add_enabled_ui(can_start, |ui| {
        item(ui, out, "Start", "", control(ServiceOp::Start));
    });
    ui.add_enabled_ui(can_stop, |ui| {
        item(
            ui,
            out,
            "Stop",
            "",
            control(ServiceOp::Stop { dependents: false }),
        );
        item(
            ui,
            out,
            "Restart",
            "",
            control(ServiceOp::Restart { dependents: false }),
        );
    });
    if s.accepts_pause {
        ui.add_enabled_ui(!busy, |ui| match s.state {
            State::Paused => item(ui, out, "Resume", "", control(ServiceOp::Continue)),
            _ => {
                ui.add_enabled_ui(running, |ui| {
                    item(ui, out, "Pause", "", control(ServiceOp::Pause));
                });
            }
        });
    }
    ui.separator();
    ui.add_enabled_ui(s.config.is_some() && !busy, |ui| {
        ui.menu_button("Startup type", |ui| {
            let now = s.start_type();
            for choice in StartType::CHOICES {
                if ui
                    .add(egui::Button::selectable(
                        now == Some(choice),
                        choice.label(),
                    ))
                    .clicked()
                {
                    out.push(control(ServiceOp::SetStart(choice)));
                    ui.close();
                }
            }
        });
    });
    ui.separator();
    if s.pid != 0 {
        item(ui, out, "Go to process", "", UiCmd::GoToPid(s.pid));
    }
    if let Some(exe) = s.config.as_ref().and_then(|c| executable(&c.binary)) {
        item(ui, out, "Open file location", "", UiCmd::OpenLocation(exe));
    }
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(format!("{} service {}", s.name, s.display)),
    );
    item(ui, out, "Copy name", "", UiCmd::Copy(s.name.to_string()));
}

fn pane(
    ui: &mut Ui,
    s: &Service,
    list: &ServiceList,
    processes: &HashMap<u32, &ProcessInfo>,
    busy: bool,
    out: &mut Vec<UiCmd>,
) {
    panes::title(ui, s.display.to_string(), Some(&s.name));
    if let Some(c) = &s.config
        && !c.description.is_empty()
    {
        panes::weak(ui, &c.description);
        ui.add_space(6.0);
    }
    let control = |op| UiCmd::Service {
        name: s.name.clone(),
        display: s.display.clone(),
        op,
    };
    ui.horizontal_wrapped(|ui| {
        let stopped = s.state == State::Stopped;
        if stopped {
            ui.add_enabled_ui(!s.user_template && !busy, |ui| {
                if ui.button("Start").clicked() {
                    out.push(control(ServiceOp::Start));
                }
            });
        } else {
            ui.add_enabled_ui(s.accepts_stop && !busy, |ui| {
                if ui.button("Stop").clicked() {
                    out.push(control(ServiceOp::Stop { dependents: false }));
                }
                if ui.button("Restart").clicked() {
                    out.push(control(ServiceOp::Restart { dependents: false }));
                }
            });
        }
        if s.accepts_pause {
            ui.add_enabled_ui(!busy, |ui| {
                if s.state == State::Paused {
                    if ui.button("Resume").clicked() {
                        out.push(control(ServiceOp::Continue));
                    }
                } else if ui.button("Pause").clicked() {
                    out.push(control(ServiceOp::Pause));
                }
            });
        }
        if busy {
            ui.spinner();
        }
    });
    if s.user_template {
        panes::weak(
            ui,
            "A per-user template: Windows runs a copy of it for each signed-in user, listed \
             with the same name and a suffix. The template itself does not start.",
        );
    } else if !s.accepts_stop && s.state == State::Running {
        panes::weak(ui, "It does not accept a stop request while it runs.");
    }

    panes::section(ui, "Service");
    panes::facts(ui, ("service-facts", &*s.name), |ui| {
        let (status, color, why) = status_text(s);
        panes::fact(ui, "Status", RichText::new(status).color(color));
        if let Some(why) = why {
            panes::fact(ui, "", RichText::new(why).color(theme::TEXT_WEAK));
        }
        ui.label(RichText::new("Startup").color(theme::TEXT_WEAK));
        match s.start_type() {
            Some(now) if !matches!(now, StartType::Boot | StartType::System) => {
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt(("startup", &*s.name))
                        .selected_text(now.label())
                        .show_ui(ui, |ui| {
                            for choice in StartType::CHOICES {
                                if ui.selectable_label(now == choice, choice.label()).clicked()
                                    && now != choice
                                {
                                    out.push(control(ServiceOp::SetStart(choice)));
                                }
                            }
                        });
                });
            }
            Some(now) => {
                ui.label(now.label());
            }
            None => {
                ui.label(RichText::new("—").color(theme::TEXT_DIM));
            }
        }
        ui.end_row();
        if let Some(c) = &s.config
            && c.triggered
        {
            panes::fact(ui, "", RichText::new(TRIGGERED).color(theme::TEXT_WEAK));
        }
        if let Some(c) = &s.config {
            panes::fact(ui, ACCOUNT, c.account.clone());
            panes::fact(ui, "Runs", RichText::new(&c.binary).small());
            if !c.dependencies.is_empty() {
                panes::fact(ui, "Needs", c.dependencies.join(", "));
            }
        }
    });

    if s.pid != 0 {
        panes::section(ui, "Process");
        let others: Vec<&Service> = list
            .services
            .iter()
            .filter(|o| o.pid == s.pid && o.name != s.name)
            .collect();
        panes::facts(ui, ("service-process", &*s.name), |ui| {
            ui.label(RichText::new("Runs in").color(theme::TEXT_WEAK));
            match processes.get(&s.pid) {
                Some(p) => {
                    if widgets::link(ui, &format!("{} ({})", p.name, p.pid)).clicked() {
                        out.push(UiCmd::GoToPid(s.pid));
                    }
                }
                None => {
                    ui.label(format!("PID {}", s.pid));
                }
            }
            ui.end_row();
            if let Some(p) = processes.get(&s.pid) {
                let cpu = fmt::cpu(p.cpu);
                panes::fact(
                    ui,
                    "CPU",
                    if cpu.is_empty() {
                        "0 %".to_string()
                    } else {
                        format!("{cpu} %")
                    },
                );
                panes::fact(ui, "Memory", fmt::bytes(p.private_bytes));
            }
        });
        if !others.is_empty() {
            panes::weak(
                ui,
                format!(
                    "The process runs {} other service{} too; its CPU and memory are theirs \
                     together:",
                    others.len(),
                    if others.len() == 1 { "" } else { "s" }
                ),
            );
            for o in others {
                if widgets::link(ui, &format!("{}  ({})", o.display, o.name)).clicked() {
                    out.push(UiCmd::SelectService(o.name.clone()));
                }
            }
        }
    }
}

pub fn show(
    ui: &mut Ui,
    page: &mut ServicesPage,
    snap: &Snapshot,
    elevated: bool,
    out: &mut Vec<UiCmd>,
) {
    let Some(list) = page.list.clone() else {
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Reading the services…").color(theme::TEXT_WEAK));
        });
        return;
    };
    if let Some(e) = &list.error {
        ui.label(
            RichText::new(format!("Could not list the services: {e}")).color(theme::ELEVATED_TEXT),
        );
        return;
    }
    let processes: HashMap<u32, &ProcessInfo> = snap
        .processes
        .iter()
        // A stopped service has PID 0, which is the idle process'.
        .filter(|p| p.state != ProcState::Exited && p.pid != 0)
        .map(|p| (p.pid, p))
        .collect();
    let mut sharing: HashMap<u32, usize> = HashMap::new();
    for s in &list.services {
        if s.pid != 0 {
            *sharing.entry(s.pid).or_default() += 1;
        }
    }
    let running = list
        .services
        .iter()
        .filter(|s| s.state != State::Stopped)
        .count();

    let view = &mut page.view;
    let show = page.show;
    let mut rows: Vec<&Service> = list
        .services
        .iter()
        .filter(|s| match show {
            Show::All => true,
            Show::Running => s.state != State::Stopped,
            Show::Stopped => s.state == State::Stopped,
        })
        .filter(|s| {
            let pid = if s.pid == 0 {
                String::new()
            } else {
                s.pid.to_string()
            };
            let c = s.config.as_deref();
            view.matches(&[
                &s.display,
                &s.name,
                &pid,
                c.map_or("", |c| &c.description),
                c.map_or("", |c| &c.account),
                c.map_or("", |c| &c.binary),
            ])
        })
        .collect();
    let cpu = |s: &Service| processes.get(&s.pid).map_or(-1.0, |p| p.cpu);
    let memory = |s: &Service| processes.get(&s.pid).map_or(0, |p| p.private_bytes);
    view.order(&mut rows, |a, b, column| match column {
        0 => text_cmp(&a.display, &b.display),
        1 => text_cmp(&a.name, &b.name),
        2 => (a.state as u8).cmp(&(b.state as u8)),
        3 => startup_text(a).cmp(&startup_text(b)),
        4 => a.pid.cmp(&b.pid),
        5 => cpu(a).partial_cmp(&cpu(b)).unwrap_or(Ordering::Equal),
        6 => memory(a).cmp(&memory(b)),
        7 => text_cmp(account(a), account(b)),
        _ => text_cmp(
            a.config.as_ref().map_or("", |c| &c.description),
            b.config.as_ref().map_or("", |c| &c.description),
        ),
    });

    ui.horizontal(|ui| {
        if view.filter_box(
            ui,
            "Filter: name, description, PID, account",
            rows.len(),
            list.services.len(),
        ) {
            ui.ctx().request_repaint();
        }
        ui.add_space(6.0);
        for (choice, label) in [
            (Show::All, "All"),
            (Show::Running, "Running"),
            (Show::Stopped, "Stopped"),
        ] {
            if ui
                .add(egui::Button::selectable(page.show == choice, label))
                .clicked()
            {
                page.show = choice;
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{} services  ·  {running} running  ·  read in {}",
                list.services.len(),
                fmt::millis(list.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        if !elevated {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(RIGHTS).small().color(theme::TEXT_DIM));
                ui.label(widgets::icon(theme::icon::LOCK, 11.0).color(theme::TEXT_DIM));
            });
        }
    });
    ui.add_space(2.0);

    let selected = page.selected.clone();
    if let Some(s) = selected
        .as_deref()
        .and_then(|name| list.services.iter().find(|s| &*s.name == name))
    {
        let busy = page.busy.contains(&s.name);
        panes::side(ui, ("service-pane", &*s.name), |ui| {
            pane(ui, s, &list, &processes, busy, out)
        });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        TableBuilder::new(ui)
            .id_salt("services")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(250.0).clip(true))
            .column(Column::initial(150.0).clip(true))
            .column(Column::initial(110.0))
            .column(Column::initial(150.0).clip(true))
            .column(Column::initial(60.0))
            .column(Column::initial(60.0))
            .column(Column::initial(80.0))
            .column(Column::initial(120.0).clip(true))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut h| {
                for (i, (t, numeric)) in [
                    ("Name", false),
                    ("Service", false),
                    ("Status", false),
                    ("Startup", false),
                    ("PID", true),
                    ("CPU %", true),
                    ("Memory", true),
                    (ACCOUNT, false),
                    ("Description", false),
                ]
                .into_iter()
                .enumerate()
                {
                    h.col(|ui| {
                        view.header(ui, i, t, numeric);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let s = rows[row.index()];
                    let busy = page.busy.contains(&s.name);
                    row.set_selected(selected.as_deref() == Some(&*s.name));
                    let host = host(s, &processes, &sharing);
                    // A shared host's figures are the whole process'.
                    let figure = if host.is_some_and(|(_, n)| n > 1) {
                        theme::TEXT_DIM
                    } else {
                        theme::TEXT
                    };
                    row.col(|ui| {
                        ui.add(Label::new(&*s.display).truncate());
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(&*s.name).color(theme::TEXT_WEAK)).truncate(),
                        );
                    });
                    row.col(|ui| {
                        let (text, color, why) = status_text(s);
                        let response = ui.label(RichText::new(text).color(color));
                        if let Some(why) = why {
                            response.on_hover_text(why);
                        }
                        if busy {
                            ui.spinner();
                        }
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(startup_text(s)).color(theme::TEXT_WEAK))
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        if s.pid != 0 {
                            right(ui, RichText::new(s.pid.to_string()).color(theme::TEXT_WEAK));
                        }
                    });
                    row.col(|ui| {
                        if let Some((p, n)) = host {
                            let response = right(ui, RichText::new(fmt::cpu(p.cpu)).color(figure));
                            if n > 1 {
                                response.on_hover_text(format!(
                                    "{} ({}) runs {n} services; this is the whole process.",
                                    p.name, p.pid
                                ));
                            }
                        }
                    });
                    row.col(|ui| {
                        if let Some((p, _)) = host {
                            right(ui, RichText::new(fmt::bytes(p.private_bytes)).color(figure));
                        }
                    });
                    row.col(|ui| {
                        ui.add(
                            Label::new(RichText::new(account(s)).color(theme::TEXT_WEAK))
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        let text = s.config.as_ref().map_or("", |c| c.description.as_str());
                        ui.add(Label::new(RichText::new(text).color(theme::TEXT_DIM)).truncate());
                    });
                    let response = row.response();
                    if response.clicked() {
                        page.selected = if selected.as_deref() == Some(&*s.name) {
                            None
                        } else {
                            Some(s.name.clone())
                        };
                    }
                    response.context_menu(|ui| service_menu(ui, s, busy, out));
                });
            });
    });
}

fn right(ui: &mut Ui, text: RichText) -> egui::Response {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false))
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::executable;

    #[test]
    #[cfg(windows)]
    fn finds_the_program_of_a_command_line() {
        assert_eq!(
            executable(r"C:\WINDOWS\system32\svchost.exe -k netsvcs -p").as_deref(),
            Some(r"C:\WINDOWS\system32\svchost.exe")
        );
        assert_eq!(
            executable(r#""C:\Program Files\App\app svc.exe" --run"#).as_deref(),
            Some(r"C:\Program Files\App\app svc.exe")
        );
        assert_eq!(executable("   "), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn finds_the_program_of_a_command_line() {
        assert_eq!(
            executable("/usr/bin/sshd -D").as_deref(),
            Some("/usr/bin/sshd")
        );
        assert_eq!(
            executable(r#""/opt/my app/svc" --run"#).as_deref(),
            Some("/opt/my app/svc")
        );
        assert_eq!(executable("   "), None);
    }
}
