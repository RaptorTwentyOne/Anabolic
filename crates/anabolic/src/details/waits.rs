//! The Wait chain tab: what each thread waits for, across processes,
//! deadlocks first.

use super::*;

pub(super) fn waits_tab(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    view: &WaitView,
    out: &mut Vec<UiCmd>,
) {
    if p.threads == 0 && !p.is_pseudo() {
        ui.label(
            RichText::new(
                "It has no threads: nothing in it can wait. General says why it is still here.",
            )
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let hung: Vec<_> = snap.system.hung.iter().filter(|h| h.pid == p.pid).collect();
    let report = view.report.as_deref().filter(|r| r.key == p.key);
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if hung.is_empty() {
                ui.label(RichText::new("None of its windows is hung right now.").color(theme::TEXT_WEAK));
            }
            for h in &hung {
                ui.horizontal(|ui| {
                    ui.label(widgets::icon(theme::icon::WARNING, 13.0).color(theme::BAD));
                    ui.label(widgets::strong(
                        format!("\u{201c}{}\u{201d} is not responding", h.title),
                        theme::BODY,
                    ));
                    ui.label(
                        RichText::new(format!("its thread {} has taken no message for 5 s", h.tid))
                            .color(theme::TEXT_WEAK),
                    );
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if view.pending == Some(p.key) {
                    ui.spinner();
                    let mut text = "Following the wait chains\u{2026}".to_string();
                    if let Some((done, total)) = view.progress {
                        text.push_str(&format!("  {done} of {total} threads"));
                    }
                    if let Some(started) = view.started {
                        text.push_str(&format!("  \u{b7}  {} s", started.elapsed().as_secs()));
                    }
                    ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                    if ui
                        .small_button("Stop")
                        .on_hover_text("Stop the analysis; nothing is shown for it.")
                        .clicked()
                    {
                        out.push(UiCmd::StopWaitChain);
                    }
                    // The seconds count up.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(500));
                } else if view.pending.is_some() {
                    ui.label(
                        RichText::new("Another process\u{2019} wait chain is being analysed.")
                            .color(theme::TEXT_WEAK),
                    );
                    if ui.small_button("Stop it").clicked() {
                        out.push(UiCmd::StopWaitChain);
                    }
                } else if ui
                    .button(if report.is_some() { "Analyze again" } else { "Analyze wait chain" })
                    .on_hover_text(
                        "Asks Windows what each of its threads is waiting for, across processes \
                         (opens its threads for reading).",
                    )
                    .clicked()
                {
                    out.push(UiCmd::ConfirmWaitChain(p.key));
                }
                if let Some(r) = report
                    && view.pending != Some(p.key)
                {
                    let mut text = format!(
                        "{} threads, {} waiting on something",
                        r.threads,
                        r.chains.iter().filter(|c| c.links.len() > 1).count(),
                    );
                    if r.idle > 0 {
                        text.push_str(&format!(
                            " \u{b7} {} running or idle, not followed",
                            r.idle
                        ));
                    }
                    if r.failed > 0 {
                        text.push_str(&format!(", {} could not be analysed", r.failed));
                    }
                    text.push_str(&format!(" \u{b7} {}", fmt::millis(r.took)));
                    ui.label(RichText::new(text).size(theme::META).color(theme::TEXT_WEAK));
                }
            });
            if let Some((key, e)) = &view.error
                && *key == p.key
            {
                ui.label(
                    RichText::new(format!("The wait chain could not be read: {e}"))
                        .color(theme::ELEVATED_TEXT),
                );
            }
            let Some(r) = report else {
                return;
            };
            if r.skipped > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} threads were skipped: their chain led into something that did not answer \
                         within 2.5 s, or the 20 s limit ran out. Analyze again to retry them.",
                        r.skipped
                    ))
                    .color(theme::ELEVATED_TEXT),
                );
            }
            ui.add_space(8.0);
            if r.chains.is_empty() {
                ui.label(
                    RichText::new(
                        "No thread is waiting on anything Windows can follow: \
                         if it is slow, it is busy, not stuck.",
                    )
                    .color(theme::TEXT_WEAK),
                );
            }
            let hung_tids: Vec<u32> = hung.iter().map(|h| h.tid).collect();
            for (i, chain) in r.chains.iter().enumerate() {
                if i > 0 {
                    ui.add_space(4.0);
                    ui.separator();
                }
                chain_view(ui, chain, p.pid, &hung_tids, snap, out);
            }
        });
}

pub(super) fn chain_view(
    ui: &mut Ui,
    chain: &Chain,
    own: u32,
    hung_tids: &[u32],
    snap: &Snapshot,
    out: &mut Vec<UiCmd>,
) {
    if chain.cycle {
        ui.label(
            widgets::strong("Deadlock: these threads wait on each other", theme::BODY)
                .color(theme::BAD),
        );
    }
    for (depth, link) in chain.links.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 16.0);
            if depth > 0 {
                ui.label(RichText::new("\u{2192}").color(theme::TEXT_DIM));
            }
            match link {
                Link::Thread {
                    pid, tid, status, ..
                } => {
                    let label = if depth == 0 {
                        "Thread"
                    } else {
                        "held by thread"
                    };
                    ui.label(format!("{label} {tid} of"));
                    process_link(ui, *pid, own, snap, out);
                    let color = match status {
                        Status::Blocked => theme::ELEVATED_TEXT,
                        Status::Running => theme::GOOD,
                        _ => theme::TEXT_WEAK,
                    };
                    ui.label(RichText::new(status.label()).color(color));
                    if depth == 0 && hung_tids.contains(tid) {
                        ui.label(
                            RichText::new("\u{b7} the hung window's thread")
                                .size(theme::META)
                                .color(theme::BAD),
                        );
                    }
                    if depth == 0 && *status == Status::Running {
                        ui.label(
                            RichText::new("\u{b7} running, not waiting: busy, not stuck")
                                .size(theme::META)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                }
                Link::Object { wait, name, status } => {
                    ui.label(format!("waits on {}", wait.label()));
                    if !name.is_empty() {
                        ui.label(RichText::new(name).monospace().color(theme::TEXT_WEAK));
                    }
                    if !matches!(status, Status::Blocked | Status::Owned | Status::Unknown) {
                        ui.label(RichText::new(status.label()).color(theme::TEXT_WEAK));
                    }
                }
            }
        });
    }
}
