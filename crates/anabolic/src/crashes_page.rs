// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Crashes page: the programs that crashed or hung, the ones Windows
//! thought were leaking, the drivers it had to reset and the stop errors,
//! from the system's own records. By program, with the module most of a
//! program's crashes were in, or each one with its cause in words and the
//! dump Windows kept. Read in the background now and then, for the
//! Overview, and again on request.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::crashes::{Crash, CrashKind, Crashes, Group, cause, groups};
use anabolic_core::sysinfo::{local_time, now_filetime};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Label, Layout, RichText, Sense, Ui};

use crate::proc_table::UiCmd;
use crate::table::{Columns, Spec};
use crate::{panes, theme, widgets};

/// How often the records are read again while Anabolic runs: the Overview
/// names a program that keeps crashing.
const EVERY: Duration = Duration::from_secs(10 * 60);
const DAY: i64 = 86_400 * 10_000_000;

#[derive(Default)]
pub struct CrashesView {
    read: Option<Result<Arc<Crashes>, String>>,
    reading: Option<Receiver<Result<Crashes, String>>>,
    read_at: Option<Instant>,
    /// Each one, not by program.
    pub each: bool,
    /// Only crashes, hangs, driver resets and stop errors.
    pub serious: bool,
    pub filter: String,
    /// The program a group's click narrowed the list to.
    pub program: Option<String>,
    /// The crash in the pane: when it was and its program.
    selected: Option<(i64, String)>,
    groups: Option<(Arc<Crashes>, Vec<Group>)>,
}

impl CrashesView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("crashes".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::crashes::read());
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

    fn poll(&mut self) {
        if let Some(rx) = &self.reading
            && let Ok(result) = rx.try_recv()
        {
            self.read = Some(result.map(Arc::new));
            self.reading = None;
        }
    }

    /// Reads the records when they are due; call every frame.
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.poll();
        let due = self.read_at.is_none_or(|at| at.elapsed() >= EVERY);
        if due && self.reading.is_none() {
            self.start(ctx);
        }
    }

    pub fn crashes(&self) -> Option<&Arc<Crashes>> {
        self.read.as_ref()?.as_ref().ok()
    }

    /// Shows one program's crashes.
    pub fn show_program(&mut self, program: &str) {
        self.each = true;
        self.program = Some(program.to_string());
        self.selected = None;
    }
}

/// A program that keeps crashing: three serious problems or more in the
/// last day. Its name and how many.
pub fn keeps_crashing(crashes: &Crashes) -> Option<(String, usize)> {
    let since = now_filetime() - DAY;
    groups(&crashes.crashes)
        .into_iter()
        .filter(|g| g.program != "Windows")
        .map(|g| {
            let n = g
                .items
                .iter()
                .filter(|&&i| {
                    let c = &crashes.crashes[i];
                    c.at >= since && c.kind.serious()
                })
                .count();
            (g.program, n)
        })
        .filter(|(_, n)| *n >= 3)
        .max_by_key(|(_, n)| *n)
}

