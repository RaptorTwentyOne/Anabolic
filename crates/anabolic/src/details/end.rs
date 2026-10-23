// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The card for a process that is ending but cannot finish, or would not
//! end: who holds it, whether a driver or the system protects it (Windows),
//! whether its parent has collected it or it waits in the kernel (Linux).

use super::*;

/// Whether the card has something to explain about `p`.
pub(super) fn needs_end_card(p: &ProcessInfo, view: &EndView) -> bool {
    p.ending || (p.threads == 0 && !p.is_pseudo()) || view.refused.contains(&p.key)
}

/// Asks for a fresh report when the one shown is old; the report, if any.
fn current_report(p: &ProcessInfo, view: &EndView, out: &mut Vec<UiCmd>) -> Option<Arc<EndReport>> {
    let report = view
        .report
        .as_ref()
        .filter(|(r, _)| r.key == p.key)
        .map(|(r, at)| (r.clone(), *at));
    let fresh = report
        .as_ref()
        .is_some_and(|(_, at)| at.elapsed() < END_REFRESH);
    if !fresh && view.pending != Some(p.key) {
        out.push(UiCmd::WhyAlive(p.key));
    }
    report.map(|(r, _)| r)
}

/// Rights of Process handles, the ones worth naming.
#[cfg(windows)]
pub(super) fn process_access(access: u32) -> String {
    if access & 0x1F_FFFF == 0x1F_FFFF {
        return "full access".into();
    }
    let names: Vec<&str> = [
        (0x0001, "end"),
        (0x0002, "create threads"),
        (0x0020, "write memory"),
        (0x0010, "read memory"),
        (0x0040, "duplicate handles"),
        (0x0800, "suspend"),
        (0x0200, "set information"),
        (0x1400, "query"),
    ]
    .iter()
    .filter(|(bit, _)| access & bit != 0)
    .map(|(_, name)| *name)
    .collect();
    if names.is_empty() {
        format!("access 0x{access:X}")
    } else {
        names.join(", ")
    }
}

