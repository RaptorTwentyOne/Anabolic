//! The side pane of the Threads, Modules and Handles tabs: everything about
//! the selected row, and what can be done with it. Each row's context menu
//! offers the same actions.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anabolic_core::ending::Holder;
use anabolic_core::filefacts::{FileFacts, Signature};
use anabolic_core::handles::{FoundHandle, HandleEntry, HandleReport, ModuleReport, ModuleRow};
use anabolic_core::nettrace::Proto;
use anabolic_core::sysinfo::{local_time, now_filetime, thread_state_name, wait_reason_name};
use anabolic_core::threads::{PRIORITIES, StackReport, ThreadOp};
use anabolic_core::{Connection, ProcKey, ProcState, ProcessInfo, Snapshot, ThreadInfo};
use eframe::egui::{self, Grid, Label, RichText, ScrollArea, Ui, WidgetText};

use crate::proc_table::{UiCmd, item};
use crate::{access, fmt, theme, widgets};

/// What the panes show that the inspector reads on request.
#[derive(Default)]
pub struct Panes {
    pub stack: Option<Arc<StackReport>>,
    pub stack_pending: Option<(ProcKey, u32)>,
    pub stack_error: Option<((ProcKey, u32), String)>,
    /// Processes whose thread stacks the user agreed to read.
    pub stack_ok: HashSet<ProcKey>,
    /// By file path; files do not change under a running module.
    pub facts: HashMap<Arc<str>, Result<Arc<FileFacts>, String>>,
    pub facts_pending: Option<Arc<str>>,
    /// Holders of one kernel object, for the handle on display.
    pub holders: Option<(ProcKey, u64, Result<Vec<Holder>, String>)>,
    pub holders_pending: Option<(ProcKey, u64)>,
}

pub const WIDTH: f32 = 390.0;

/// A pane on the right of the tab; call before the tab's central panel.
pub fn side(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    egui::Panel::right(egui::Id::new(id))
        .exact_size(WIDTH)
        .resizable(false)
        .show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(2.0);
                    add(ui);
                });
        });
}

pub fn title(ui: &mut Ui, text: String, sub: Option<&str>) {
    ui.label(widgets::strong(text, theme::SUBTITLE));
    if let Some(sub) = sub.filter(|s| !s.is_empty()) {
        ui.add(Label::new(RichText::new(sub).color(theme::TEXT_WEAK)).wrap());
    }
    ui.add_space(6.0);
}

pub fn facts(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    Grid::new(egui::Id::new(id))
        .num_columns(2)
        .spacing([10.0, 4.0])
        .min_col_width(72.0)
        .max_col_width(WIDTH - 100.0)
        .show(ui, add);
}

pub fn fact(ui: &mut Ui, key: &str, value: impl Into<WidgetText>) {
    ui.label(RichText::new(key).color(theme::TEXT_WEAK));
    ui.add(Label::new(value).wrap().selectable(true));
    ui.end_row();
}

pub fn section(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(widgets::strong(text, theme::BODY));
    ui.add_space(2.0);
}

pub fn weak(ui: &mut Ui, text: impl Into<String>) {
    ui.add(Label::new(RichText::new(text.into()).color(theme::TEXT_WEAK)).wrap());
}

fn button(ui: &mut Ui, text: &str, tip: &str, out: &mut Vec<UiCmd>, cmd: UiCmd) {
    let response = ui.button(text);
    let response = if tip.is_empty() {
        response
    } else {
        response.on_hover_text(tip)
    };
    if response.clicked() {
        out.push(cmd);
    }
}

fn ago(filetime: i64) -> Option<String> {
    let at = local_time(filetime)?;
    let age = (now_filetime() - filetime).max(0) as u64;
    Some(format!(
        "{at}  ·  {} ago",
        fmt::duration(std::time::Duration::from_nanos(age * 100))
    ))
}

fn link_to(ui: &mut Ui, pid: u32, name: &str, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    match snap
        .processes
        .iter()
        .find(|q| q.pid == pid && q.state != ProcState::Exited)
    {
        Some(q) => {
            if widgets::link(ui, &format!("{name} ({pid})")).clicked() {
                out.push(UiCmd::Select(Some(q.key)));
            }
        }
        None => {
            ui.label(RichText::new(format!("{name} ({pid})")).color(theme::TEXT_WEAK));
        }
    }
}

// Threads ------------------------------------------------------------------

