// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What the pages ask for: selecting, ending processes, and every UiCmd
//! they send, carried out once per frame.

use super::*;

impl App {
    pub(super) fn target(&self, key: ProcKey) -> Option<Target> {
        self.snapshot.find(key).map(|p| Target {
            key,
            name: p.name.clone(),
        })
    }

    pub(super) fn select(&mut self, key: Option<ProcKey>) {
        self.table.selected_group = None;
        if self.table.selected != key {
            self.table.selected = key;
            self.focus_thread = None;
            self.collector.send(Command::Select(key));
        }
    }

    pub(super) fn select_group(&mut self, id: Arc<str>) {
        self.select(None);
        self.table.selected_group = Some(id);
    }

    /// Switches to the process list, selects `key` and makes sure its row is
    /// visible (expanding parents/groups, dropping a filter that hides it).
    pub(super) fn go_to(&mut self, key: ProcKey) {
        self.jumped();
        self.settings.tab = Tab::Processes;
        self.select(Some(key));
        match self.settings.view {
            View::Tree => {
                let procs = &self.snapshot.processes;
                let parents = tree::parents(procs);
                if let Some(mut i) = procs.iter().position(|p| p.key == key) {
                    let mut guard = 0;
                    while let Some(parent) = parents[i]
                        && guard < 256
                    {
                        self.table.collapsed.remove(&procs[parent as usize].key);
                        i = parent as usize;
                        guard += 1;
                    }
                }
            }
            View::Apps => {
                if let Some(p) = self.snapshot.find(key) {
                    self.table
                        .expanded_groups
                        .insert(Arc::from(p.name.to_ascii_lowercase()));
                }
            }
            View::List => {}
        }
        self.table
            .rebuild(&self.snapshot, &self.details, &self.settings);
        if self.table.selected_row(&self.snapshot).is_none() && self.table.filtered() {
            self.table.unfilter();
            self.table
                .rebuild(&self.snapshot, &self.details, &self.settings);
        }
        self.table.scroll_to_selected = true;
    }

    /// The process and everything below it, children first.
    pub(super) fn tree_targets(&self, root: ProcKey) -> Vec<Target> {
        let procs = &self.snapshot.processes;
        let parents = tree::parents(procs);
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, parent) in parents.iter().enumerate() {
            if let Some(p) = parent {
                children.entry(*p as usize).or_default().push(i);
            }
        }
        let Some(start) = procs.iter().position(|p| p.key == root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![(start, false)];
        while let Some((i, visited)) = stack.pop() {
            if visited {
                let p = &procs[i];
                if p.state != ProcState::Exited && !p.is_pseudo() {
                    out.push(Target {
                        key: p.key,
                        name: p.name.clone(),
                    });
                }
                continue;
            }
            stack.push((i, true));
            if out.len() + stack.len() < 10_000 {
                stack.extend(children.get(&i).into_iter().flatten().map(|&c| (c, false)));
            }
        }
        out
    }

    pub(super) fn confirm_kill(
        &mut self,
        title: String,
        subject: String,
        detail: Option<String>,
        targets: Vec<Target>,
    ) {
        if targets.is_empty() {
            return;
        }
        let critical: Vec<String> = targets
            .iter()
            .filter(|t| self.details.get(&t.key).is_some_and(|d| d.critical))
            .map(|t| t.name.to_string())
            .collect();
        if critical.is_empty() && !self.settings.confirm_kill {
            self.actions.run(Action::Terminate(targets));
        } else {
            self.kill_dialog = Some(KillDialog {
                title,
                subject,
                detail,
                targets,
                critical,
                dont_ask: false,
            });
        }
    }

    pub(super) fn request_kill(&mut self, key: ProcKey, tree: bool) {
        let Some(root) = self.target(key) else { return };
        if self.snapshot.find(key).is_some_and(|p| p.is_pseudo()) {
            return;
        }
        let subject = format!("{} (PID {})", root.name, key.pid);
        if tree {
            let targets = self.tree_targets(key);
            let detail = (targets.len() > 1)
                .then(|| format!("and {} descendant process(es)", targets.len() - 1));
            self.confirm_kill("Terminate process tree?".into(), subject, detail, targets);
        } else {
            self.confirm_kill("Terminate process?".into(), subject, None, vec![root]);
        }
    }