/// "It does not go away, and this is why", with what can be done.
#[cfg(windows)]
pub(super) fn end_card(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    view: &EndView,
    out: &mut Vec<UiCmd>,
) {
    let report = current_report(p, view, out);
    ui.ctx().request_repaint_after(END_REFRESH);

    let (title, color) = match &report {
        Some(r) if r.flags.deleting => ("Windows is ending it but cannot finish", theme::BAD),
        Some(r) if r.protection != 0 => ("Windows protects it", theme::ELEVATED_TEXT),
        Some(r) if r.can_end == Some(false) => {
            ("The right to end it is withheld", theme::ELEVATED_TEXT)
        }
        Some(r) if r.open_error.is_some() => {
            ("Windows does not let Anabolic end it", theme::ELEVATED_TEXT)
        }
        _ if p.ending => ("Windows is ending it but cannot finish", theme::BAD),
        _ if p.threads == 0 => ("It has no threads", theme::ELEVATED_TEXT),
        _ => ("It could not be ended", theme::ELEVATED_TEXT),
    };
    Frame::new()
        .fill(theme::RAISED)
        .stroke(Stroke::new(1.0, color))
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::icon(theme::icon::WARNING, 14.0).color(color));
                ui.label(widgets::strong(title, theme::BODY).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if view.pending == Some(p.key) {
                        ui.spinner();
                    } else if ui
                        .small_button("Check again")
                        .on_hover_text("Reads its state and the system handle table again.")
                        .clicked()
                    {
                        out.push(UiCmd::WhyAlive(p.key));
                    }
                });
            });
            if let Some((key, e)) = &view.error
                && *key == p.key
            {
                ui.label(
                    RichText::new(format!("Its state could not be read: {e}"))
                        .color(theme::ELEVATED_TEXT),
                );
            }
            let Some(r) = report else {
                ui.label(RichText::new("Reading its state\u{2026}").color(theme::TEXT_WEAK));
                return;
            };
            let weak = |ui: &mut Ui, text: String| {
                ui.add(Label::new(RichText::new(text).color(theme::TEXT_WEAK)).wrap());
            };
            let size = fmt::bytes(p.private_bytes);
            if r.flags.deleting && r.threads == 0 {
                weak(
                    ui,
                    format!(
                        "It has no threads left and does nothing (0 % CPU, {size}). Every new attempt to end it \
                         is answered \u{201c}the process is terminating\u{201d}: there is nothing left to end. \
                         Windows removes it once the last handle to it is closed."
                    ),
                );
            } else if r.flags.deleting {
                weak(
                    ui,
                    format!(
                        "{} threads are still unwinding inside Windows. A thread that does not finish is \
                         usually waiting for a driver to complete an I/O request (graphics, anti-cheat, \
                         antivirus, a device that went away). The Threads tab shows what they wait for.",
                        r.threads
                    ),
                );
            } else if r.threads == 0 {
                weak(
                    ui,
                    format!(
                        "It never started, or all its threads are gone: it does nothing (0 % CPU, {size}). \
                         Ending it should work."
                    ),
                );
            }
            if r.protection != 0 {
                weak(
                    ui,
                    format!(
                        "It runs as a protected process ({}): not even an administrator may end it. \
                         Windows itself ends it, or the program that owns it.",
                        anabolic_core::ending::protection_label(r.protection)
                    ),
                );
            }
            if r.can_end == Some(false) && r.protection == 0 {
                let guards: Vec<String> = r
                    .guards
                    .iter()
                    .map(|(file, product)| format!("{product} ({file})"))
                    .collect();
                weak(
                    ui,
                    format!(
                        "Windows gave Anabolic a handle to it without the right to end it: a driver strips \
                         that right from every handle opened to it.{} It can be ended once that driver lets \
                         go, usually when the program it guards closes.",
                        if guards.is_empty() {
                            String::new()
                        } else {
                            format!(" Loaded drivers that do this: {}.", guards.join(", "))
                        }
                    ),
                );
            }
            if let Some(e) = &r.open_error {
                weak(
                    ui,
                    format!(
                        "Opening it to end it was refused: {e}. If Anabolic is not running as administrator, \
                         restarting it so may help."
                    ),
                );
            }
            if !r.flags.deleting && r.can_end == Some(true) && r.protection == 0 && r.threads > 0 {
                weak(
                    ui,
                    "Windows grants the right to end it and sees no protection; the refusal may have been \
                     momentary."
                        .into(),
                );
            }
            if !r.flags.deleting
                && r.can_end != Some(false)
                && r.protection == 0
                && ui.button("End it").clicked()
            {
                out.push(UiCmd::Kill {
                    key: p.key,
                    tree: false,
                });
            }

            ui.add_space(6.0);
            if r.holders.is_empty() {
                weak(
                    ui,
                    if r.flags.deleting {
                        "No program holds a handle to it, so only Windows itself does (a driver). If it stays, \
                         only a restart removes it; it does no harm meanwhile."
                            .into()
                    } else {
                        "No other program holds a handle to it.".into()
                    },
                );
                return;
            }
            ui.label(widgets::strong("Programs holding it", theme::META).color(theme::TEXT_WEAK));
            for h in &r.holders {
                ui.horizontal(|ui| {
                    process_link(ui, h.pid, p.pid, snap, out);
                    ui.label(
                        RichText::new(format!(
                            "{} {}  \u{b7}  {}",
                            h.handles.len(),
                            if h.handles.len() == 1 { "handle" } else { "handles" },
                            process_access(h.access)
                        ))
                        .color(theme::TEXT_WEAK),
                    );
                    if ui
                        .small_button("Close them\u{2026}")
                        .on_hover_text(
                            "Closes these handles inside that program, without it knowing.",
                        )
                        .clicked()
                    {
                        out.push(UiCmd::ConfirmCloseHandles {
                            pid: h.pid,
                            process: h.name.clone(),
                            handles: h.handles.clone(),
                            what: format!("{} ({})", p.name, p.pid),
                        });
                    }
                });
            }
            if r.flags.deleting {
                weak(
                    ui,
                    "While they hold it, Windows cannot remove it. It goes away when they close these \
                     handles, at the latest when they exit."
                        .into(),
                );
            }
        });
    ui.add_space(8.0);
}