/// The SetThreadPriority level a thread runs at, from its base priority and
/// its process'.
fn thread_level(t: &ThreadInfo, process_base: i32) -> Option<i32> {
    match t.base_priority {
        15 | 31 => Some(15),
        1 | 16 => Some(-15),
        b => Some(b - process_base).filter(|rel| (-2..=2).contains(rel)),
    }
}

fn suspended(t: &ThreadInfo) -> bool {
    t.state == 5 && t.wait_reason == 5
}

pub fn thread_menu(
    ui: &mut Ui,
    p: &ProcessInfo,
    t: &ThreadInfo,
    start: &str,
    out: &mut Vec<UiCmd>,
) {
    let (key, tid) = (p.key, t.tid);
    item(ui, out, "Call stack", "", UiCmd::ConfirmStack { key, tid });
    item(
        ui,
        out,
        "Wait chain of this thread",
        "",
        UiCmd::ConfirmThreadWaitChain { key, tid },
    );
    ui.separator();
    let op = if suspended(t) {
        ("Resume thread", ThreadOp::Resume)
    } else {
        ("Suspend thread", ThreadOp::Suspend)
    };
    item(ui, out, op.0, "", UiCmd::Thread { key, tid, op: op.1 });
    let level = thread_level(t, p.base_priority);
    ui.menu_button("Priority", |ui| {
        for (l, name) in PRIORITIES {
            if ui.radio(level == Some(l), name).clicked() {
                out.push(UiCmd::Thread {
                    key,
                    tid,
                    op: ThreadOp::Priority(l),
                });
                ui.close();
            }
        }
    });
    item(
        ui,
        out,
        "End thread\u{2026}",
        "",
        UiCmd::ConfirmEndThread { key, tid },
    );
    ui.separator();
    item(ui, out, "Copy thread ID", "", UiCmd::Copy(tid.to_string()));
    item(
        ui,
        out,
        "Copy start address",
        "",
        UiCmd::Copy(start.to_string()),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn thread_pane(
    ui: &mut Ui,
    p: &ProcessInfo,
    t: &ThreadInfo,
    report: Option<&ModuleReport>,
    start: &str,
    start_address: u64,
    panes: &Panes,
    out: &mut Vec<UiCmd>,
) {
    let (key, tid) = (p.key, t.tid);
    let name = report.and_then(|r| r.thread_names.get(&tid));
    title(ui, format!("Thread {tid}"), name.map(|s| s.as_str()));
    facts(ui, ("thread-facts", tid), |ui| {
        let state = if t.state == 5 {
            format!("Waiting  ·  {}", wait_reason_name(t.wait_reason))
        } else {
            thread_state_name(t.state).to_string()
        };
        fact(ui, "State", state);
        fact(ui, "Starts in", RichText::new(start).monospace());
        if let Some(m) = report.and_then(|r| {
            r.modules
                .iter()
                .find(|m| start_address >= m.info.base && start_address < m.info.base + m.info.size)
        }) {
            fact(
                ui,
                "Module",
                RichText::new(&*m.info.path).color(theme::TEXT_DIM),
            );
        }
        if let Some(created) = ago(t.create_time) {
            fact(ui, "Created", created);
        }
        let level = thread_level(t, p.base_priority)
            .and_then(|l| PRIORITIES.iter().find(|(x, _)| *x == l))
            .map_or("Custom", |(_, n)| n);
        fact(
            ui,
            "Priority",
            format!("{level}  (base {}, now {})", t.base_priority, t.priority),
        );
        let cpu = fmt::cpu(t.cpu);
        fact(
            ui,
            "CPU",
            format!(
                "{} %  ·  {} kernel, {} user",
                if cpu.is_empty() { "0" } else { &cpu },
                fmt::cpu_time(t.kernel_time),
                fmt::cpu_time(t.user_time)
            ),
        );
        fact(
            ui,
            "Switches",
            format!(
                "{} in all  ·  {}/s",
                fmt::count(t.context_switches as u64),
                fmt::count(t.context_switches_delta as u64)
            ),
        );
        // The thread list reports the kernel-mode stack.
        if t.stack_limit != 0 && t.stack_base > t.stack_limit {
            fact(ui, "Kernel stack", fmt::bytes(t.stack_base - t.stack_limit));
        }
        if t.teb != 0 {
            fact(
                ui,
                "TEB",
                RichText::new(format!("0x{:X}", t.teb)).monospace(),
            );
        }
    });

    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        let (label, op) = if suspended(t) {
            ("Resume", ThreadOp::Resume)
        } else {
            ("Suspend", ThreadOp::Suspend)
        };
        button(ui, label, "", out, UiCmd::Thread { key, tid, op });
        button(
            ui,
            "End\u{2026}",
            "End this thread only",
            out,
            UiCmd::ConfirmEndThread { key, tid },
        );
        let level = thread_level(t, p.base_priority);
        ui.menu_button("Priority", |ui| {
            for (l, name) in PRIORITIES {
                if ui.radio(level == Some(l), name).clicked() {
                    out.push(UiCmd::Thread {
                        key,
                        tid,
                        op: ThreadOp::Priority(l),
                    });
                    ui.close();
                }
            }
        });
        button(
            ui,
            "Wait chain",
            "What this thread waits for, across processes",
            out,
            UiCmd::ConfirmThreadWaitChain { key, tid },
        );
    });

    section(ui, "Call stack");
    let pending = panes.stack_pending == Some((key, tid));
    let stack = panes
        .stack
        .as_ref()
        .filter(|s| s.key == key && s.tid == tid);
    ui.horizontal(|ui| {
        if pending {
            ui.spinner();
            ui.label(RichText::new("Reading\u{2026}").color(theme::TEXT_WEAK));
        } else {
            button(
                ui,
                if stack.is_some() {
                    "Read again"
                } else {
                    "Read"
                },
                "Suspends the thread for a few milliseconds to read its stack \
                 (opens the process for reading).",
                out,
                UiCmd::ConfirmStack { key, tid },
            );
        }
        if let Some(s) = stack {
            let text: Vec<String> = s
                .frames
                .iter()
                .map(|f| {
                    f.symbol
                        .clone()
                        .unwrap_or_else(|| format!("0x{:X}", f.address))
                })
                .collect();
            if ui.small_button("Copy").clicked() {
                out.push(UiCmd::Copy(text.join("\r\n")));
            }
        }
    });
    if let Some(((k, t_), e)) = &panes.stack_error
        && *k == key
        && *t_ == tid
    {
        ui.label(RichText::new(format!("Could not read it: {e}")).color(theme::ELEVATED_TEXT));
    }
    match stack {
        Some(s) => {
            ui.label(
                RichText::new(format!(
                    "{} frames, innermost first  ·  user mode only{}  ·  held {}",
                    s.frames.len(),
                    if s.wow64 { ", 32-bit" } else { "" },
                    fmt::millis(s.held)
                ))
                .size(theme::META)
                .color(theme::TEXT_WEAK),
            );
            for f in &s.frames {
                let text = f
                    .symbol
                    .clone()
                    .unwrap_or_else(|| format!("0x{:X}", f.address));
                ui.add(
                    Label::new(RichText::new(text).monospace().size(12.0))
                        .truncate()
                        .selectable(true),
                )
                .on_hover_text(format!("0x{:X}", f.address));
            }
        }
        None if !pending => weak(
            ui,
            "Which functions the thread is in right now: what it waits for, or what keeps it busy.",
        ),
        None => {}
    }
}

