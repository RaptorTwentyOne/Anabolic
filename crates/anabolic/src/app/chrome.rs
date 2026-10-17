//! Around the pages: keyboard shortcuts, the navigation rail, the top bar
//! with its controls and settings, the alert bar and the status bar.

use super::*;

impl App {
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<UiCmd>) {
        if self.kill_dialog.is_some() || self.affinity_dialog.is_some() || self.confirm.is_some() {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        // An open menu or list takes the arrows, Space, Enter and Escape.
        let popup_open = egui::Popup::is_any_open(ctx);
        let rows = self.table.rows.len();
        let current = self.table.selected_row(&self.snapshot);
        let current_row = current.map(|r| self.table.rows[r]);
        let group_id = current_row
            .and_then(|r| r.group)
            .map(|g| self.table.groups[g as usize].id.clone());
        let mut jump: Option<isize> = None;
        let mut cycle_view = false;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::F) {
                self.focus_search = true;
                self.settings.tab = Tab::Processes;
            }
            if i.consume_key(Modifiers::COMMAND, Key::T) {
                cycle_view = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::H) {
                out.push(UiCmd::OpenFind);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num1) {
                self.settings.tab = Tab::Processes;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num2) {
                self.settings.tab = Tab::Performance;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num3) {
                self.settings.tab = Tab::History;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num4) {
                self.settings.tab = Tab::Rules;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num5) {
                self.settings.tab = Tab::Services;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num6) {
                self.settings.tab = Tab::Startup;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num7) {
                self.settings.tab = Tab::Stutters;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num8) {
                self.settings.tab = Tab::Network;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num9) {
                self.settings.tab = Tab::Activity;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Num0) {
                self.settings.tab = Tab::Mcp;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Comma) {
                self.settings.tab = Tab::Settings;
            }
            if i.consume_key(Modifiers::NONE, Key::F5) {
                self.collector.send(Command::RefreshNow);
            }
            if i.consume_key(Modifiers::NONE, Key::F1) {
                self.help_open = !self.help_open;
            }
            if typing || popup_open {
                return;
            }
            // The Pause button promises Space on every page.
            if i.consume_key(Modifiers::NONE, Key::Space) {
                self.paused = !self.paused;
            }
            if self.settings.tab != Tab::Processes {
                return;
            }
            // Shift+F10 opens the selected row's menu, as in Explorer.
            if (group_id.is_some() || self.table.selected.is_some())
                && i.consume_key(Modifiers::SHIFT, Key::F10)
            {
                self.table.open_menu.set(true);
                self.table.scroll_to_selected = true;
            }
            if let Some(id) = &group_id {
                if i.consume_key(Modifiers::NONE, Key::Delete) {
                    out.push(UiCmd::KillGroup(id.clone()));
                }
                let expanded = current_row.is_some_and(|r| r.expanded);
                if (i.consume_key(Modifiers::NONE, Key::ArrowLeft) && expanded)
                    || (i.consume_key(Modifiers::NONE, Key::ArrowRight) && !expanded)
                {
                    out.push(UiCmd::ToggleGroup(id.clone()));
                }
            } else if let Some(key) = self.table.selected {
                if i.consume_key(Modifiers::SHIFT, Key::Delete) {
                    out.push(UiCmd::Kill { key, tree: true });
                } else if i.consume_key(Modifiers::NONE, Key::Delete) {
                    out.push(UiCmd::Kill { key, tree: false });
                }
                if self.settings.view == View::Tree
                    && let Some(row) = current_row
                {
                    if i.consume_key(Modifiers::NONE, Key::ArrowLeft)
                        && row.has_children
                        && row.expanded
                    {
                        out.push(UiCmd::ToggleCollapse(key));
                    }
                    if i.consume_key(Modifiers::NONE, Key::ArrowRight)
                        && row.has_children
                        && !row.expanded
                    {
                        out.push(UiCmd::ToggleCollapse(key));
                    }
                }
            }
            for (key, delta) in [
                (Key::ArrowUp, -1),
                (Key::ArrowDown, 1),
                (Key::PageUp, -25),
                (Key::PageDown, 25),
                (Key::Home, isize::MIN / 2),
                (Key::End, isize::MAX / 2),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    jump = Some(delta);
                }
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                out.push(UiCmd::Select(None));
            }
        });
        if cycle_view {
            self.set_view(self.settings.view.next());
        }
        if let Some(delta) = jump
            && rows > 0
        {
            let from = current.map_or(if delta > 0 { -1 } else { rows as isize }, |r| r as isize);
            let to = from.saturating_add(delta).clamp(0, rows as isize - 1) as usize;
            let row = self.table.rows[to];
            out.push(match row.group {
                Some(g) => UiCmd::SelectGroup(self.table.groups[g as usize].id.clone()),
                None => UiCmd::Select(Some(self.snapshot.processes[row.idx as usize].key)),
            });
        }
    }

    /// Pages on the left, like Windows 11's Task Manager; tools and settings
    /// below them. Collapses to icons.
    pub(super) fn nav_rail(&mut self, ui: &mut egui::Ui) {
        let collapsed = self.settings.nav_collapsed;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let toggle = widgets::rail_icon(ui, theme::icon::MENU, "Navigation").on_hover_text(
                if collapsed {
                    "Show labels"
                } else {
                    "Icons only"
                },
            );
            if toggle.clicked() {
                self.settings.nav_collapsed = !collapsed;
            }
            if !collapsed {
                ui.add(egui::Image::new(&self.mark).fit_to_exact_size(egui::vec2(20.0, 20.0)));
                ui.label(
                    RichText::new("ANABOLIC")
                        .font(theme::brand(theme::BODY))
                        .extra_letter_spacing(1.2)
                        .color(theme::ACCENT),
                );
            }
        });
        ui.add_space(10.0);
        let pages = [
            (
                Tab::Processes,
                theme::icon::PROCESSES,
                "Processes",
                "Ctrl+1",
            ),
            (
                Tab::Performance,
                theme::icon::PERFORMANCE,
                "Performance",
                "Ctrl+2",
            ),
            (Tab::History, theme::icon::HISTORY, "History", "Ctrl+3"),
            (Tab::Rules, theme::icon::RULES, "Rules", "Ctrl+4"),
            (Tab::Services, theme::icon::SERVICES, "Services", "Ctrl+5"),
            (Tab::Startup, theme::icon::STARTUP, "Startup", "Ctrl+6"),
            (Tab::Stutters, theme::icon::STUTTERS, "Stutters", "Ctrl+7"),
            (Tab::Network, theme::icon::NETWORK, "Network", "Ctrl+8"),
            (Tab::Activity, theme::icon::RECORD, "Activity", "Ctrl+9"),
            (Tab::Mcp, theme::icon::MCP, "MCP", "Ctrl+0"),
        ];
        for (tab, icon, label, keys) in pages {
            let selected = self.settings.tab == tab;
            if widgets::rail_item(ui, icon, label, selected, collapsed)
                .on_hover_text(format!("{label} ({keys})"))
                .clicked()
            {
                self.settings.tab = tab;
            }
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        // A tool, not a page: it opens a window over the current page.
        if widgets::rail_item(ui, theme::icon::SEARCH, "Find handle", false, collapsed)
            .on_hover_text("Who has this file, key or object open? (Ctrl+H)")
            .clicked()
        {
            self.open_find();
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(8.0);
            let selected = self.settings.tab == Tab::Settings;
            if widgets::rail_item(ui, theme::icon::SETTINGS, "Settings", selected, collapsed)
                .on_hover_text("Settings (Ctrl+,)")
                .clicked()
            {
                self.settings.tab = Tab::Settings;
            }
        });
    }

    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // The filter and views belong to the process list.
            if self.settings.tab == Tab::Processes {
                self.list_controls(ui);
            }
            self.global_controls(ui);
        });
    }

    pub(super) fn list_controls(&mut self, ui: &mut egui::Ui) {
        let search = ui.add(
            TextEdit::singleline(&mut self.table.filter)
                .hint_text("Filter by name, PID, path or user  (Ctrl+F)")
                .desired_width(300.0),
        );
        if std::mem::take(&mut self.focus_search) {
            self.settings.tab = Tab::Processes;
            search.request_focus();
        }
        if search.changed() {
            self.table.dirty = true;
        }
        if search.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.table.filter.clear();
            self.table.dirty = true;
            search.surrender_focus();
        }
        ui.add_space(4.0);
        for view in [View::Tree, View::List, View::Apps] {
            let tip = match view {
                View::Tree => "Parent/child tree",
                View::List => "Every process, sorted globally",
                View::Apps => "Grouped by executable with totals, like Task Manager",
            };
            if ui
                .add(egui::Button::selectable(
                    self.settings.view == view,
                    view.label(),
                ))
                .on_hover_text(format!("{tip} (Ctrl+T cycles)"))
                .clicked()
            {
                self.set_view(view);
            }
        }
    }

    /// Elevation, refresh rate, pause: right-aligned on every page.
    pub(super) fn global_controls(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if self.elevated {
                    ui.label(RichText::new("Administrator").size(theme::META).color(theme::TEXT_WEAK))
                        .on_hover_text("Running elevated: every process, disk counters and owners are visible.");
                    ui.label(widgets::icon(theme::icon::SHIELD, 13.0).color(theme::GOOD));
                    ui.add_space(6.0);
                } else if ui
                    .add_enabled(
                        !self.relaunching,
                        egui::Button::new(RichText::new("Restart as admin").color(theme::ELEVATED_TEXT)),
                    )
                    .on_hover_text("Without elevation protected processes, disk counters and some owners are hidden.")
                    .on_disabled_hover_text("Waiting for the UAC prompt.")
                    .clicked()
                {
                    self.relaunching = true;
                    self.actions.run(Action::RelaunchElevated);
                }

                let label = INTERVALS.iter().find(|(ms, _)| *ms == self.settings.interval_ms).map_or("custom", |(_, l)| *l);
                egui::ComboBox::from_id_salt("interval").selected_text(label).width(70.0).show_ui(ui, |ui| {
                    for (ms, label) in INTERVALS {
                        if ui.selectable_value(&mut self.settings.interval_ms, ms, label).changed() {
                            self.collector.send(Command::SetInterval(Duration::from_millis(ms)));
                        }
                    }
                });
                let (glyph, label) = if self.paused { (theme::icon::PLAY, "Resume") } else { (theme::icon::PAUSE, "Pause") };
                let button = egui::Button::new(widgets::icon_text(ui, glyph, label)).selected(self.paused);
                if ui.add(button).on_hover_text("Freeze the view (Space)").clicked() {
                    self.paused = !self.paused;
                }
                if self.paused {
                    ui.label(widgets::strong("Paused", theme::META).color(theme::ACCENT));
                }
            });
    }

    pub(super) fn alert_bar(&mut self, ui: &mut egui::Ui, out: &mut Vec<UiCmd>) {
        let Some(alert) = &self.alert else { return };
        let mut dismiss = false;
        let mut show_apps = false;
        let white = theme::TEXT;
        // Buttons first, anchored right, so they stay put while the text changes.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Hide until memory drops")
                .clicked()
            {
                dismiss = true;
            }
            if ui.small_button("Show apps by memory").clicked() {
                show_apps = true;
            }
            if let Some((key, _, _)) = &alert.process {
                if ui.small_button("Terminate…").clicked() {
                    out.push(UiCmd::Kill {
                        key: *key,
                        tree: false,
                    });
                }
                if ui.small_button("Show").clicked() {
                    out.push(UiCmd::GoTo(*key));
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(widgets::icon(theme::icon::WARNING, 13.0).color(theme::ELEVATED_TEXT));
                widgets::text(
                    ui,
                    130.0,
                    Align::Min,
                    widgets::strong(
                        format!("{} {:.0}% full", alert.what, alert.pct),
                        theme::BODY,
                    )
                    .color(white),
                );
                let mut message = String::new();
                if let Some((name, bytes, count)) = &alert.app {
                    let procs = if *count == 1 {
                        "1 process".to_string()
                    } else {
                        format!("{count} processes")
                    };
                    message.push_str(&format!(
                        "Biggest app: {name}, {} in {procs}",
                        fmt::bytes(*bytes)
                    ));
                }
                if let Some((key, name, bytes)) = &alert.process {
                    message.push_str(&format!(
                        "   ·   largest process: {name} ({}) {}",
                        key.pid,
                        fmt::bytes(*bytes)
                    ));
                }
                ui.add(egui::Label::new(RichText::new(message).color(white)).truncate());
            });
        });
        if dismiss {
            self.alert_dismissed = true;
        }
        if show_apps {
            let sort = if alert.what == "RAM" {
                Col::PrivateWs
            } else {
                Col::Private
            };
            self.settings.tab = Tab::Processes;
            self.set_view(View::Apps);
            self.settings.sort = sort;
            self.settings.descending = true;
            if !self.settings.columns.contains(&sort) {
                self.settings
                    .columns
                    .insert(3.min(self.settings.columns.len()), sort);
            }
            self.table.scroll_to_selected = false;
        }
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui, out: &mut Vec<UiCmd>) {
        let snap = &self.snapshot;
        let s = &snap.system;
        let m = &s.memory;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Label weak, value strong; fixed widths, the numbers change every refresh.
            let pair = |ui: &mut egui::Ui, width: f32, label: &str, value: String| {
                widgets::cell(ui, width, Align::Min, |ui| {
                    ui.label(
                        RichText::new(label)
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                    );
                    ui.label(RichText::new(value).size(theme::META));
                });
            };
            pair(ui, 96.0, "Processes", fmt::count(s.processes as u64));
            pair(ui, 96.0, "Threads", fmt::count(s.threads as u64));
            pair(ui, 120.0, "Handles", fmt::count(s.handles as u64));
            pair(ui, 74.0, "CPU", format!("{:.0} %", s.cpu.total));
            pair(
                ui,
                170.0,
                "Memory",
                format!("{} / {}", fmt::bytes(m.used()), fmt::bytes(m.total)),
            );
            pair(
                ui,
                160.0,
                "Commit",
                format!("{} / {}", fmt::bytes(m.commit), fmt::bytes(m.commit_limit)),
            );
            if snap.info_level == InfoLevel::Full {
                pair(
                    ui,
                    200.0,
                    "Disk",
                    format!(
                        "R {}  W {}",
                        fmt::rate_always(s.disk_read_rate),
                        fmt::rate_always(s.disk_write_rate)
                    ),
                );
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let own = snap
                    .processes
                    .iter()
                    .find(|p| p.pid == snap.own_pid && p.state != ProcState::Exited);
                let text = format!(
                    "scan {}  ·  ui {}  ·  self {:.1} %  ·  {}",
                    fmt::millis(snap.collect_time),
                    fmt::millis(self.ui_time),
                    own.map_or(0.0, |p| p.cpu),
                    // Like Task Manager's Memory: what is Anabolic's own. The
                    // GPU drivers' code it maps is shared with every program
                    // that draws, and comes and goes by hundreds of MB.
                    fmt::bytes(own.map_or(0, |p| p.private_working_set))
                );
                ui.add_space(6.0);
                widgets::cell(ui, 290.0, Align::Max, |ui| {
                    ui.label(
                        RichText::new(text)
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                    )
                    .on_hover_ui(|ui| self.hardening_report(ui));
                    ui.label(widgets::icon(theme::icon::SPEED, 12.0).color(theme::TEXT_WEAK));
                });
                // Leaks: a mark that says how many, and who on hover.
                let leaks = self.growth.leaks();
                if !leaks.is_empty() {
                    let mut who: Vec<ProcKey> = Vec::new();
                    for l in leaks {
                        if !who.contains(&l.key) {
                            who.push(l.key);
                        }
                    }
                    let text = format!(
                        "{} growing",
                        if who.len() == 1 {
                            "1 process".to_string()
                        } else {
                            format!("{} processes", who.len())
                        }
                    );
                    let response = ui
                        .add(
                            egui::Button::new(
                                RichText::new(text)
                                    .size(theme::META)
                                    .color(theme::ELEVATED_TEXT),
                            )
                            .frame(false),
                        )
                        .on_hover_ui(|ui| {
                            ui.label(widgets::strong("Growing steadily", theme::BODY));
                            for l in leaks.iter().take(8) {
                                ui.label(format!("{} ({}): {}", l.name, l.key.pid, fmt::leak(l)));
                            }
                            ui.label(
                                RichText::new("Click to show the first one.")
                                    .size(theme::META)
                                    .color(theme::TEXT_WEAK),
                            );
                        });
                    if response.clicked() {
                        out.push(UiCmd::GoTo(who[0]));
                    }
                    ui.label(widgets::icon(theme::icon::WARNING, 12.0).color(theme::ELEVATED_TEXT));
                }
            });
        });
    }

    pub(super) fn hardening_report(&self, ui: &mut egui::Ui) {
        ui.label(widgets::strong(
            "Staying responsive under load",
            theme::BODY,
        ));
        ui.label(
            RichText::new(format!("Information level: {:?}", self.snapshot.info_level))
                .color(theme::TEXT_WEAK),
        );
        if let Some(renderer) = &self.renderer {
            ui.label(RichText::new(format!("Drawn by {renderer}")).color(theme::TEXT_WEAK));
        }
        let mark = |ui: &mut egui::Ui, ok: bool| {
            let (glyph, color) = if ok {
                (theme::icon::CHECK, theme::GOOD)
            } else {
                (theme::icon::CLOSE, theme::BAD)
            };
            ui.label(widgets::icon(glyph, 11.0).color(color));
        };
        for item in &self.hardening {
            ui.horizontal(|ui| {
                mark(ui, item.result.is_ok());
                ui.label(item.what);
                if let Err(e) = &item.result {
                    ui.label(
                        RichText::new(e)
                            .size(theme::CAPTION)
                            .color(theme::TEXT_WEAK),
                    );
                }
            });
        }
        ui.horizontal(|ui| {
            mark(ui, self.stalls.count == 0);
            if self.stalls.count == 0 {
                ui.label("No UI stalls this session");
            } else {
                ui.label(format!(
                    "{} UI stalls, longest {:.0} ms (stalls.log)",
                    self.stalls.count,
                    self.stalls.longest * 1000.0
                ));
            }
        });
        ui.horizontal(|ui| match &self.pinned {
            Some(Ok(bytes)) => {
                mark(ui, true);
                ui.label(format!("Working set pinned at {}", fmt::bytes(*bytes)));
            }
            Some(Err(e)) => {
                mark(ui, false);
                ui.label(format!("Working set pin: {e}"));
            }
            None => {
                ui.label(RichText::new("·").color(theme::TEXT_WEAK));
                ui.label("Working set pin pending");
            }
        });
    }
}