/// "It does not go away, and this is why", with what can be done: on Linux
/// a process that ended and was not collected, waits in the kernel, or is
/// not Anabolic's to end.
#[cfg(target_os = "linux")]
pub(super) fn end_card(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    view: &EndView,
    out: &mut Vec<UiCmd>,
) {
    let report = current_report(p, view, out);
    ui.ctx().request_repaint_after(END_REFRESH);
    let waiting = |r: &EndReport| r.flags.exit_status == u32::from(b'D');
    let (title, color) = match &report {
        Some(r) if r.flags.deleting => (
            "It has ended; its parent has not collected it",
            theme::ELEVATED_TEXT,
        ),
        Some(r) if r.flags.protected => ("It is the system's own", theme::ELEVATED_TEXT),
        Some(r) if waiting(r) => ("It waits inside the kernel", theme::BAD),
        Some(r) if r.can_end == Some(false) => ("Anabolic may not end it", theme::ELEVATED_TEXT),
        _ if p.ending => (
            "It has ended; its parent has not collected it",
            theme::ELEVATED_TEXT,
        ),
        _ => ("It could not be ended", theme::ELEVATED_TEXT),
    };
    Frame::new()
        .fill(theme::RAISED)
        .stroke(Stroke::new(1.0, color))
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::icon(theme::icon::WARNING, 14.0).color(color));
                ui.label(widgets::strong(title, theme::BODY).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if view.pending == Some(p.key) {
                        ui.spinner();
                    } else if ui
                        .small_button("Check again")
                        .on_hover_text("Reads its state again.")
                        .clicked()
                    {
                        out.push(UiCmd::WhyAlive(p.key));
                    }
                });
            });
            if let Some((key, e)) = &view.error
                && *key == p.key
            {
                ui.label(
                    RichText::new(format!("Its state could not be read: {e}"))
                        .color(theme::ELEVATED_TEXT),
                );
            }
            let Some(r) = report else {
                ui.label(RichText::new("Reading its state\u{2026}").color(theme::TEXT_WEAK));
                return;
            };
            let weak = |ui: &mut Ui, text: String| {
                ui.add(Label::new(RichText::new(text).color(theme::TEXT_WEAK)).wrap());
            };
            if r.flags.deleting {
                weak(
                    ui,
                    "It has ended and does nothing: all that is left is its PID and exit status, \
                     kept until its parent collects them. That parent has not, so it stays \
                     listed; it goes away when the parent collects it, or when the parent \
                     exits (then init does). It costs nothing meanwhile."
                        .into(),
                );
                for h in &r.holders {
                    let parent = snap
                        .processes
                        .iter()
                        .find(|q| q.pid == h.pid && q.state != ProcState::Exited);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Its parent").color(theme::TEXT_WEAK));
                        process_link(ui, h.pid, p.pid, snap, out);
                        if let Some(parent) = parent
                            && h.pid > 1
                            && ui
                                .small_button("End the parent\u{2026}")
                                .on_hover_text(
                                    "Ending the parent hands this one to init, which collects it \
                                     at once.",
                                )
                                .clicked()
                        {
                            out.push(UiCmd::Kill {
                                key: parent.key,
                                tree: false,
                            });
                        }
                    });
                }
                return;
            }
            if r.flags.protected {
                weak(
                    ui,
                    if p.pid == 1 {
                        "It is init, the first process: everything else runs under it, and the \
                         system ends with it."
                            .into()
                    } else {
                        "It is one of the kernel's own threads: only the kernel starts and ends \
                         them."
                            .into()
                    },
                );
                return;
            }
            if waiting(&r) {
                let at = r
                    .guards
                    .first()
                    .map_or(String::new(), |(wchan, _)| format!(" (in {wchan})"));
                weak(
                    ui,
                    format!(
                        "It waits inside the kernel{at} and cannot be interrupted, not even by \
                         SIGKILL: the signal takes effect the moment that wait ends. A network file \
                         system that stopped answering, a device that went away or a driver that \
                         hangs is the usual cause. The Threads tab shows where each thread waits."
                    ),
                );
            }
            if r.can_end == Some(false) {
                weak(
                    ui,
                    "It belongs to another user: ending it needs the right to signal any process \
                     (cap_kill), which Anabolic has once it is given its rights."
                        .into(),
                );
            }
            if let Some(e) = &r.open_error {
                weak(ui, format!("Asking whether it may be ended failed: {e}."));
            }
            if r.can_end == Some(true) && !waiting(&r) {
                weak(
                    ui,
                    "Linux lets Anabolic signal it and it is not stuck in the kernel; the \
                     refusal may have been momentary."
                        .into(),
                );
                if ui.button("End it").clicked() {
                    out.push(UiCmd::Kill {
                        key: p.key,
                        tree: false,
                    });
                }
            }
        });
    ui.add_space(8.0);
}