// Modules ------------------------------------------------------------------

fn virustotal(hash: &str) -> String {
    format!("https://www.virustotal.com/gui/file/{hash}")
}

pub fn module_menu(ui: &mut Ui, m: &ModuleRow, facts: Option<&FileFacts>, out: &mut Vec<UiCmd>) {
    let path = m.info.path.to_string();
    item(
        ui,
        out,
        "Open file location",
        "",
        UiCmd::OpenLocation(path.clone()),
    );
    item(ui, out, "Properties", "", UiCmd::Properties(path.clone()));
    ui.separator();
    item(ui, out, "Copy path", "", UiCmd::Copy(path));
    item(
        ui,
        out,
        "Copy name",
        "",
        UiCmd::Copy(m.info.name.to_string()),
    );
    ui.separator();
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(m.info.name.to_string()),
    );
    match facts.and_then(|f| f.sha256.as_deref()) {
        Some(hash) => item(
            ui,
            out,
            "Look up on VirusTotal",
            "",
            UiCmd::OpenUrl(virustotal(hash)),
        ),
        None => {
            ui.add_enabled(false, egui::Button::new("Look up on VirusTotal"))
                .on_disabled_hover_text(
                    "Select the module first: the side pane computes its hash.",
                );
        }
    }
}

fn signature_text(s: &Signature) -> (String, egui::Color32) {
    match s {
        Signature::Signed { signer, catalog } => (
            format!(
                "Signed by {}{}",
                signer.as_deref().unwrap_or("an unnamed signer"),
                if *catalog { "  (Windows catalog)" } else { "" }
            ),
            theme::GOOD,
        ),
        Signature::Unsigned if cfg!(windows) => ("Not signed".into(), theme::ELEVATED_TEXT),
        // Linux files carry no signature: no package vouches for this one.
        Signature::Unsigned => (
            "Not from a package: built or copied here by hand".into(),
            theme::ELEVATED_TEXT,
        ),
        Signature::Invalid(why) => (format!("Signature does not hold: {why}"), theme::BAD),
        Signature::Package {
            manager,
            name,
            version,
            verified,
        } => {
            let from = format!("From the package {name} {version} ({manager})");
            match verified {
                Some(true) => (format!("{from}, unchanged since installed"), theme::GOOD),
                Some(false) => (
                    format!("{from}, but changed since it was installed"),
                    theme::BAD,
                ),
                None => (from, theme::GOOD),
            }
        }
    }
}