fn when(at: i64) -> String {
    local_time(at)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

fn when_exactly(at: i64) -> String {
    local_time(at)
        .map(|t| t.chars().take(19).collect())
        .unwrap_or_default()
}

/// "3 min", "2 h 5 min", "4 d".
fn ran(ms: u64) -> String {
    if ms < 60_000 {
        format!("{} s", ms / 1000)
    } else if ms < 2 * 86_400_000 {
        crate::fmt::span(Duration::from_millis(ms))
    } else {
        format!("{} d", ms / 86_400_000)
    }
}

fn kind_color(kind: CrashKind) -> egui::Color32 {
    match kind {
        CrashKind::Crash | CrashKind::StopError => theme::BAD,
        CrashKind::Hang | CrashKind::DriverReset => theme::ELEVATED_TEXT,
        CrashKind::Stall | CrashKind::Leak => theme::TEXT_WEAK,
    }
}

/// What it was, short: the cause in words, or the detail's first line.
fn what(c: &Crash) -> String {
    cause(c)
        .or_else(|| {
            c.detail
                .as_deref()
                .and_then(|d| d.lines().next())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn matches(c: &Crash, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    let has = |s: Option<&str>| s.is_some_and(|s| s.to_lowercase().contains(&f));
    has(Some(&c.program))
        || has(c.path.as_deref())
        || has(c.module.as_deref())
        || has(c.detail.as_deref())
        || has(Some(&what(c)))
        || has(Some(c.kind.label()))
}

pub fn show(ui: &mut Ui, view: &mut CrashesView, out: &mut Vec<UiCmd>) {
    view.tick(ui.ctx());
    let crashes = match &view.read {
        None => {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the system's records…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(
                RichText::new(format!("The crashes could not be read: {e}")).color(theme::BAD),
            );
            return;
        }
        Some(Ok(c)) => c.clone(),
    };
    let fresh = !view
        .groups
        .as_ref()
        .is_some_and(|(of, _)| Arc::ptr_eq(of, &crashes));
    if fresh {
        view.groups = Some((crashes.clone(), groups(&crashes.crashes)));
    }

    header(ui, view, &crashes);
    ui.add_space(4.0);
    if crashes.crashes.is_empty() {
        ui.add_space(12.0);
        panes::weak(
            ui,
            "Nothing crashed, hung or had to be reset as far back as the records go.",
        );
        return;
    }

    let shown: Vec<usize> = (0..crashes.crashes.len())
        .filter(|&i| {
            let c = &crashes.crashes[i];
            (!view.serious || c.kind.serious())
                && view
                    .program
                    .as_deref()
                    .is_none_or(|p| c.program.eq_ignore_ascii_case(p))
                && matches(c, view.filter.trim())
        })
        .collect();

    if view.each {
        let selected = view.selected.as_ref().and_then(|(at, program)| {
            crashes
                .crashes
                .iter()
                .find(|c| c.at == *at && &c.program == program)
        });
        if let Some(c) = selected {
            let mut close = false;
            panes::side(ui, ("crash-pane", c.at), |ui| close = pane(ui, c, out));
            if close {
                view.selected = None;
            }
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            each_table(ui, &crashes.crashes, &shown, &mut view.selected, out);
        });
    } else {
        let groups = view
            .groups
            .as_ref()
            .map(|(_, g)| g.as_slice())
            .unwrap_or(&[]);
        let shown_groups: Vec<&Group> = groups
            .iter()
            .filter(|g| g.items.iter().any(|i| shown.contains(i)))
            .collect();
        if let Some(program) = group_table(ui, &crashes.crashes, &shown_groups, &shown) {
            view.show_program(&program);
        }
    }
}

fn header(ui: &mut Ui, view: &mut CrashesView, crashes: &Crashes) {
    let count = |k: CrashKind| crashes.crashes.iter().filter(|c| c.kind == k).count();
    let mut parts = Vec::new();
    for (k, word) in [
        (CrashKind::Crash, "crashed"),
        (CrashKind::Hang, "hung"),
        (CrashKind::DriverReset, "driver resets"),
        (CrashKind::StopError, "stop errors"),
        (CrashKind::Stall, "stalled"),
        (CrashKind::Leak, "thought to leak"),
    ] {
        let n = count(k);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    let since = crashes
        .since
        .or_else(|| crashes.crashes.iter().map(|c| c.at).min())
        .map(|t| format!(" since {}", when(t).chars().take(10).collect::<String>()))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{}{since}  ·  {}  ·  read in {}",
                if parts.is_empty() {
                    "None".to_string()
                } else {
                    parts.join(", ")
                },
                if cfg!(windows) {
                    "from Windows Error Reporting and the event logs"
                } else {
                    "from systemd-coredump"
                },
                crate::fmt::millis(crashes.took)
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
    for note in &crashes.notes {
        ui.label(RichText::new(note).small().color(theme::ELEVATED_TEXT));
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if ui.selectable_label(!view.each, "By program").clicked() {
            view.each = false;
        }
        if ui.selectable_label(view.each, "Each one").clicked() {
            view.each = true;
        }
        ui.add_space(8.0);
        ui.checkbox(&mut view.serious, "Only what went away")
            .on_hover_text("Crashes, hangs that ended a program, driver resets and stop errors");
        ui.add_space(8.0);
        if let Some(program) = view.program.clone() {
            let chip = ui
                .button(format!("{program}  {}", theme::icon::CLOSE))
                .on_hover_text("Show every program again");
            if chip.clicked() {
                view.program = None;
            }
        }
        widgets::clearable(ui, &mut view.filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter: program, module, cause")
                .desired_width(240.0)
        });
    });
}

const GROUP_TITLES: [(&str, bool); 5] = [
    ("Program", false),
    ("Times", true),
    ("Latest", false),
    ("Most often in", false),
    ("What happened", false),
];
const GROUP_COLUMNS: [Spec; 5] = [
    Spec::new(220.0).clip(),
    Spec::new(60.0),
    Spec::new(124.0),
    Spec::new(200.0).clip(),
    Spec::new(320.0).clip(),
];

/// The groups; the program clicked, if one was.
fn group_table(
    ui: &mut Ui,
    crashes: &[Crash],
    groups: &[&Group],
    shown: &[usize],
) -> Option<String> {
    let mut pick = None;
    Columns::load(ui.ctx(), "crash-groups", &GROUP_COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = GROUP_TITLES[c];
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, groups.len(), |mut row| {
                    let g = groups[row.index()];
                    let items: Vec<usize> = g
                        .items
                        .iter()
                        .copied()
                        .filter(|i| shown.contains(i))
                        .collect();
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                let label = ui.add(
                                    Label::new(widgets::strong(&g.program, theme::BODY)).truncate(),
                                );
                                if let Some(path) = &g.path {
                                    label.on_hover_text(path);
                                }
                            }
                            1 => {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(items.len().to_string());
                                });
                            }
                            2 => {
                                ui.label(when(g.latest));
                            }
                            3 => {
                                if let Some((module, n)) = &g.module {
                                    let text = if *n > 1 {
                                        format!("{module} ({n})")
                                    } else {
                                        module.clone()
                                    };
                                    ui.add(Label::new(text).truncate());
                                }
                            }
                            _ => {
                                let mut kinds: Vec<(CrashKind, usize)> = Vec::new();
                                for &i in &items {
                                    let k = crashes[i].kind;
                                    match kinds.iter_mut().find(|(x, _)| *x == k) {
                                        Some((_, n)) => *n += 1,
                                        None => kinds.push((k, 1)),
                                    }
                                }
                                kinds.sort();
                                let text = kinds
                                    .iter()
                                    .map(|(k, n)| format!("{} ×{n}", k.label()))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                let first = kinds.first().map_or(CrashKind::Leak, |(k, _)| *k);
                                ui.add(
                                    Label::new(RichText::new(text).color(kind_color(first)))
                                        .truncate(),
                                );
                            }
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(g.program.clone());
                    }
                });
            })
    });
    pick
}