    pub(super) fn request_kill_group(&mut self, id: &str) {
        let Some(group) = self.table.group(id) else {
            return;
        };
        let targets: Vec<Target> = group
            .members
            .iter()
            .map(|&i| &self.snapshot.processes[i as usize])
            .filter(|p| p.state != ProcState::Exited && !p.is_pseudo())
            .map(|p| Target {
                key: p.key,
                name: p.name.clone(),
            })
            .collect();
        let title = format!("Terminate all {} processes?", targets.len());
        let subject = group.agg.name.to_string();
        let detail = Some(format!(
            "{} of memory in use",
            fmt::bytes(group.agg.private_working_set)
        ));
        self.confirm_kill(title, subject, detail, targets);
    }

    pub(super) fn handle(&mut self, cmds: Vec<UiCmd>) {
        for cmd in cmds {
            match cmd {
                UiCmd::Select(key) => {
                    self.select(key);
                    self.table.scroll_to_selected = true;
                }
                UiCmd::SelectGroup(id) => {
                    self.select_group(id);
                    self.table.scroll_to_selected = true;
                }
                UiCmd::GoTo(key) => self.go_to(key),
                UiCmd::GoToPid(pid) => {
                    if let Some(key) = self
                        .snapshot
                        .processes
                        .iter()
                        .find(|p| p.pid == pid && p.state != ProcState::Exited)
                        .map(|p| p.key)
                    {
                        self.go_to(key);
                    }
                }
                UiCmd::Service { name, display, op } => {
                    self.services.busy.insert(name.clone());
                    self.service_watch.control(name, display, op);
                }
                UiCmd::ConfirmBlock { path, name } => {
                    let dialog = ConfirmDialog {
                        title: format!("Block {name} from the network?"),
                        lines: vec![
                            format!(
                                "Windows Firewall gets two rules, in and out, that block {path}: \
                                 every process started from this file, now and later. Its open \
                                 connections stop working."
                            ),
                            "Allow it again from the same menu, from the Rules page, or in \
                             Windows' firewall settings (rules named \"Anabolic: block …\")."
                                .into(),
                        ],
                        confirm: "Block".into(),
                    };
                    self.confirm = Some((dialog, Pending::Block(path)));
                }
                UiCmd::Unblock(path) => self.actions.run(Action::Firewall { path, block: false }),
                UiCmd::CloseConnection {
                    pid,
                    process,
                    remote,
                    remote_port,
                    local_port,
                } => self.actions.run(Action::CloseConnection {
                    pid,
                    process,
                    remote,
                    remote_port,
                    local_port,
                }),
                UiCmd::ShowNetwork => self.settings.tab = Tab::Network,
                UiCmd::LiveActivity(key) => {
                    use crate::activity_page::Watch;
                    if self.snapshot.find(key).is_some() {
                        // The tree view shows a process with the ones under
                        // it; so does its live activity.
                        let watch = if self.settings.view == crate::settings::View::Tree {
                            Watch::Tree(key)
                        } else {
                            Watch::Process(key)
                        };
                        self.activity.watch(watch, &self.snapshot);
                        self.settings.tab = Tab::Activity;
                    }
                }
                UiCmd::LiveActivityGroup(id) => {
                    if let Some(g) = self.table.group(&id) {
                        let name = g.agg.name.clone();
                        self.activity
                            .watch(crate::activity_page::Watch::Name(name), &self.snapshot);
                        self.settings.tab = Tab::Activity;
                    }
                }
                UiCmd::RelaunchElevated => {
                    self.relaunching = true;
                    self.actions.run(Action::RelaunchElevated);
                }
                UiCmd::SetStartup(item, on) => {
                    self.startup.busy = true;
                    self.startup_watch.set_enabled(*item, on);
                }
                UiCmd::SelectService(name) => {
                    self.jumped();
                    self.settings.tab = Tab::Services;
                    self.services.selected = Some(name);
                }
                UiCmd::ShowServices(pid) => {
                    self.jumped();
                    self.settings.tab = Tab::Services;
                    self.services.show_pid(pid);
                }
                UiCmd::GoToThread { key, tid } => {
                    self.go_to(key);
                    self.detail_tab = DetailTab::Threads;
                    self.settings.show_details = true;
                    self.focus_thread = Some((tid, false));
                }
                UiCmd::ToggleCollapse(key) => {
                    if !self.table.collapsed.remove(&key) {
                        self.table.collapsed.insert(key);
                    }
                    self.table.dirty = true;
                }
                UiCmd::ToggleGroup(id) => {
                    if !self.table.expanded_groups.remove(&id) {
                        self.table.expanded_groups.insert(id);
                    }
                    self.table.dirty = true;
                }
                UiCmd::Kill { key, tree } => self.request_kill(key, tree),
                UiCmd::KillGroup(id) => self.request_kill_group(&id),
                UiCmd::Suspend(key) => self.run_on(key, Action::Suspend),
                UiCmd::Resume(key) => self.run_on(key, Action::Resume),
                UiCmd::SetPriority(key, priority) => {
                    self.run_on(key, |t| Action::SetPriority(t, priority))
                }
                UiCmd::Efficiency(key, on) => {
                    self.run_on(key, |t| Action::SetEfficiencyMode(t, on))
                }
                UiCmd::AffinityDialog(key) => self.run_on(key, Action::QueryAffinity),
                UiCmd::OpenLocation(path) => self.actions.run(Action::OpenFileLocation(path)),
                UiCmd::Properties(path) => self.actions.run(Action::FileProperties(path)),
                UiCmd::SearchOnline(query) => self.actions.run(Action::SearchOnline(query)),
                UiCmd::Copy(text) => self.actions.run(Action::Copy(text)),
                UiCmd::OpenUrl(url) => self.actions.run(Action::OpenUrl(url)),
                UiCmd::CloseToTray(on) => self.settings.close_to_tray = on,
                UiCmd::FindText(text) => {
                    self.find.open = true;
                    self.find.query = text.clone();
                    self.find.pending = true;
                    self.inspector.request(InspectRequest::FindHandles {
                        query: text,
                        exclude: Vec::new(),
                    });
                }
                UiCmd::Thread { key, tid, op } => self.actions.run(Action::Thread {
                    pid: key.pid,
                    tid,
                    op,
                }),
                UiCmd::FileFacts(path) => {
                    self.panes.facts_pending = Some(path.clone());
                    self.inspector.request(InspectRequest::FileFacts { path });
                }
                UiCmd::Holders { key, object } => {
                    self.panes.holders_pending = Some((key, object));
                    self.inspector
                        .request(InspectRequest::Holders { key, object });
                }
                UiCmd::ConfirmStack { key, tid } => {
                    if self.panes.stack_ok.contains(&key) {
                        self.read_stack(key, tid);
                        continue;
                    }
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: "Read the call stack?".into(),
                        lines: vec![
                            format!(
                                "Anabolic suspends thread {tid} of {} (PID {}) for a few milliseconds and reads \
                                 its stack. The process is opened for reading; nothing is written into it. \
                                 Other threads of it are not asked again this session.",
                                p.name, p.pid
                            ),
                            "Protected programs such as anti-cheat or security software may refuse this access \
                             or treat it as suspicious."
                                .into(),
                        ],
                        confirm: "Read stack".into(),
                    };
                    self.confirm = Some((dialog, Pending::Stack(key, tid)));
                }
                UiCmd::ConfirmEndThread { key, tid } => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: format!("End thread {tid}?"),
                        lines: vec![
                            format!(
                                "Ending one thread of {} (PID {}) from outside can leave it broken: locks the \
                                 thread held stay held, data it was writing stays half-written.",
                                p.name, p.pid
                            ),
                            "Use it on a runaway thread when ending the whole program is worse; expect to \
                             restart the program afterwards."
                                .into(),
                        ],
                        confirm: "End thread".into(),
                    };
                    self.confirm = Some((dialog, Pending::EndThread(key, tid)));
                }
                UiCmd::ConfirmThreadWaitChain { key, tid } => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: "Analyze the wait chain?".into(),
                        lines: vec![
                            if cfg!(windows) {
                                format!(
                                    "Anabolic will ask Windows what thread {tid} of {} (PID {}) is waiting for, and \
                                     follow the chain into other processes; the process' other threads come after \
                                     it. Windows opens the threads for reading; nothing is injected or suspended.",
                                    p.name, p.pid
                                )
                            } else {
                                format!(
                                    "Anabolic will read what thread {tid} of {} (PID {}) is waiting for from the \
                                     kernel, and follow the chain into other processes (a file lock, a pipe); the \
                                     process' other threads come after it. Nothing is injected or stopped.",
                                    p.name, p.pid
                                )
                            },
                            "Protected programs such as anti-cheat or security software may refuse this access \
                             or treat it as suspicious."
                                .into(),
                        ],
                        confirm: "Analyze".into(),
                    };
                    self.confirm = Some((dialog, Pending::WaitChainFrom(key, tid)));
                }
                UiCmd::Sort(col) => {
                    if self.settings.sort == col {
                        self.settings.descending = !self.settings.descending;
                    } else {
                        self.settings.sort = col;
                        self.settings.descending = col.default_descending();
                    }
                    self.table.dirty = true;
                }
                UiCmd::ToggleColumn(col) => {
                    if let Some(i) = self.settings.columns.iter().position(|&c| c == col) {
                        self.settings.columns.remove(i);
                    } else {
                        // Keep the canonical column order.
                        let rank = |c: &Col| Col::ALL.iter().position(|x| x == c);
                        let at = self
                            .settings
                            .columns
                            .iter()
                            .position(|c| rank(c) > rank(&col))
                            .unwrap_or(self.settings.columns.len());
                        self.settings.columns.insert(at, col);
                    }
                }
                UiCmd::Columns(columns) => self.settings.columns = columns,
                UiCmd::ResetColumns => self.settings.columns = Settings::default().columns,
                UiCmd::InspectHandles { key, names } => self.inspect_handles(key, names),
                UiCmd::ConfirmResolveNames(key) => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: "Resolve handle names?".into(),
                        lines: vec![
                            if cfg!(windows) {
                                format!(
                                    "Anabolic will open {} (PID {}) and duplicate each of its {} handles to read \
                                     their names: file paths, registry keys, object names, and the processes \n                                      and \
                                     threads they point to.",
                                    p.name,
                                    p.pid,
                                    fmt::count(p.handles as u64)
                                )
                            } else {
                                format!(
                                    "Anabolic will read what each of the {} files {} (PID {}) has open is: its \
                                     path, or the connection, pipe or kernel object behind it. The process is not \
                                     opened; the kernel tells.",
                                    fmt::count(p.handles as u64),
                                    p.name,
                                    p.pid
                                )
                            },
                            "With many handles this takes a few seconds.".into(),
                            "Protected programs such as anti-cheat or security software may refuse this access \
                             or treat it as suspicious."
                                .into(),
                        ],
                        confirm: "Resolve names".into(),
                    };
                    self.confirm = Some((dialog, Pending::ResolveNames(key)));
                }
                UiCmd::ConfirmLoadModules(key) => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: "Read the module list?".into(),
                        lines: vec![
                            format!(
                                "Anabolic will open {} (PID {}) for reading to list the DLLs it has loaded and to \
                                 name its threads' start addresses.",
                                p.name, p.pid
                            ),
                            "Protected programs such as anti-cheat or security software may refuse this access \
                             or treat it as suspicious."
                                .into(),
                        ],
                        confirm: "Read modules".into(),
                    };
                    self.confirm = Some((dialog, Pending::LoadModules(key)));
                }
                UiCmd::LoadModules(key) => self.load_modules(key),
                UiCmd::ConfirmDump(key, kind) => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let what = match kind {
                        anabolic_core::dump::DumpKind::Full => format!(
                            "A full dump holds all of its memory: about {} of private memory \
                             plus the code it has loaded.",
                            fmt::bytes(p.private_bytes)
                        ),
                        anabolic_core::dump::DumpKind::Small => {
                            "A small dump holds its threads with their stacks, its modules and \
                             handles: enough to see where it is stuck, usually a few MB."
                                .to_string()
                        }
                    };
                    let dir = crate::data_dir()
                        .map(|d| d.join("dumps").display().to_string())
                        .unwrap_or_default();
                    let mut lines = vec![
                        what,
                        format!(
                            "Anabolic opens {} (PID {}) to read its memory and copies it first, \
                             so it pauses only for a moment. The file goes to {dir}.",
                            p.name, p.pid
                        ),
                        "Reading another program's memory is what anti-cheat software guards \
                         against: do not dump a protected game, or the anti-cheat itself, while \
                         you play."
                            .into(),
                    ];
                    if anabolic_core::dump::holds_secrets(&p.name) {
                        lines.push(if cfg!(windows) {
                            "lsass holds the sign-in secrets of this PC: the file will hold \
                             password hashes and tickets of everyone signed in, and your \
                             antivirus will likely stop the dump and may quarantine Anabolic. \
                             Keep the file safe and delete it when done."
                                .into()
                        } else {
                            "This keyring holds the session's passwords and keys, unlocked: the \
                             file will hold them in the clear. Keep it safe and delete it when \
                             done."
                                .into()
                        });
                    }
                    let dialog = ConfirmDialog {
                        title: format!("Write a {} dump of {}?", kind.label(), p.name),
                        lines,
                        confirm: "Write dump".into(),
                    };
                    self.confirm = Some((dialog, Pending::Dump(key, kind)));
                }
                UiCmd::NewRule {
                    image,
                    base_priority,
                } => {
                    let priority = anabolic_core::actions::Priority::from_base(base_priority)
                        .and_then(crate::rules::RulePriority::from_core);
                    self.rules_page.new_rule(&image, priority);
                    self.settings.tab = Tab::Rules;
                }
                UiCmd::WhyAlive(key) => {
                    self.end_view.pending = Some(key);
                    self.inspector.request(InspectRequest::WhyAlive { key });
                }
                UiCmd::ConfirmCloseHandles {
                    pid,
                    process,
                    handles,
                    what,
                } => {
                    let dialog = ConfirmDialog {
                        title: format!("Close {} handles in {process}?", handles.len()),
                        lines: vec![
                            format!(
                                "{process} (PID {pid}) holds {} handles to {what}. They are closed inside it \
                                 without it knowing.",
                                handles.len()
                            ),
                            "It may crash or misbehave if it uses them later. A game's anti-cheat may treat \
                             this as tampering."
                                .into(),
                        ],
                        confirm: "Close handles".into(),
                    };
                    self.confirm = Some((
                        dialog,
                        Pending::CloseHandles {
                            pid,
                            process,
                            handles,
                            what,
                        },
                    ));
                }
                UiCmd::StopWaitChain => {
                    self.inspector.request(InspectRequest::StopWaitChain);
                    self.wait_view.pending = None;
                    self.wait_view.progress = None;
                }
                UiCmd::ConfirmWaitChain(key) => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: "Analyze the wait chain?".into(),
                        lines: vec![
                            if cfg!(windows) {
                                format!(
                                    "Anabolic will ask Windows what each thread of {} (PID {}) is waiting for, \
                                     and follow the chain into other processes. Windows opens the threads for \
                                     reading; nothing is injected or suspended.",
                                    p.name, p.pid
                                )
                            } else {
                                format!(
                                    "Anabolic will read what each thread of {} (PID {}) is waiting for from the \
                                     kernel, and follow the chain into other processes (a file lock, a pipe). \
                                     Nothing is injected or stopped.",
                                    p.name, p.pid
                                )
                            },
                            if cfg!(windows) {
                                format!(
                                    "Windows spends about 60 ms of CPU on each waiting thread. {} has {} threads, so \
                                     this can take up to {} s, on four threads at low priority. Threads that are \
                                     running or idle are left out.",
                                    p.name,
                                    p.threads,
                                    (p.threads as f32 * 0.06 / 4.0).ceil().max(1.0)
                                )
                            } else {
                                format!(
                                    "{} has {} threads; reading them takes a moment. Threads that are running \
                                     are left out.",
                                    p.name, p.threads
                                )
                            },
                            "Protected programs such as anti-cheat or security software may refuse this access \
                             or treat it as suspicious."
                                .into(),
                        ],
                        confirm: "Analyze".into(),
                    };
                    self.confirm = Some((dialog, Pending::WaitChain(key)));
                }
                UiCmd::OpenFind => self.open_find(),
                UiCmd::FindHandles { query, exclude } => {
                    self.find.pending = true;
                    self.inspector
                        .request(InspectRequest::FindHandles { query, exclude });
                }
                UiCmd::ConfirmCloseHandle(found) => {
                    let dialog = ConfirmDialog {
                        title: "Close this handle?".into(),
                        lines: vec![
                            format!("{} (PID {}) holds {}", found.process, found.pid, found.name),
                            "The handle is closed inside that process without it knowing. It may crash, lose \
                             data or misbehave; use it to free a stuck file, not routinely."
                                .into(),
                        ],
                        confirm: "Close handle".into(),
                    };
                    self.confirm = Some((dialog, Pending::CloseHandle(found)));
                }
                UiCmd::FindZombies => {
                    self.zombie_view.pending = true;
                    self.inspector.request(InspectRequest::Zombies);
                }
                UiCmd::ShowTab(tab) => {
                    self.jumped();
                    self.settings.tab = tab;
                }
                UiCmd::ShowPerf(view) => {
                    self.jumped();
                    self.settings.tab = Tab::Performance;
                    self.perf_view = view;
                }
                UiCmd::ShowApp(name) => self.show_app(&name),
                UiCmd::ShowCopies { name, starter } => self.show_copies(&name, starter),
                UiCmd::ShowNew => {
                    self.jumped();
                    self.settings.tab = Tab::Processes;
                    self.table.unfilter();
                    self.table.quick.toggle(crate::tree::Quick::New);
                    self.set_view(View::List);
                }
                UiCmd::AppsByMemory(ram) => self.apps_by_memory(ram),
                UiCmd::KillMany {
                    title,
                    subject,
                    keys,
                } => {
                    let targets = keys.into_iter().filter_map(|k| self.target(k)).collect();
                    self.confirm_kill(title, subject, None, targets);
                }
                UiCmd::IgnoreRunaway(id) => {
                    if !self.settings.runaway_ignored.contains(&id.to_string()) {
                        self.settings.runaway_ignored.push(id.to_string());
                    }
                    if let Some(r) = self.runaways.iter().find(|r| r.id == id) {
                        self.toasts.info(format!(
                            "No more warnings about {} running in many copies. Settings > \
                             Warnings can bring them back.",
                            r.name
                        ));
                    }
                    self.runaways.retain(|r| r.id != id);
                }
                UiCmd::RunTask => {
                    if self.run_dialog.is_none() {
                        self.run_dialog =
                            Some(crate::dialogs::RunDialog::new(self.settings.run_elevated));
                    }
                }
                UiCmd::NotifyExit(key, on) => self.notify_exit(key, on),
                UiCmd::ShowListening => {
                    self.jumped();
                    self.settings.tab = Tab::Network;
                    self.network.tab = crate::network_page::NetTab::Listening;
                }
                UiCmd::ShowDevPorts => {
                    self.jumped();
                    self.settings.tab = Tab::Network;
                    self.network.tab = crate::network_page::NetTab::Development;
                }
                UiCmd::WatchPort(watch) => {
                    let ports = &mut self.settings.watched_ports;
                    ports.retain(|w| w.port != watch.port);
                    if watch.port != 0 {
                        ports.push(watch);
                        ports.sort_by_key(|w| w.port);
                    }
                }
                UiCmd::UnwatchPort(port) => self.settings.watched_ports.retain(|w| w.port != port),
                UiCmd::FreePort { port, pid } => self.free_port(port, pid),
            }
        }
        if self.table.dirty {
            self.table
                .rebuild(&self.snapshot, &self.details, &self.settings);
        }
    }

    /// Every process of a program, in the Apps view: its group selected, or
    /// the process when it runs alone.
    pub(super) fn show_app(&mut self, name: &str) {
        let id: Arc<str> = name.to_ascii_lowercase().into();
        self.jumped();
        self.settings.tab = Tab::Processes;
        self.set_view(View::Apps);
        self.table.unfilter();
        self.table.only_app = Some(id.clone());
        self.table
            .rebuild(&self.snapshot, &self.details, &self.settings);
        if self.table.group(&id).is_some() {
            self.select_group(id);
        } else if let Some(key) = self
            .snapshot
            .processes
            .iter()
            .find(|p| p.state != ProcState::Exited && p.name.eq_ignore_ascii_case(name))
            .map(|p| p.key)
        {
            self.select(Some(key));
        }
        self.table.scroll_to_selected = true;
    }

    /// A program's copies in the tree, under the program that started them
    /// (selected), and nothing else.
    pub(super) fn show_copies(&mut self, name: &str, starter: Option<ProcKey>) {
        self.jumped();
        self.settings.tab = Tab::Processes;
        self.set_view(View::Tree);
        self.table.unfilter();
        self.table.only_app = Some(name.to_ascii_lowercase().into());
        if let Some(key) = starter {
            self.table.collapsed.remove(&key);
        }
        self.table
            .rebuild(&self.snapshot, &self.details, &self.settings);
        let first = || {
            self.snapshot
                .processes
                .iter()
                .find(|p| p.state != ProcState::Exited && p.name.eq_ignore_ascii_case(name))
                .map(|p| p.key)
        };
        let key = starter.or_else(first);
        self.select(key);
        self.table.scroll_to_selected = true;
    }

    /// The Apps view by memory, biggest first: RAM (private working set)
    /// or commit (private bytes), with its column shown.
    pub(super) fn apps_by_memory(&mut self, ram: bool) {
        let sort = if ram { Col::PrivateWs } else { Col::Private };
        self.jumped();
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

    /// Ends what holds a port, after asking; a service or Windows itself is
    /// pointed at instead.
    pub(super) fn free_port(&mut self, port: u16, pid: u32) {
        let Some(p) = self
            .snapshot
            .processes
            .iter()
            .find(|p| p.pid == pid && p.state != ProcState::Exited)
        else {
            self.toasts
                .info(format!("Port {port} is free: what held it has exited"));
            return;
        };
        if crate::platform::system_pid(pid) {
            self.toasts.error_with(
                format!("{} itself holds port {port}", crate::platform::SYSTEM),
                Some(if cfg!(windows) {
                    "A driver or Windows' web server (HTTP.sys) has it for a service or a program; \
                     the Services page may name which."
                        .into()
                } else {
                    "The kernel has it for a service; the Services page may name which.".into()
                }),
            );
            return;
        }
        let services = self.services.hosted(pid);
        if !services.is_empty() {
            let names: Vec<&str> = services.iter().map(|(_, d)| &**d).collect();
            self.toasts.info_with(
                format!(
                    "Port {port} is held by {} (service: {}). Stop the service to free it.",
                    p.name,
                    names.join(", ")
                ),
                "Show services",
                UiCmd::ShowServices(pid),
            );
            return;
        }
        let Some(target) = self.target(p.key) else {
            return;
        };
        let subject = format!("{} (PID {pid}) holds port {port}", p.name);
        self.confirm_kill(
            format!("Free port {port}?"),
            subject,
            Some("Ending it frees the port.".into()),
            vec![target],
        );
    }

    pub(super) fn run_on(&self, key: ProcKey, make: impl FnOnce(Target) -> Action) {
        if let Some(t) = self.target(key) {
            self.actions.run(make(t));
        }
    }

    pub(super) fn set_view(&mut self, view: View) {
        self.settings.view = view;
        if view != View::Apps {
            self.table.selected_group = None;
        }
        self.table.dirty = true;
    }
}
