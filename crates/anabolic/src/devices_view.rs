// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Performance page's Devices: every device the system knows, as Device
//! Manager does, with the ones that have a problem first and the problem in
//! words, and the driver each runs on. Read in the background now and then,
//! for the Overview, and again while the view is open.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::devices::{Device, DeviceState, Devices, problem_words, troubled};
use anabolic_core::sysinfo::local_time;
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{panes, theme, widgets};

/// Read again this often in the background, and this often while shown.
const EVERY: Duration = Duration::from_secs(10 * 60);
const WHILE_SHOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Which {
    #[default]
    Connected,
    NotConnected,
    All,
    /// Every USB device ever plugged in, as USBDeview shows them.
    Usb,
}

#[derive(Default)]
pub struct DevicesView {
    read: Option<Result<Arc<Devices>, String>>,
    reading: Option<Receiver<Result<Devices, String>>>,
    read_at: Option<Instant>,
    pub which: Which,
    /// Devices Windows hides in Device Manager too.
    pub hidden: bool,
    pub filter: String,
    /// The device in the pane, by its id.
    selected: Option<String>,
    /// How the devices interrupt, instead of the list (Windows).
    pub interrupts: bool,
    #[cfg(windows)]
    irq: crate::interrupts_view::InterruptsView,
}

impl DevicesView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("devices".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::devices::read());
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.reading = Some(rx);
                self.read_at = Some(Instant::now());
            }
            Err(e) => self.read = Some(Err(format!("could not start reading: {e}"))),
        }
    }

    /// Reads the devices when due; call every frame, with whether the view
    /// is on screen.
    pub fn tick(&mut self, ctx: &egui::Context, shown: bool) {
        if let Some(rx) = &self.reading
            && let Ok(result) = rx.try_recv()
        {
            self.read = Some(result.map(Arc::new));
            self.reading = None;
        }
        let every = if shown { WHILE_SHOWN } else { EVERY };
        let due = self.read_at.is_none_or(|at| at.elapsed() >= every);
        if due && self.reading.is_none() {
            self.start(ctx);
        }
    }

    pub fn devices(&self) -> Option<&Arc<Devices>> {
        self.read.as_ref()?.as_ref().ok()
    }

    /// Shows one device in the pane.
    pub fn select(&mut self, id: &str) {
        self.which = Which::All;
        self.hidden = true;
        self.selected = Some(id.to_string());
    }
}

/// The Performance page's line for the view.
pub fn summary(devices: Option<&Devices>) -> String {
    let Some(d) = devices else {
        return "Reading…".into();
    };
    let connected = d
        .devices
        .iter()
        .filter(|x| x.state != DeviceState::NotConnected && !x.hidden)
        .count();
    match troubled(&d.devices).len() {
        0 => format!("{connected} connected, all working"),
        n => format!("{connected} connected  ·  {n} to look at"),
    }
}