/// What Windows calls a file's company: on Linux, the package it came with.
const COMPANY: &str = if cfg!(windows) { "Company" } else { "Package" };

pub fn module_pane(ui: &mut Ui, m: &ModuleRow, panes: &Panes, out: &mut Vec<UiCmd>) {
    let path = m.info.path.clone();
    let known = panes.facts.get(&path);
    if known.is_none() && panes.facts_pending.as_ref() != Some(&path) {
        out.push(UiCmd::FileFacts(path.clone()));
    }
    title(ui, m.info.name.to_string(), m.description.as_deref());
    facts(ui, ("module-facts", m.info.base), |ui| {
        fact(ui, "Path", RichText::new(&*path).color(theme::TEXT_WEAK));
        fact(
            ui,
            "Address",
            RichText::new(format!(
                "0x{:X} \u{2013} 0x{:X}",
                m.info.base,
                m.info.base + m.info.size
            ))
            .monospace(),
        );
        fact(ui, "Size", fmt::bytes(m.info.size));
        if let Some(c) = &m.company {
            fact(ui, COMPANY, c.clone());
        }
        if let Some(v) = &m.version {
            fact(ui, "Version", v.clone());
        }
    });

    section(ui, "File");
    match known {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                weak(ui, "Checking its signature and hash\u{2026}");
            });
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("Could not read it: {e}")).color(theme::ELEVATED_TEXT));
        }
        Some(Ok(f)) => {
            let (text, color) = signature_text(&f.signature);
            ui.add(Label::new(RichText::new(text).color(color)).wrap());
            ui.add_space(4.0);
            facts(ui, ("module-file", m.info.base), |ui| {
                let modified = f.modified.and_then(local_time).unwrap_or_default();
                fact(
                    ui,
                    "On disk",
                    format!("{}  ·  written {modified}", fmt::bytes(f.size)),
                );
                if let Some(pe) = &f.pe {
                    let kind = if pe.dll { "DLL" } else { "program" };
                    let linked = if pe.reproducible {
                        "reproducible build (no link time)".to_string()
                    } else {
                        linked_text(pe.linked)
                    };
                    fact(
                        ui,
                        "Built",
                        format!(
                            "{} {kind}{}  ·  {linked}",
                            pe.machine,
                            if pe.dotnet { ", .NET" } else { "" }
                        ),
                    );
                    fact(ui, "Hardening", hardening(pe));
                    let base = if pe.image_base == m.info.base {
                        "at its preferred address".to_string()
                    } else {
                        format!("moved from its preferred 0x{:X}", pe.image_base)
                    };
                    fact(ui, "Loaded", base);
                }
                if let Some(elf) = &f.elf {
                    let kind = match (elf.library, elf.static_linked) {
                        (true, _) => "shared library",
                        (false, true) => "program, statically linked",
                        (false, false) => "program",
                    };
                    let symbols = if elf.stripped { ", stripped" } else { "" };
                    fact(ui, "Built", format!("{} {kind}{symbols}", elf.machine));
                    fact(ui, "Hardening", elf_hardening(elf));
                    if let Some(id) = &elf.build_id {
                        fact(
                            ui,
                            "Build ID",
                            RichText::new(id)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                }
                if let Some(hash) = &f.sha256 {
                    fact(
                        ui,
                        "SHA-256",
                        RichText::new(hash)
                            .monospace()
                            .size(11.0)
                            .color(theme::TEXT_WEAK),
                    );
                }
            });
        }
    }

    ui.add_space(8.0);
    let file = known.and_then(|k| k.as_ref().ok()).cloned();
    ui.horizontal_wrapped(|ui| {
        button(
            ui,
            "Open location",
            "",
            out,
            UiCmd::OpenLocation(path.to_string()),
        );
        button(
            ui,
            "Properties",
            "",
            out,
            UiCmd::Properties(path.to_string()),
        );
        button(ui, "Copy path", "", out, UiCmd::Copy(path.to_string()));
        if let Some(hash) = file.as_ref().and_then(|f| f.sha256.as_deref()) {
            button(
                ui,
                "VirusTotal",
                "Opens VirusTotal's page for this file's hash in your browser",
                out,
                UiCmd::OpenUrl(virustotal(hash)),
            );
        }
    });
}