const TITLES: [(&str, bool); 7] = [
    ("When", false),
    ("Program", false),
    ("What", false),
    ("Cause", false),
    ("Module", false),
    ("Ran for", true),
    ("Dump", false),
];
const COLUMNS: [Spec; 7] = [
    Spec::new(124.0),
    Spec::new(180.0).clip(),
    Spec::new(130.0).clip(),
    Spec::new(320.0).clip(),
    Spec::new(170.0).clip(),
    Spec::new(70.0),
    Spec::new(60.0),
];

fn each_table(
    ui: &mut Ui,
    crashes: &[Crash],
    shown: &[usize],
    selected: &mut Option<(i64, String)>,
    out: &mut Vec<UiCmd>,
) {
    let mut pick = None;
    Columns::load(ui.ctx(), "crashes", &COLUMNS).show(ui, |table, columns| {
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
                    let c = &crashes[shown[row.index()]];
                    let is = selected
                        .as_ref()
                        .is_some_and(|(at, p)| *at == c.at && *p == c.program);
                    row.set_selected(is);
                    for &col in columns.order() {
                        row.col(|ui| match col {
                            0 => {
                                ui.label(when(c.at));
                            }
                            1 => {
                                ui.add(Label::new(&c.program).truncate());
                            }
                            2 => {
                                ui.add(
                                    Label::new(
                                        RichText::new(c.kind.label()).color(kind_color(c.kind)),
                                    )
                                    .truncate(),
                                );
                            }
                            3 => {
                                ui.add(
                                    Label::new(RichText::new(what(c)).color(theme::TEXT_WEAK))
                                        .truncate(),
                                );
                            }
                            4 => {
                                ui.add(Label::new(c.module.as_deref().unwrap_or("")).truncate());
                            }
                            5 => {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(c.ran_ms.map(ran).unwrap_or_default());
                                });
                            }
                            _ => {
                                if c.dump.is_some() {
                                    ui.label(RichText::new("kept").color(theme::TEXT_WEAK));
                                }
                            }
                        });
                    }
                    let response = row.response();
                    if response.clicked() {
                        pick = Some((c.at, c.program.clone()));
                    }
                    response.context_menu(|ui| menu(ui, c, out));
                });
            })
    });
    if let Some(p) = pick {
        *selected = if selected.as_ref() == Some(&p) {
            None
        } else {
            Some(p)
        };
    }
}