fn when(at: i64) -> String {
    local_time(at)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

fn state_color(state: DeviceState) -> egui::Color32 {
    match state {
        DeviceState::Problem(_) => theme::BAD,
        DeviceState::NoDriver => theme::ELEVATED_TEXT,
        DeviceState::Disabled | DeviceState::NotConnected => theme::TEXT_WEAK,
        DeviceState::Working => theme::TEXT,
    }
}

fn driver_line(d: &Device) -> String {
    let Some(dr) = &d.driver else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(p) = &dr.provider {
        parts.push(p.clone());
    } else if let Some(s) = &dr.service {
        parts.push(s.clone());
    }
    if let Some(v) = &dr.version {
        parts.push(v.clone());
    }
    parts.join(" ")
}

fn matches(d: &Device, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    let has = |s: Option<&str>| s.is_some_and(|s| s.to_lowercase().contains(&f));
    has(Some(&d.name))
        || has(Some(&d.class))
        || has(d.manufacturer.as_deref())
        || has(Some(&d.id))
        || has(Some(&driver_line(d)))
        || has(d.driver.as_ref().and_then(|x| x.service.as_deref()))
}

pub fn show(ui: &mut Ui, view: &mut DevicesView, out: &mut Vec<UiCmd>) {
    let devices = match &view.read {
        None => {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the devices…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(
                RichText::new(format!("The devices could not be read: {e}")).color(theme::BAD),
            );
            return;
        }
        Some(Ok(d)) => d.clone(),
    };
    ui.horizontal(|ui| {
        ui.label(widgets::strong("Devices", theme::TITLE));
        #[cfg(windows)]
        {
            ui.add_space(16.0);
            if ui
                .selectable_label(!view.interrupts, "Every device")
                .clicked()
            {
                view.interrupts = false;
            }
            if ui.selectable_label(view.interrupts, "Interrupts").clicked() {
                view.interrupts = true;
            }
        }
    });
    #[cfg(windows)]
    if view.interrupts {
        crate::interrupts_view::show(ui, &mut view.irq, out);
        return;
    }
    let gone = devices
        .devices
        .iter()
        .filter(|d| d.state == DeviceState::NotConnected)
        .count();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{}  ·  {gone} known but not connected  ·  read in {}",
                summary(Some(&devices)),
                crate::fmt::millis(devices.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let reading = view.reading.is_some();
            if ui
                .add_enabled(!reading, egui::Button::new("Read again"))
                .clicked()
            {
                view.start(ui.ctx());
            }
            if reading {
                ui.spinner();
            }
        });
    });
    for note in &devices.notes {
        ui.label(RichText::new(note).small().color(theme::ELEVATED_TEXT));
    }

    // The ones with a problem, in words, above everything.
    let bad = troubled(&devices.devices);
    if !bad.is_empty() {
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(theme::RAISED)
            .corner_radius(4.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for d in bad.iter().take(6) {
                    ui.horizontal(|ui| {
                        ui.label(
                            widgets::icon(theme::icon::WARNING, 13.0).color(state_color(d.state)),
                        );
                        let name = ui.add(
                            Label::new(widgets::strong(&d.name, theme::BODY)).sense(Sense::click()),
                        );
                        if name.clicked() {
                            view.selected = Some(d.id.clone());
                        }
                        let what = match d.state {
                            DeviceState::Problem(code) => problem_words(code)
                                .map_or_else(|| d.state.label(), |(w, _)| w.to_string()),
                            _ => "No driver is installed for it.".to_string(),
                        };
                        ui.label(RichText::new(what).color(theme::TEXT_WEAK));
                    });
                }
                if bad.len() > 6 {
                    ui.label(
                        RichText::new(format!("and {} more below", bad.len() - 6))
                            .small()
                            .color(theme::TEXT_WEAK),
                    );
                }
            });
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        for (which, label) in [
            (Which::Connected, "Connected"),
            (Which::NotConnected, "Not connected"),
            (Which::All, "All"),
            (Which::Usb, "USB history"),
        ] {
            if ui.selectable_label(view.which == which, label).clicked() {
                view.which = which;
            }
        }
        ui.add_space(8.0);
        ui.checkbox(&mut view.hidden, "System devices too")
            .on_hover_text("The ones Device Manager hides unless asked: the system's plumbing");
        ui.add_space(8.0);
        widgets::clearable(ui, &mut view.filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter: name, kind, maker, driver")
                .desired_width(240.0)
        });
    });
    ui.add_space(4.0);

    let shown: Vec<&Device> = if view.which == Which::Usb {
        anabolic_core::devices::usb_history(&devices.devices)
            .into_iter()
            .filter(|d| matches(d, view.filter.trim()))
            .collect()
    } else {
        devices
            .devices
            .iter()
            .filter(|d| match view.which {
                Which::Connected => d.state != DeviceState::NotConnected,
                Which::NotConnected => d.state == DeviceState::NotConnected,
                _ => true,
            })
            .filter(|d| view.hidden || !d.hidden || d.state.bad())
            .filter(|d| matches(d, view.filter.trim()))
            .collect()
    };
    if view.which == Which::Usb {
        ui.label(
            RichText::new(if cfg!(windows) {
                "Every USB device this PC has known, the last connected first, as USBDeview \
                 shows them: Windows keeps a device's record after it is unplugged."
            } else {
                "The USB devices connected now: Linux keeps no record of the ones unplugged."
            })
            .small()
            .color(theme::TEXT_WEAK),
        );
    }

    if let Some(d) = view
        .selected
        .as_deref()
        .and_then(|id| devices.devices.iter().find(|d| d.id == id))
    {
        let mut close = false;
        panes::side(ui, ("device-pane", &d.id), |ui| close = pane(ui, d, out));
        if close {
            view.selected = None;
        }
    }
    let usb = view.which == Which::Usb;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        if usb {
            usb_table(ui, &shown, &mut view.selected)
        } else {
            table(ui, &shown, &mut view.selected)
        }
    });
}