/// Link time; a stamp that cannot be one says so.
fn linked_text(stamp: u32) -> String {
    let now = (now_filetime() - 116_444_736_000_000_000) / 10_000_000;
    if stamp == 0 || stamp as i64 > now + 86_400 * 365 {
        return "no link time recorded".into();
    }
    let filetime = stamp as i64 * 10_000_000 + 116_444_736_000_000_000;
    local_time(filetime).map_or_else(String::new, |t| format!("linked {t}"))
}

/// The exploit mitigations the file opts into, those it lacks dimmed.
fn hardening(pe: &anabolic_core::filefacts::PeFacts) -> egui::text::LayoutJob {
    mitigations(&[
        ("ASLR", pe.aslr),
        ("high-entropy ASLR", pe.high_entropy),
        ("DEP", pe.dep),
        ("CFG", pe.cfg),
        ("integrity check", pe.force_integrity),
    ])
}

/// The same for a Linux file: a random load address (PIE; libraries always
/// have one), read-only relocations, a stack that cannot run code, stack
/// canaries, checked libc calls and Intel CET.
fn elf_hardening(elf: &anabolic_core::filefacts::ElfFacts) -> egui::text::LayoutJob {
    let relro = if elf.bind_now { "full RELRO" } else { "RELRO" };
    let mut items = Vec::new();
    if !elf.library {
        items.push(("PIE", elf.pie));
    }
    items.extend([
        (relro, elf.relro),
        ("NX stack", elf.nx_stack),
        ("stack protector", elf.stack_protector),
        ("FORTIFY", elf.fortify),
        ("CET", elf.cet),
    ]);
    mitigations(&items)
}

fn mitigations(items: &[(&str, bool)]) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for (i, (name, on)) in items.iter().enumerate() {
        if i > 0 {
            job.append(
                "  ",
                0.0,
                egui::TextFormat::simple(egui::FontId::proportional(theme::BODY), theme::TEXT_DIM),
            );
        }
        let (mark, color) = if *on {
            ("\u{2713} ", theme::GOOD)
        } else {
            ("\u{2717} ", theme::TEXT_DIM)
        };
        job.append(
            &format!("{mark}{name}"),
            0.0,
            egui::TextFormat::simple(egui::FontId::proportional(theme::BODY), color),
        );
    }
    job.wrap.max_width = WIDTH - 110.0;
    job
}

// Handles ------------------------------------------------------------------

fn looks_like_path(name: &str) -> bool {
    name.as_bytes().get(1) == Some(&b':') && name.as_bytes().get(2) == Some(&b'\\')
}

pub fn handle_menu(
    ui: &mut Ui,
    p: &ProcessInfo,
    type_name: &str,
    e: &HandleEntry,
    name: Option<&str>,
    out: &mut Vec<UiCmd>,
) {
    if let Some(name) = name {
        item(
            ui,
            out,
            "Find in all processes",
            "",
            UiCmd::FindText(name.to_string()),
        );
        if type_name == "File" && looks_like_path(name) {
            item(
                ui,
                out,
                "Open file location",
                "",
                UiCmd::OpenLocation(name.to_string()),
            );
        }
        item(ui, out, "Copy name", "", UiCmd::Copy(name.to_string()));
    }
    item(
        ui,
        out,
        if cfg!(windows) {
            "Copy handle value"
        } else {
            "Copy file descriptor"
        },
        "",
        UiCmd::Copy(crate::details::handles::value_text(e.value)),
    );
    // Linux does not let one program close another's files.
    if cfg!(windows) {
        ui.separator();
        item(
            ui,
            out,
            "Close handle\u{2026}",
            "",
            UiCmd::ConfirmCloseHandle(FoundHandle {
                pid: p.pid,
                process: p.name.clone(),
                handle: e.value,
                type_name: Arc::from(type_name),
                name: name.unwrap_or("this handle").to_string(),
            }),
        );
    }
}