fn menu(ui: &mut Ui, c: &Crash, out: &mut Vec<UiCmd>) {
    let mut item = |ui: &mut Ui, text: &str, cmd: UiCmd| {
        if ui.button(text).clicked() {
            out.push(cmd);
            ui.close();
        }
    };
    if let Some(dump) = &c.dump {
        item(
            ui,
            "Open the dump's location",
            UiCmd::OpenLocation(dump.clone()),
        );
    }
    if let Some(report) = &c.report {
        item(
            ui,
            "Open the report's folder",
            UiCmd::OpenLocation(report.clone()),
        );
    }
    if let Some(path) = &c.path {
        item(
            ui,
            "Open the program's location",
            UiCmd::OpenLocation(path.clone()),
        );
    }
    item(ui, "Copy the details", UiCmd::Copy(details(c)));
    if let Some(q) = search(c) {
        item(ui, "Search online", UiCmd::SearchOnline(q));
    }
}

/// What to search the web for: the program, the module and the code.
fn search(c: &Crash) -> Option<String> {
    let mut words = vec![c.program.clone()];
    if let Some(m) = &c.module {
        words.push(m.clone());
    }
    if let Some(code) = c.code {
        words.push(if cfg!(windows) {
            format!("0x{code:08X}")
        } else {
            format!("signal {code}")
        });
    }
    (words.len() > 1).then(|| words.join(" "))
}

/// The crash in full, as text to paste into a bug report.
fn details(c: &Crash) -> String {
    let mut lines = vec![format!(
        "{} {} at {}",
        c.program,
        c.kind.label().to_lowercase(),
        when_exactly(c.at)
    )];
    let mut add = |key: &str, value: Option<String>| {
        if let Some(v) = value {
            lines.push(format!("{key}: {v}"));
        }
    };
    add("Path", c.path.clone());
    add("Version", c.version.clone());
    add("Package", c.package.clone());
    add("Process ID", c.pid.map(|p| p.to_string()));
    add("Ran for", c.ran_ms.map(ran));
    add("Cause", cause(c));
    add("Module", c.module.clone());
    add("Module path", c.module_path.clone());
    add("Module version", c.module_version.clone());
    add("Offset", c.offset.map(|o| format!("0x{o:X}")));
    add("Dump", c.dump.clone());
    add("Report", c.report.clone());
    if let Some(d) = &c.detail {
        lines.push(String::new());
        lines.push(d.clone());
    }
    lines.join("\n")
}