const USB_TITLES: [&str; 6] = [
    "Device",
    "Kind",
    "Now",
    "First connected",
    "Last connected",
    "Last removed",
];
const USB_COLUMNS: [Spec; 6] = [
    Spec::new(300.0).clip(),
    Spec::new(130.0).clip(),
    Spec::new(110.0).clip(),
    Spec::new(130.0),
    Spec::new(130.0),
    Spec::new(130.0),
];

fn usb_table(ui: &mut Ui, shown: &[&Device], selected: &mut Option<String>) {
    let mut pick = None;
    Columns::load(ui.ctx(), "usb-history", &USB_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    widgets::column_header(ui, USB_TITLES[c], false)
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, shown.len(), |mut row| {
                    let d = shown[row.index()];
                    row.set_selected(selected.as_deref() == Some(d.id.as_str()));
                    let connected = d.state != DeviceState::NotConnected;
                    for &c in columns.order() {
                        row.col(|ui| {
                            let at = |t: Option<i64>| t.map(when).unwrap_or_default();
                            let text = match c {
                                0 => RichText::new(d.full_name()).color(if connected {
                                    theme::TEXT
                                } else {
                                    theme::TEXT_WEAK
                                }),
                                1 => RichText::new(&d.class).color(theme::TEXT_WEAK),
                                2 => RichText::new(if connected {
                                    "Connected"
                                } else {
                                    "Not connected"
                                })
                                .color(if connected {
                                    theme::GOOD
                                } else {
                                    theme::TEXT_DIM
                                }),
                                3 => RichText::new(at(d.installed)).color(theme::TEXT_WEAK),
                                4 => RichText::new(at(d.arrived)).color(theme::TEXT_WEAK),
                                _ => RichText::new(at(d.removed)).color(theme::TEXT_WEAK),
                            };
                            ui.add(Label::new(text).truncate());
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(d.id.clone());
                    }
                });
            })
    });
    if let Some(id) = pick {
        *selected = if selected.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
    }
}

const TITLES: [(&str, bool); 6] = [
    ("Device", false),
    ("Kind", false),
    ("State", false),
    ("Driver", false),
    ("Driver date", false),
    ("Maker", false),
];
const COLUMNS: [Spec; 6] = [
    Spec::new(300.0).clip(),
    Spec::new(120.0).clip(),
    Spec::new(120.0).clip(),
    Spec::new(220.0).clip(),
    Spec::new(90.0),
    Spec::new(180.0).clip(),
];

fn table(ui: &mut Ui, shown: &[&Device], selected: &mut Option<String>) {
    let mut pick = None;
    Columns::load(ui.ctx(), "devices", &COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, shown.len(), |mut row| {
                    let d = shown[row.index()];
                    row.set_selected(selected.as_deref() == Some(d.id.as_str()));
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                let text = RichText::new(&d.name).color(match d.state {
                                    DeviceState::NotConnected | DeviceState::Disabled => {
                                        theme::TEXT_WEAK
                                    }
                                    _ => theme::TEXT,
                                });
                                ui.add(Label::new(text).truncate());
                            }
                            1 => {
                                ui.add(
                                    Label::new(RichText::new(&d.class).color(theme::TEXT_WEAK))
                                        .truncate(),
                                );
                            }
                            2 => {
                                ui.add(
                                    Label::new(
                                        RichText::new(d.state.label()).color(state_color(d.state)),
                                    )
                                    .truncate(),
                                );
                            }
                            3 => {
                                ui.add(Label::new(driver_line(d)).truncate());
                            }
                            4 => {
                                let date = d
                                    .driver
                                    .as_ref()
                                    .and_then(|x| x.date)
                                    .map(|t| when(t).chars().take(10).collect::<String>())
                                    .unwrap_or_default();
                                ui.label(RichText::new(date).color(theme::TEXT_WEAK));
                            }
                            _ => {
                                ui.add(
                                    Label::new(
                                        RichText::new(d.manufacturer.as_deref().unwrap_or(""))
                                            .color(theme::TEXT_WEAK),
                                    )
                                    .truncate(),
                                );
                            }
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(d.id.clone());
                    }
                });
            })
    });
    if let Some(id) = pick {
        *selected = if selected.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id)
        };
    }
}