/// For a row of handles grouped by name.
pub fn group_menu(ui: &mut Ui, type_name: &str, name: &str, out: &mut Vec<UiCmd>) {
    item(
        ui,
        out,
        "Find in all processes",
        "",
        UiCmd::FindText(name.to_string()),
    );
    if type_name == "File" && looks_like_path(name) {
        item(
            ui,
            out,
            "Open file location",
            "",
            UiCmd::OpenLocation(name.to_string()),
        );
    }
    item(ui, out, "Copy name", "", UiCmd::Copy(name.to_string()));
}

pub fn handle_pane(
    ui: &mut Ui,
    p: &ProcessInfo,
    report: &HandleReport,
    index: usize,
    snap: &Snapshot,
    panes: &Panes,
    out: &mut Vec<UiCmd>,
) {
    let e = &report.entries[index];
    let type_name = report.type_name(e.type_index);
    let name = report.names.get(index).cloned().flatten();
    let number = crate::details::handles::value_text(e.value);
    title(
        ui,
        if cfg!(windows) {
            format!("Handle {number}")
        } else {
            format!("File descriptor {number}")
        },
        Some(&type_name),
    );
    facts(ui, ("handle-facts", e.value), |ui| {
        match &name {
            Some(n) => fact(ui, "Name", (**n).to_string()),
            None => fact(
                ui,
                "Name",
                RichText::new("not read: Resolve names reads it").color(theme::TEXT_DIM),
            ),
        }
        if cfg!(windows) {
            fact(
                ui,
                "Access",
                format!(
                    "0x{:08X}  ·  {}",
                    e.access,
                    access::rights(&type_name, e.access).join(", ")
                ),
            );
            let attributes = access::attributes(e.attributes);
            fact(
                ui,
                "Attributes",
                if attributes.is_empty() {
                    "none".to_string()
                } else {
                    attributes.join(", ")
                },
            );
        } else {
            fact(
                ui,
                "Open for",
                crate::details::handles::access_text(e.access),
            );
        }
        if e.object != 0 {
            fact(
                ui,
                if cfg!(windows) { "Object" } else { "File id" },
                RichText::new(format!("0x{:X}", e.object)).monospace(),
            );
        }
    });

    section(ui, "Also held by");
    if e.object == 0 {
        weak(ui, "Windows shows object addresses to administrators only.");
    } else {
        let key = p.key;
        let known = panes
            .holders
            .as_ref()
            .filter(|(k, o, _)| *k == key && *o == e.object)
            .map(|(_, _, h)| h);
        if known.is_none() && panes.holders_pending != Some((key, e.object)) {
            out.push(UiCmd::Holders {
                key,
                object: e.object,
            });
        }
        match known {
            None => {
                ui.spinner();
            }
            Some(Err(err)) => {
                ui.label(RichText::new(err).color(theme::ELEVATED_TEXT));
            }
            Some(Ok(holders)) if holders.is_empty() => {
                weak(ui, "No other process: the object is this one's alone.");
            }
            Some(Ok(holders)) => {
                for h in holders {
                    ui.horizontal_wrapped(|ui| {
                        link_to(ui, h.pid, &h.name, snap, out);
                        ui.label(
                            RichText::new(if cfg!(windows) {
                                format!(
                                    "{} {}  ·  {}",
                                    h.handles.len(),
                                    if h.handles.len() == 1 {
                                        "handle"
                                    } else {
                                        "handles"
                                    },
                                    access::rights(&type_name, h.access).join(", ")
                                )
                            } else {
                                format!(
                                    "open {} {}  ·  {}",
                                    h.handles.len(),
                                    if h.handles.len() == 1 {
                                        "time"
                                    } else {
                                        "times"
                                    },
                                    crate::details::handles::access_text(h.access)
                                )
                            })
                            .color(theme::TEXT_WEAK),
                        );
                    });
                }
            }
        }
    }

    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        // Linux does not let one program close another's files.
        if cfg!(windows) {
            button(
                ui,
                "Close handle\u{2026}",
                "Closes it inside the process, without it knowing",
                out,
                UiCmd::ConfirmCloseHandle(FoundHandle {
                    pid: p.pid,
                    process: p.name.clone(),
                    handle: e.value,
                    type_name: type_name.clone(),
                    name: name.as_deref().unwrap_or("this handle").to_string(),
                }),
            );
        }
        if let Some(n) = &name {
            button(
                ui,
                "Find everywhere",
                "Which processes hold a handle with this name",
                out,
                UiCmd::FindText(n.to_string()),
            );
            if &*type_name == "File" && looks_like_path(n) {
                button(
                    ui,
                    "Open location",
                    "",
                    out,
                    UiCmd::OpenLocation(n.to_string()),
                );
            }
            button(ui, "Copy name", "", out, UiCmd::Copy(n.to_string()));
        }
    });
}

