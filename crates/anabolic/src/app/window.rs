//! The window itself: the notification area, where the window opens,
//! staying on top, the Find window and the questions asked before acting.

use super::*;

impl App {
    /// The notification area icon: there while it is wanted, its clicks,
    /// its tooltip, and what a closed window should not keep running.
    pub(super) fn tray_logic(&mut self, ctx: &egui::Context) {
        let want = self.settings.close_to_tray || self.start_in_tray;
        if !want {
            self.tray_failed = false;
            if !self.hidden {
                self.tray = None;
            }
        } else if self.tray.is_none() && !self.tray_failed {
            self.tray = crate::tray::Tray::start(ctx.clone());
            self.tray_failed = self.tray.is_none();
        }
        let Some(tray) = &self.tray else {
            // Nothing could bring a hidden window back: show it.
            if self.hidden {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                self.hidden = false;
            }
            return;
        };
        let events: Vec<crate::tray::TrayEvent> = tray.events().collect();
        // The memory alert starting while nobody looks at the window.
        let alert = self.alert.as_ref().map(|a| {
            let who = match &a.app {
                Some((name, bytes, count)) if *count > 1 => {
                    format!("{name} uses {} in {count} processes.", fmt::bytes(*bytes))
                }
                Some((name, bytes, _)) => format!("{name} uses {}.", fmt::bytes(*bytes)),
                None => "Open Anabolic to see who uses it.".into(),
            };
            (format!("{} at {:.0} %", a.what, a.pct), who)
        });
        if self.hidden
            && !self.alert_seen
            && let Some((title, text)) = &alert
        {
            tray.notify(title, text, true);
        }
        self.alert_seen = alert.is_some();
        if self.tip_at.elapsed() >= Duration::from_secs(2) {
            self.tip_at = Instant::now();
            let s = &self.snapshot.system;
            let memory = s.memory.used() as f64 * 100.0 / s.memory.total.max(1) as f64;
            tray.tip(&format!(
                "Anabolic  ·  CPU {:.0} %  ·  memory {memory:.0} %{}",
                s.cpu.total,
                if self.settings.record_history {
                    "  ·  recording"
                } else {
                    ""
                }
            ));
        }
        for event in events {
            match event {
                crate::tray::TrayEvent::Open => self.hidden = false,
                crate::tray::TrayEvent::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    ctx.request_repaint();
                }
            }
        }
    }

    /// Holds the hotkey chosen in Settings, and lets it go or swaps it when
    /// the choice changes. Pressed, it brings the window back as a second
    /// start would, from the notification area too.
    pub(super) fn hotkey_logic(&mut self, ctx: &egui::Context) {
        let want = if self.serves_hotkey {
            self.settings.hotkey
        } else {
            crate::settings::Hotkey::Off
        };
        if self.hotkey_for != Some(want) {
            self.hotkey = None;
            self.hotkey_error = None;
            self.hotkey = want.keys().map(|(modifiers, key)| {
                let ctx = ctx.clone();
                anabolic_core::hotkey::Hotkey::register(modifiers, key, move || {
                    crate::come_forward(&ctx)
                })
            });
            self.hotkey_for = Some(want);
        }
        if let Some(Err(e)) = self.hotkey.as_ref().and_then(|h| h.answer()) {
            self.hotkey_error = Some(e);
            self.hotkey = None;
        }
    }

    /// Keeps the window off a full-screen game's monitor at startup, and on
    /// a monitor that exists (checked every couple of seconds).
    pub(super) fn place_window(&mut self) {
        if self.avoid_monitor.is_none() && self.placed_at.elapsed() < Duration::from_secs(2) {
            return;
        }
        let Some(hwnd) = anabolic_core::instance::own_window() else {
            return;
        };
        if let Some(game) = self.avoid_monitor.take() {
            anabolic_core::placement::avoid(hwnd, game);
        }
        self.placed_at = Instant::now();
        anabolic_core::placement::rescue(hwnd);
    }

    pub(super) fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
    }

    pub(super) fn find_window(&mut self, ctx: &egui::Context, out: &mut Vec<UiCmd>) {
        if !self.find.open {
            return;
        }
        let mut open = true;
        let snapshot = self.snapshot.clone();
        egui::Window::new(widgets::strong("Find handle", theme::BODY))
            .open(&mut open)
            .collapsible(false)
            .default_size([1000.0, 520.0])
            .show(ctx, |ui| {
                let find = &mut self.find;
                ui.horizontal(|ui| {
                    ui.label("Name contains");
                    let query = ui.add(TextEdit::singleline(&mut find.query).hint_text("file, key or object name").desired_width(340.0));
                    if std::mem::take(&mut find.focus) {
                        query.request_focus();
                    }
                    ui.label("Skip");
                    ui.add(TextEdit::singleline(&mut find.exclude).hint_text("e.g. game.exe, avp.exe").desired_width(260.0));
                    let go = query.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if find.pending {
                        ui.spinner();
                    } else if (ui.add_enabled(!find.query.trim().is_empty(), egui::Button::new("Search")).clicked() || go)
                        && !find.query.trim().is_empty()
                    {
                        let exclude = find.exclude.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        out.push(UiCmd::FindHandles { query: find.query.trim().to_string(), exclude });
                    }
                });
                ui.label(
                    RichText::new(
                        "Searching duplicates handles out of every process to read their names. Protected programs \
                         (anti-cheat, security software) may object; list them under Skip.",
                    )
                    .small()
                    .color(theme::TEXT_WEAK),
                );
                let Some(report) = &find.report else { return };
                let skipped: Vec<String> = report.skipped.iter().map(|(n, pid, why)| format!("{n} ({pid}): {why}")).collect();
                ui.label(
                    RichText::new(format!(
                        "{} matches for “{}”  ·  {} processes, {} named handles searched  ·  {}  ·  {} skipped",
                        report.matches.len(),
                        report.query,
                        report.processes,
                        fmt::count(report.handles as u64),
                        fmt::millis(report.took),
                        report.skipped.len()
                    ))
                    .small(),
                )
                .on_hover_text(skipped.join("\n"));
                let matches = &report.matches;
                egui_extras::TableBuilder::new(ui)
                    .id_salt("find-results")
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .auto_shrink([false, false])
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(egui_extras::Column::initial(170.0).clip(true))
                    .column(egui_extras::Column::initial(60.0))
                    .column(egui_extras::Column::initial(90.0))
                    .column(egui_extras::Column::initial(80.0))
                    .column(egui_extras::Column::remainder().clip(true))
                    .header(22.0, |mut h| {
                        for t in ["Process", "PID", "Type", "Handle", "Name"] {
                            h.col(|ui| widgets::column_header(ui, t, false));
                        }
                    })
                    .body(|body| {
                        body.rows(18.0, matches.len(), |mut row| {
                            let m = &matches[row.index()];
                            let key = snapshot.processes.iter().find(|p| p.pid == m.pid && p.state != ProcState::Exited).map(|p| p.key);
                            row.col(|ui| {
                                ui.add(egui::Label::new(&*m.process).truncate());
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(m.pid.to_string()).color(theme::TEXT_WEAK));
                            });
                            row.col(|ui| {
                                ui.label(&*m.type_name);
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format!("0x{:X}", m.handle)).monospace().color(theme::TEXT_WEAK));
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(&m.name).truncate());
                            });
                            let response = row.response();
                            if response.double_clicked()
                                && let Some(key) = key
                            {
                                out.push(UiCmd::GoTo(key));
                            }
                            response.context_menu(|ui| {
                                if let Some(key) = key
                                    && ui.button("Show process").clicked()
                                {
                                    out.push(UiCmd::GoTo(key));
                                    ui.close();
                                }
                                if ui.button("Copy name").clicked() {
                                    out.push(UiCmd::Copy(m.name.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(RichText::new("Close handle…").color(theme::BAD)).clicked() {
                                    out.push(UiCmd::ConfirmCloseHandle(m.clone()));
                                    ui.close();
                                }
                            });
                        });
                    });
            });
        self.find.open = open;
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some((dialog, _)) = &self.confirm {
            match dialog.show(ctx) {
                ConfirmChoice::Pending => {}
                ConfirmChoice::Confirm => {
                    if let Some((_, pending)) = self.confirm.take() {
                        match pending {
                            Pending::ResolveNames(key) => self.inspect_handles(key, true),
                            Pending::LoadModules(key) => self.load_modules(key),
                            Pending::WaitChain(key) => self.analyze_waits(key),
                            Pending::WaitChainFrom(key, tid) => {
                                self.analyze_waits_from(key, Some(tid))
                            }
                            Pending::Stack(key, tid) => {
                                self.panes.stack_ok.insert(key);
                                self.read_stack(key, tid);
                            }
                            Pending::EndThread(key, tid) => self.actions.run(Action::Thread {
                                pid: key.pid,
                                tid,
                                op: anabolic_core::threads::ThreadOp::Terminate,
                            }),
                            Pending::CloseHandles {
                                pid,
                                process,
                                handles,
                                what,
                            } => {
                                self.actions.run(Action::CloseHandles {
                                    pid,
                                    process,
                                    handles,
                                    what,
                                });
                                // Read the holders again once that is done.
                                self.end_view.report = None;
                            }
                            Pending::CloseHandle(found) => self.actions.run(Action::CloseHandle {
                                pid: found.pid,
                                process: found.process.clone(),
                                handle: found.handle,
                                name: found.name.clone(),
                            }),
                            Pending::Block(path) => {
                                self.actions.run(Action::Firewall { path, block: true })
                            }
                            Pending::Dump(key, kind) => {
                                if let (Some(p), Some(dir)) = (
                                    self.snapshot.find(key),
                                    crate::data_dir().map(|d| d.join("dumps")),
                                ) {
                                    self.actions.run(Action::Dump {
                                        target: Target {
                                            key,
                                            name: p.name.clone(),
                                        },
                                        kind,
                                        dir,
                                    });
                                    self.toasts.info(format!(
                                        "Writing a {} dump of {}\u{2026}",
                                        kind.label(),
                                        p.name
                                    ));
                                }
                            }
                            Pending::ServiceWithDependents { name, display, op } => {
                                self.services.busy.insert(name.clone());
                                self.service_watch.control(name, display, op);
                            }
                        }
                    }
                }
                ConfirmChoice::Cancel => self.confirm = None,
            }
        }
        if let Some(dialog) = &mut self.kill_dialog {
            match dialog.show(ctx) {
                KillChoice::Pending => {}
                KillChoice::Confirm { dont_ask_again } => {
                    if dont_ask_again {
                        self.settings.confirm_kill = false;
                    }
                    let targets = std::mem::take(&mut dialog.targets);
                    self.actions.run(Action::Terminate(targets));
                    self.kill_dialog = None;
                }
                KillChoice::Cancel => self.kill_dialog = None,
            }
        }
        if let Some(dialog) = &mut self.affinity_dialog {
            match dialog.show(ctx) {
                AffinityChoice::Pending => {}
                AffinityChoice::Apply(mask) => {
                    let target = dialog.target.clone();
                    self.actions.run(Action::SetAffinity(target, mask));
                    self.affinity_dialog = None;
                }
                AffinityChoice::Cancel => self.affinity_dialog = None,
            }
        }
    }

    /// Once the UI is fully warmed up (fonts, GL, first textures), forbid
    /// Windows from trimming our working set below what we use now.
    pub(super) fn maybe_pin(&mut self) {
        if self.pinned.is_some() || self.started.elapsed() < Duration::from_secs(5) {
            return;
        }
        let ws = hardening::own_working_set() as usize;
        let target = ws + ws / 4 + 32 * 1024 * 1024;
        self.pinned = Some(hardening::pin_working_set(target).map(|_| target as u64));
    }
}