/// One crash in full; true when it is closed.
fn pane(ui: &mut Ui, c: &Crash, out: &mut Vec<UiCmd>) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(widgets::strong(&c.program, theme::SUBTITLE));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Close")
                .clicked();
        });
    });
    ui.label(RichText::new(c.kind.label()).color(kind_color(c.kind)));
    ui.add_space(6.0);
    panes::facts(ui, ("crash-facts", c.at), |ui| {
        panes::fact(ui, "When", when_exactly(c.at));
        if let Some(ms) = c.ran_ms {
            panes::fact(ui, "It had run", ran(ms));
        }
        if let Some(why) = cause(c) {
            panes::fact(ui, "Cause", why);
        }
        if let Some(m) = &c.module {
            let v = c
                .module_version
                .as_deref()
                .map_or(m.clone(), |v| format!("{m}, version {v}"));
            panes::fact(ui, "In module", v);
        }
        if let Some(p) = &c.module_path {
            panes::fact(ui, "Module's file", p.clone());
        }
        if let Some(o) = c.offset {
            panes::fact(ui, "At", format!("0x{o:X} into it"));
        }
        if let Some(v) = &c.version {
            panes::fact(ui, "Version", v.clone());
        }
        if let Some(p) = &c.path {
            panes::fact(ui, "Program", p.clone());
        }
        if let Some(p) = &c.package {
            panes::fact(ui, "Package", p.clone());
        }
        if let Some(pid) = c.pid {
            panes::fact(ui, "Process ID", pid.to_string());
        }
        if let Some(d) = &c.dump {
            panes::fact(ui, "Dump", d.clone());
        }
        if let Some(r) = &c.report {
            panes::fact(ui, "Report", r.clone());
        }
    });
    if let Some(d) = &c.detail {
        panes::section(ui, "In more words");
        ui.add(
            Label::new(RichText::new(d).monospace().size(11.5))
                .wrap()
                .selectable(true),
        );
    }
    if c.kind == CrashKind::Leak {
        ui.add_space(6.0);
        panes::weak(
            ui,
            "Windows' resource exhaustion detector saw this program's memory keep growing \
             and took a note. It is a hint, not a proof: a program that holds a lot on \
             purpose looks the same.",
        );
    }
    if c.kind == CrashKind::DriverReset {
        ui.add_space(6.0);
        panes::weak(
            ui,
            "Windows reset the driver instead of stopping. Many in a short while point at the \
             driver, the card's clocks or its power.",
        );
    }
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        if let Some(dump) = &c.dump
            && ui.button("Open the dump's location").clicked()
        {
            out.push(UiCmd::OpenLocation(dump.clone()));
        }
        if let Some(report) = &c.report
            && ui.button("Report folder").clicked()
        {
            out.push(UiCmd::OpenLocation(report.clone()));
        }
        if ui.button("Copy the details").clicked() {
            out.push(UiCmd::Copy(details(c)));
        }
        if let Some(q) = search(c)
            && ui.button("Search online").clicked()
        {
            out.push(UiCmd::SearchOnline(q));
        }
    });
    close
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_a_crash_in_full() {
        let c = Crash {
            at: 0,
            kind: CrashKind::Crash,
            program: "chrome.exe".into(),
            module: Some("ntdll.dll".into()),
            code: Some(0xC000_0008),
            offset: Some(0x165e6a),
            ..Crash::default()
        };
        let text = details(&c);
        assert!(text.starts_with("chrome.exe crashed at"));
        assert!(text.contains("Module: ntdll.dll"));
        assert!(text.contains("Offset: 0x165E6A"));
        if cfg!(windows) {
            assert_eq!(
                search(&c).as_deref(),
                Some("chrome.exe ntdll.dll 0xC0000008")
            );
        }
        assert_eq!(ran(90_000), crate::fmt::span(Duration::from_millis(90_000)));
    }

    #[test]
    fn names_a_program_that_keeps_crashing() {
        let now = now_filetime();
        let crash = |program: &str, ago: i64, kind| Crash {
            at: now - ago,
            kind,
            program: program.into(),
            ..Crash::default()
        };
        let list = Crashes {
            crashes: vec![
                crash("a.exe", 10, CrashKind::Crash),
                crash("a.exe", 20, CrashKind::Hang),
                crash("a.exe", 30, CrashKind::Crash),
                crash("b.exe", 10, CrashKind::Crash),
                crash("b.exe", 10, CrashKind::Leak),
                crash("b.exe", 10, CrashKind::Leak),
                crash("a.exe", 2 * DAY, CrashKind::Crash),
            ],
            ..Crashes::default()
        };
        assert_eq!(keeps_crashing(&list), Some(("a.exe".to_string(), 3)));
    }
}