// Connections ----------------------------------------------------------------

/// A connection on display, with what the pane needs around it.
pub struct ConnRow<'a> {
    pub c: &'a Connection,
    /// Its DNS name, or what kind of address it is; may be empty.
    pub name: &'a str,
    pub p: &'a ProcessInfo,
    /// The program's file, for blocking it.
    pub path: Option<&'a str>,
    pub blocked: bool,
}

/// What usually listens on a port.
pub fn port_service(proto: Proto, port: u16) -> Option<&'static str> {
    Some(match (proto, port) {
        (Proto::Udp, 443) => "QUIC (HTTP/3)",
        (_, 20 | 21) => "FTP",
        (_, 22) => "SSH",
        (_, 25 | 465 | 587) => "mail (SMTP)",
        (_, 53) => "DNS",
        (_, 67 | 68) => "DHCP",
        (_, 80) => "HTTP",
        (_, 110 | 995) => "mail (POP3)",
        (_, 123) => "time (NTP)",
        (_, 137..=139) => "Windows file sharing (NetBIOS)",
        (_, 143 | 993) => "mail (IMAP)",
        (_, 443) => "HTTPS",
        (_, 445) => "Windows file sharing (SMB)",
        (_, 1900) => "device discovery (SSDP)",
        (_, 3389) => "Remote Desktop",
        (_, 3478..=3481) => "calls and games (STUN/TURN)",
        (_, 5222 | 5223) => "push and chat (XMPP)",
        (_, 5228) => "Google push",
        (_, 5353) => "local names (mDNS)",
        (_, 5355) => "local names (LLMNR)",
        (_, 8080 | 8443) => "HTTP (alternate port)",
        (_, 27015..=27050) => "Steam",
        _ => return None,
    })
}

pub fn remote_text(c: &Connection) -> String {
    match c.remote {
        std::net::IpAddr::V6(v6) => format!("[{v6}]:{}", c.remote_port),
        v4 => format!("{v4}:{}", c.remote_port),
    }
}

/// Why a connection cannot be closed from here, or None when it can.
fn why_not_closable(c: &Connection) -> Option<&'static str> {
    match (c.proto, c.remote) {
        (Proto::Udp, _) => Some("UDP has no connection to close: it just sends packets."),
        (_, std::net::IpAddr::V6(_)) => {
            Some("Windows offers no way to close another program's IPv6 connection.")
        }
        _ => None,
    }
}

fn close_cmd(r: &ConnRow) -> Option<UiCmd> {
    match (r.c.proto, r.c.remote) {
        (Proto::Tcp, std::net::IpAddr::V4(v4)) => Some(UiCmd::CloseConnection {
            pid: r.p.pid,
            process: r.p.name.clone(),
            remote: v4,
            remote_port: r.c.remote_port,
            local_port: r.c.local_port,
        }),
        _ => None,
    }
}

fn public(c: &Connection) -> bool {
    anabolic_core::resolver::local_kind(c.remote).is_none()
}