/// One device in full; true when it is closed.
fn pane(ui: &mut Ui, d: &Device, out: &mut Vec<UiCmd>) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.add(Label::new(widgets::strong(&d.name, theme::SUBTITLE)).wrap());
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            close = ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Close")
                .clicked();
        });
    });
    ui.label(RichText::new(d.state.label()).color(state_color(d.state)));
    if let DeviceState::Problem(code) = d.state
        && let Some((what, helps)) = problem_words(code)
    {
        ui.add_space(4.0);
        ui.add(Label::new(what).wrap());
        ui.add(
            Label::new(
                RichText::new(format!("What usually helps: {helps}")).color(theme::TEXT_WEAK),
            )
            .wrap(),
        );
    }
    if let Some(code) = d.status_code {
        ui.add(
            Label::new(
                RichText::new(format!("The driver said: {}", status_words(code)))
                    .small()
                    .color(theme::TEXT_WEAK),
            )
            .wrap(),
        );
    }
    ui.add_space(6.0);
    panes::facts(ui, ("device-facts", &d.id), |ui| {
        panes::fact(ui, "Kind", d.class.clone());
        if let Some(m) = &d.manufacturer {
            panes::fact(ui, "Maker", m.clone());
        }
        if let Some(l) = &d.location {
            panes::fact(ui, "Where", l.clone());
        }
        if let Some(t) = d.installed {
            panes::fact(ui, "First installed", when(t));
        }
        if let Some(t) = d.arrived {
            panes::fact(ui, "Last connected", when(t));
        }
        if let Some(t) = d.removed {
            panes::fact(ui, "Last removed", when(t));
        }
        if d.hidden {
            panes::fact(ui, "Shown", "Only with hidden devices".to_string());
        }
        panes::fact(ui, "ID", d.id.clone());
        if let Some(p) = &d.parent {
            panes::fact(ui, "On", p.clone());
        }
    });
    if let Some(dr) = &d.driver {
        panes::section(ui, "Its driver");
        panes::facts(ui, ("device-driver", &d.id), |ui| {
            if let Some(p) = &dr.provider {
                panes::fact(ui, "From", p.clone());
            }
            if let Some(v) = &dr.version {
                panes::fact(ui, "Version", v.clone());
            }
            if let Some(t) = dr.date {
                panes::fact(ui, "Dated", when(t).chars().take(10).collect::<String>());
            }
            if let Some(s) = &dr.service {
                panes::fact(
                    ui,
                    if cfg!(windows) { "Service" } else { "Driver" },
                    s.clone(),
                );
            }
            if let Some(i) = &dr.inf {
                panes::fact(
                    ui,
                    if cfg!(windows) {
                        "Installed from"
                    } else {
                        "Module"
                    },
                    i.clone(),
                );
            }
        });
    } else if d.state != DeviceState::NotConnected {
        ui.add_space(6.0);
        panes::weak(ui, "No driver runs it.");
    }
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy the ID").clicked() {
            out.push(UiCmd::Copy(d.id.clone()));
        }
        if ui.button("Search online").clicked() {
            let q = match d.state {
                DeviceState::Problem(code) => format!("{} code {code}", d.name),
                _ => d.name.clone(),
            };
            out.push(UiCmd::SearchOnline(q));
        }
        #[cfg(windows)]
        if ui
            .button("Device Manager")
            .on_hover_text("Windows' own, to update, turn off or uninstall a device")
            .clicked()
        {
            out.push(UiCmd::OpenUrl("devmgmt.msc".into()));
        }
    });
    close
}

/// What the driver's status code says.
fn status_words(code: u32) -> String {
    #[cfg(windows)]
    return anabolic_core::ntapi::status_message(code as i32);
    #[cfg(not(windows))]
    format!("0x{code:08X}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anabolic_core::devices::Driver;

    #[test]
    fn says_how_many_are_connected_and_working() {
        let device = |state, hidden| Device {
            state,
            hidden,
            ..Device::default()
        };
        let d = Devices {
            devices: vec![
                device(DeviceState::Working, false),
                device(DeviceState::Working, true),
                device(DeviceState::NotConnected, false),
                device(DeviceState::Problem(43), false),
            ],
            ..Devices::default()
        };
        assert_eq!(summary(Some(&d)), "2 connected  ·  1 to look at");
        let one = Device {
            driver: Some(Driver {
                provider: Some("NVIDIA".into()),
                version: Some("32.0.15.9174".into()),
                ..Driver::default()
            }),
            ..Device::default()
        };
        assert_eq!(driver_line(&one), "NVIDIA 32.0.15.9174");
    }
}