pub fn connection_menu(ui: &mut Ui, r: &ConnRow, out: &mut Vec<UiCmd>) {
    match close_cmd(r) {
        Some(cmd) => item(ui, out, "Close connection", "", cmd),
        None => {
            ui.add_enabled(false, egui::Button::new("Close connection"))
                .on_disabled_hover_text(why_not_closable(r.c).unwrap_or(""));
        }
    }
    if let Some(path) = r.path {
        if r.blocked {
            item(
                ui,
                out,
                "Allow network access again",
                "",
                UiCmd::Unblock(path.to_string()),
            );
        } else if !crate::proc_table::system_file(path) {
            item(
                ui,
                out,
                &format!("Block {} from the network\u{2026}", r.p.name),
                "",
                UiCmd::ConfirmBlock {
                    path: path.to_string(),
                    name: r.p.name.clone(),
                },
            );
        }
    }
    ui.separator();
    if public(r.c) {
        item(
            ui,
            out,
            "Look up the address online",
            "",
            UiCmd::OpenUrl(format!("https://ipinfo.io/{}", r.c.remote)),
        );
    }
    if !r.name.is_empty() && public(r.c) {
        item(
            ui,
            out,
            "Search the name online",
            "",
            UiCmd::SearchOnline(r.name.to_string()),
        );
    }
    ui.separator();
    item(ui, out, "Copy address", "", UiCmd::Copy(remote_text(r.c)));
    if !r.name.is_empty() && public(r.c) {
        item(ui, out, "Copy name", "", UiCmd::Copy(r.name.to_string()));
    }
}

pub fn connection_pane(ui: &mut Ui, r: &ConnRow, out: &mut Vec<UiCmd>) {
    let c = r.c;
    let title_text = if r.name.is_empty() || !public(c) {
        remote_text(c)
    } else {
        r.name.to_string()
    };
    let proto = format!("{:?}", c.proto).to_uppercase();
    title(
        ui,
        title_text,
        Some(&format!("{proto} to {}", remote_text(c))),
    );
    ui.horizontal_wrapped(|ui| {
        match close_cmd(r) {
            Some(cmd) => button(
                ui,
                "Close connection",
                "The TCP stack drops it; the program sees it reset, as if the other end hung up.",
                out,
                cmd,
            ),
            None => {
                ui.add_enabled(false, egui::Button::new("Close connection"))
                    .on_disabled_hover_text(why_not_closable(c).unwrap_or(""));
            }
        }
        if public(c) {
            button(
                ui,
                "Look up",
                "Who owns the address and where it is (ipinfo.io, in the browser)",
                out,
                UiCmd::OpenUrl(format!("https://ipinfo.io/{}", c.remote)),
            );
        }
    });

    section(ui, "Connection");
    facts(ui, ("conn-facts", remote_text(c), c.local_port), |ui| {
        fact(
            ui,
            "Address",
            RichText::new(c.remote.to_string()).monospace(),
        );
        if !r.name.is_empty() {
            fact(ui, "Name", r.name);
        } else if public(c) {
            fact(
                ui,
                "Name",
                RichText::new("none (the address has no reverse DNS name)").color(theme::TEXT_DIM),
            );
        }
        let port = match port_service(c.proto, c.remote_port) {
            Some(service) => format!("{}  ·  usually {service}", c.remote_port),
            None => c.remote_port.to_string(),
        };
        fact(ui, "Port", port);
        fact(ui, "Local port", c.local_port.to_string());
        fact(ui, "Protocol", proto.as_str());
    });

    section(ui, "Traffic");
    facts(ui, ("conn-traffic", remote_text(c), c.local_port), |ui| {
        fact(
            ui,
            "Now",
            format!(
                "receive {}  ·  send {}",
                fmt::rate_always(c.rx_rate),
                fmt::rate_always(c.tx_rate)
            ),
        );
        fact(
            ui,
            "In all",
            format!(
                "{} received  ·  {} sent",
                fmt::bytes(c.rx_total),
                fmt::bytes(c.tx_total)
            ),
        );
        fact(
            ui,
            "Last packet",
            if c.idle == 0 {
                "this refresh".to_string()
            } else {
                format!("{} s ago", c.idle)
            },
        );
    });
    weak(
        ui,
        "Counted since Anabolic started watching the network; a connection that stays quiet \
         drops off the list after a while.",
    );

    if let Some(path) = r.path {
        section(ui, "The program");
        if r.blocked {
            weak(ui, "Blocked from the network by Anabolic's firewall rules.");
            button(
                ui,
                "Allow network access again",
                "",
                out,
                UiCmd::Unblock(path.to_string()),
            );
        } else if crate::proc_table::system_file(path) {
            weak(
                ui,
                "A Windows file: it cannot be blocked without cutting Windows off.",
            );
        } else {
            button(
                ui,
                &format!("Block {} from the network\u{2026}", r.p.name),
                "Two Windows Firewall rules, in and out, for every process of this file",
                out,
                UiCmd::ConfirmBlock {
                    path: path.to_string(),
                    name: r.p.name.clone(),
                },
            );
        }
    }
}
