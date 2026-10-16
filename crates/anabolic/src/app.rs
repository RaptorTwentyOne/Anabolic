//! Top-level application state and layout.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anabolic_core::actions::{Action, ActionRunner, Outcome, Target};
use anabolic_core::handles::{FindReport, FoundHandle, InspectRequest, Inspection, Inspector};
use anabolic_core::hardening::{self, HardeningItem};
use anabolic_core::recorder::RecorderConfig;
use anabolic_core::services::{ServiceEvent, ServiceOp};
use anabolic_core::startup::StartupEvent;
use anabolic_core::{
    Collector, CollectorConfig, Command, InfoLevel, ProcKey, ProcState, ProcessInfo, Snapshot,
};
use crossbeam_channel::Receiver;
use eframe::egui::{
    self, Align, Color32, Frame, Key, Layout, Margin, Modifiers, RichText, TextEdit,
};

use crate::details::{self, DetailTab, HandleView, ModuleView};
use crate::dialogs::{
    AffinityChoice, AffinityDialog, ConfirmChoice, ConfirmDialog, KillChoice, KillDialog, Toasts,
};
use crate::fmt;
use crate::icons::IconCache;
use crate::perf::{self, PerfView, ZombieView};
use crate::proc_table::{self, ProcTable, UiCmd};
use crate::settings::{Col, GraphSpan, Settings, Tab, View};
use crate::theme;
use crate::tree::{self, Details};
use crate::widgets;

/// The Ctrl+Shift+Esc launcher, built by build.rs and carried inside anabolic.exe.
const LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/anabolic-launch.exe"));

const INTERVALS: [(u64, &str); 6] = [
    (250, "0.25 s"),
    (500, "0.5 s"),
    (1000, "1 s"),
    (2000, "2 s"),
    (5000, "5 s"),
    (10000, "10 s"),
];

/// What a confirmation dialog is guarding.
enum Pending {
    ResolveNames(ProcKey),
    LoadModules(ProcKey),
    WaitChain(ProcKey),
    CloseHandle(FoundHandle),
    CloseHandles {
        pid: u32,
        process: Arc<str>,
        handles: Vec<u64>,
        what: String,
    },
    EndThread(ProcKey, u32),
    Stack(ProcKey, u32),
    WaitChainFrom(ProcKey, u32),
    Dump(ProcKey, anabolic_core::dump::DumpKind),
    Block(String),
    /// Stop (or restart) a service and the services depending on it.
    ServiceWithDependents {
        name: Arc<str>,
        display: Arc<str>,
        op: anabolic_core::services::ServiceOp,
    },
}

/// What to do about a failure, from what Windows said; None when there is
/// nothing useful to add.
fn next_step(message: &str, elevated: bool) -> Option<String> {
    let m = message.to_lowercase();
    let says = |what: &str| m.contains(what);
    let step = if says("access is denied") || says("required privilege is not held") {
        if elevated {
            "Windows or a security program protects it; its details (General) say what Anabolic can tell."
        } else {
            "Restart Anabolic as administrator (top right) and try again."
        }
    } else if says("parameter is incorrect") || says("has exited") || says("no longer") {
        "It has probably exited already; the list catches up on the next refresh."
    } else if says("being used by another process") || says("clipboard") {
        "Another program is holding it; try again in a moment."
    } else if says("cannot find the file") || says("cannot find the path") {
        "The file has been moved or deleted since it was loaded."
    } else if says("not supported") {
        "This Windows version does not offer it."
    } else {
        return None;
    };
    Some(step.to_string())
}

/// The "find handles" window.
#[derive(Default)]
struct FindView {
    open: bool,
    /// Put the cursor in the query field on the next frame.
    focus: bool,
    query: String,
    exclude: String,
    pending: bool,
    report: Option<Arc<FindReport>>,
}

/// RAM or commit is nearly exhausted; who is responsible.
struct MemoryAlert {
    what: &'static str,
    pct: f64,
    /// Biggest executable: name, bytes, process count.
    app: Option<(Arc<str>, u64, usize)>,
    /// Biggest single process.
    process: Option<(ProcKey, Arc<str>, u64)>,
}

pub struct App {
    collector: Collector,
    actions: ActionRunner,
    outcomes: Receiver<Outcome>,
    inspector: Inspector,
    inspections: Receiver<Inspection>,
    snapshot: Arc<Snapshot>,
    details: Details,
    icons: IconCache,
    table: ProcTable,
    settings: Settings,
    paused: bool,
    detail_tab: DetailTab,
    /// A thread the CPU view asked to show, and whether it was scrolled to.
    focus_thread: Option<(u32, bool)>,
    handle_view: HandleView,
    perf_view: PerfView,
    history: crate::history_page::HistoryPage,
    wait_view: crate::details::WaitView,
    end_view: crate::details::EndView,
    panes: crate::panes::Panes,
    tables: crate::table::DetailTables,
    rules_page: crate::rules::RulesPage,
    rule_engine: crate::rules::Engine,
    services: crate::services_page::ServicesPage,
    service_watch: anabolic_core::services::ServiceWatch,
    service_events: Receiver<anabolic_core::services::ServiceEvent>,
    /// When the service list was last asked for.
    services_at: Option<Instant>,
    startup: crate::startup_page::StartupPage,
    startup_watch: anabolic_core::startup::StartupWatch,
    startup_events: Receiver<StartupEvent>,
    /// When the startup entries were last read.
    startup_at: Option<Instant>,
    stutters: crate::stutters_page::StuttersPage,
    /// Processes whose handles, memory or threads keep climbing.
    growth: anabolic_core::growth::Growth,
    game_mode: crate::gamemode::GameMode,
    /// Frame and DPC tracing are on.
    stutters_traced: bool,
    network: crate::network_page::NetworkPage,
    /// Every connection is being published (the Network page is open).
    network_watched: bool,
    /// Logical processor shown in detail on the CPU view.
    perf_core: Option<usize>,
    /// Context switch tracing is on (only while the CPU view is visible).
    cores_traced: bool,
    disks_traced: bool,
    hardware: anabolic_core::hwinfo::HardwareInfo,
    zombie_view: ZombieView,
    kill_dialog: Option<KillDialog>,
    affinity_dialog: Option<AffinityDialog>,
    /// A question waiting for an answer, and what to do on yes.
    confirm: Option<(ConfirmDialog, Pending)>,
    module_view: ModuleView,
    find: FindView,
    toasts: Toasts,
    alert: Option<MemoryAlert>,
    alert_dismissed: bool,
    elevated: bool,
    hardening: Vec<HardeningItem>,
    cpu_name: String,
    started: Instant,
    ui_time: Duration,
    stalls: crate::stalls::Stalls,
    /// The logo mark beside the wordmark in the rail.
    mark: egui::TextureHandle,
    /// The title bar has been painted to match (tries until the window exists).
    title_bar_styled: bool,
    pinned: Option<Result<u64, String>>,
    focus_search: bool,
    /// What Ctrl+Shift+Esc starts; None until the action thread has read it.
    taskmgr: Option<anabolic_core::taskmgr::Registration>,
    /// This build's anabolic.exe, to compare with the registered one.
    this_app: Option<String>,
    /// "Restart as admin" is waiting on the UAC prompt.
    relaunching: bool,
    /// `--pid N` on the command line: select it once it shows up.
    pending_pid: Option<u32>,
    /// `--waits`: analyze the wait chain of `--pid` once it shows up (the
    /// person who typed the flag asked for it; no dialog).
    pending_waits: bool,
    /// `--pane [NAME]`: once `--pid` is selected, open the side pane of the
    /// detail tab on its first thread, the module NAME, or the first handle
    /// of type NAME. `--stack` also reads that thread's call stack. Both
    /// skip the dialogs: the person who typed them asked.
    pending_pane: Option<String>,
    pending_stack: bool,
    /// The shortcuts and colours page (F1).
    help_open: bool,
    /// The notification area icon, while closing hides the window.
    tray: Option<crate::tray::Tray>,
    /// There is no notification area (no Explorer on this desktop).
    tray_failed: bool,
    /// `--tray`: started hidden, so the icon is needed this session.
    start_in_tray: bool,
    /// The window is closed into the notification area.
    hidden: bool,
    /// Quit was chosen: let the close through.
    quitting: bool,
    /// The first hide this session explained itself.
    told_hidden: bool,
    /// The memory alert was active at the last check (notify on its start).
    alert_seen: bool,
    tip_at: Instant,
    /// Started with command line overrides: do not persist settings.
    session_only: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        hardening: Vec<HardeningItem>,
        elevated: bool,
    ) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        settings.sanitize();
        // Command line overrides apply to this session only; with any of them
        // present nothing is saved, so the stored preferences stay intact.
        let args: Vec<String> = std::env::args().collect();
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
        };
        // Only flags that change what is shown make a session "session only".
        let session_only = args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--view"
                    | "--sort"
                    | "--pid"
                    | "--handles"
                    | "--perf"
                    | "--history"
                    | "--rule"
                    | "--core"
                    | "--alert"
                    | "--zombies"
                    | "--tab"
                    | "--find"
                    | "--filter"
                    | "--detail-sort"
                    | "--services"
                    | "--service"
                    | "--startup"
                    | "--stutters"
                    | "--network"
            )
        });
        if args.iter().any(|a| a == "--history") {
            settings.tab = Tab::History;
        }
        // `--services`, or `--service NAME` with its side pane open.
        let mut services = crate::services_page::ServicesPage::default();
        if args.iter().any(|a| a == "--services") {
            settings.tab = Tab::Services;
        }
        if let Some(name) = value("--service") {
            settings.tab = Tab::Services;
            services.selected = Some(name.into());
        }
        if args.iter().any(|a| a == "--startup") {
            settings.tab = Tab::Startup;
        }
        if args.iter().any(|a| a == "--stutters") {
            settings.tab = Tab::Stutters;
        }
        // `--network [programs|connections|listening|destinations]`.
        let mut network = crate::network_page::NetworkPage::default();
        if args.iter().any(|a| a == "--network") {
            use crate::network_page::NetTab;
            settings.tab = Tab::Network;
            network.tab = match value("--network") {
                Some("connections") => NetTab::Connections,
                Some("listening") => NetTab::Listening,
                Some("destinations") => NetTab::Destinations,
                Some("dns") => NetTab::Lookups,
                _ => NetTab::Programs,
            };
        }
        // `--rule NAME`: the rule editor, for that program.
        let mut rules_page = crate::rules::RulesPage::default();
        if let Some(image) = value("--rule") {
            rules_page.new_rule(image, None);
            settings.tab = Tab::Rules;
        }
        let mut perf_view = PerfView::default();
        if args.iter().any(|a| a == "--perf") {
            settings.tab = Tab::Performance;
            match value("--perf") {
                Some("memory") => perf_view = PerfView::Memory,
                Some("disk") => perf_view = PerfView::Disk,
                Some("io") => perf_view = PerfView::Io,
                Some("network") => perf_view = PerfView::Network,
                Some("gpu") => perf_view = PerfView::Gpu(0),
                Some("cpuinfo") => perf_view = PerfView::CpuInfo,
                Some("meminfo") => perf_view = PerfView::MemoryInfo,
                Some(v) if v.starts_with("disk") => {
                    if let Ok(n) = v[4..].parse() {
                        perf_view = PerfView::PhysicalDisk(n);
                    }
                }
                _ => {}
            }
        }
        let perf_core: Option<usize> = value("--core").and_then(|v| v.parse().ok());
        if let Some(pct) = value("--alert").and_then(|v| v.parse::<f32>().ok()) {
            settings.memory_alert_pct = pct.clamp(50.0, 99.0);
        }
        match value("--view") {
            Some("tree") => settings.view = View::Tree,
            Some("list") => settings.view = View::List,
            Some("apps") => settings.view = View::Apps,
            _ => {}
        }
        let sort = match value("--sort") {
            Some("cpu") => Some(Col::Cpu),
            Some("memory") => Some(Col::PrivateWs),
            Some("private") => Some(Col::Private),
            Some("handles") => Some(Col::Handles),
            Some("gpu") => Some(Col::Gpu),
            Some("net") => Some(Col::Net),
            Some("name") => Some(Col::Name),
            _ => None,
        };
        if let Some(col) = sort {
            settings.sort = col;
            settings.descending = col.default_descending();
        }
        if value("--view").is_some() || sort.is_some() || args.iter().any(|a| a == "--pid") {
            settings.tab = Tab::Processes;
        }
        let detail_tab = match value("--tab") {
            Some("threads") => DetailTab::Threads,
            Some("waits") => DetailTab::Waits,
            Some("modules") => DetailTab::Modules,
            Some("network") => DetailTab::Network,
            _ if args.iter().any(|a| a == "--handles") => DetailTab::Handles,
            _ => DetailTab::default(),
        };
        let find_query = value("--find").map(str::to_string);
        // `--filter TEXT` and `--detail-sort N[d]`: the detail tab's table.
        let mut tables = crate::table::DetailTables::default();
        let mut handle_view = HandleView::default();
        {
            let filter = value("--filter").unwrap_or_default();
            let sort = value("--detail-sort").and_then(crate::table::TableView::parse_sort);
            let table = match detail_tab {
                DetailTab::Threads => Some(&mut tables.threads),
                DetailTab::Modules => Some(&mut tables.modules),
                DetailTab::Network => Some(&mut tables.network),
                _ => None,
            };
            match table {
                Some(t) => {
                    t.set_filter(filter);
                    if sort.is_some() {
                        t.sort = sort;
                    }
                }
                None => handle_view.set_table(filter, sort),
            }
        }
        let pending_pid = args
            .iter()
            .position(|a| a == "--pid")
            .and_then(|i| args.get(i + 1)?.parse().ok());

        let repaint = cc.egui_ctx.clone();
        let collector = Collector::start(
            CollectorConfig {
                interval: Duration::from_millis(settings.interval_ms),
                recorder: recorder_config(&settings),
                ..Default::default()
            },
            move || crate::stalls::wake(&repaint),
        );
        let repaint = cc.egui_ctx.clone();
        let (actions, outcomes) = ActionRunner::start(move || crate::stalls::wake(&repaint));
        actions.run(Action::QueryTaskManager);
        if !settings.blocked.is_empty() {
            actions.run(Action::QueryFirewall(settings.blocked.clone()));
        }
        let repaint = cc.egui_ctx.clone();
        let (inspector, inspections) = Inspector::start(move || crate::stalls::wake(&repaint));
        if let Some(query) = &find_query {
            inspector.request(InspectRequest::FindHandles {
                query: query.clone(),
                exclude: Vec::new(),
            });
        }
        let repaint = cc.egui_ctx.clone();
        let (service_watch, service_events) =
            anabolic_core::services::ServiceWatch::start(move || crate::stalls::wake(&repaint));
        let repaint = cc.egui_ctx.clone();
        let (startup_watch, startup_events) =
            anabolic_core::startup::StartupWatch::start(move || crate::stalls::wake(&repaint));
        let mut zombie_view = ZombieView::default();
        if args.iter().any(|a| a == "--zombies") {
            zombie_view.pending = true;
            inspector.request(InspectRequest::Zombies);
        }

        Self {
            snapshot: collector.latest(),
            collector,
            actions,
            outcomes,
            inspector,
            inspections,
            details: HashMap::new(),
            icons: IconCache::default(),
            table: ProcTable {
                dirty: true,
                ..Default::default()
            },
            settings,
            paused: false,
            detail_tab,
            focus_thread: None,
            handle_view,
            perf_view,
            history: Default::default(),
            wait_view: Default::default(),
            end_view: Default::default(),
            panes: Default::default(),
            tables,
            rules_page,
            rule_engine: Default::default(),
            services,
            service_watch,
            service_events,
            services_at: None,
            startup: Default::default(),
            startup_watch,
            startup_events,
            startup_at: None,
            stutters: Default::default(),
            stutters_traced: false,
            network,
            network_watched: false,
            growth: Default::default(),
            game_mode: Default::default(),
            zombie_view,
            kill_dialog: None,
            affinity_dialog: None,
            confirm: None,
            module_view: ModuleView::default(),
            find: FindView {
                open: find_query.is_some(),
                pending: find_query.is_some(),
                query: find_query.clone().unwrap_or_default(),
                ..Default::default()
            },
            toasts: Toasts::default(),
            alert: None,
            alert_dismissed: false,
            elevated,
            hardening,
            cpu_name: anabolic_core::sysinfo::cpu_name().unwrap_or_default(),
            perf_core,
            cores_traced: false,
            disks_traced: false,
            hardware: anabolic_core::hwinfo::hardware_info(),
            started: Instant::now(),
            ui_time: Duration::ZERO,
            stalls: Default::default(),
            mark: cc.egui_ctx.load_texture(
                "anabolic-mark",
                egui::ColorImage::from_rgba_unmultiplied(
                    [64, 64],
                    include_bytes!("../assets/logo/mark-64.rgba"),
                ),
                egui::TextureOptions::LINEAR,
            ),
            title_bar_styled: false,
            pinned: None,
            focus_search: false,
            pending_pid,
            pending_waits: args.iter().any(|a| a == "--waits"),
            pending_pane: args.iter().any(|a| a == "--pane").then(|| {
                value("--pane")
                    .filter(|v| !v.starts_with("--"))
                    .unwrap_or_default()
                    .to_string()
            }),
            pending_stack: args.iter().any(|a| a == "--stack"),
            help_open: args.iter().any(|a| a == "--help-page"),
            tray: None,
            tray_failed: false,
            start_in_tray: args.iter().any(|a| a == "--tray"),
            hidden: args.iter().any(|a| a == "--tray"),
            quitting: false,
            told_hidden: false,
            alert_seen: false,
            tip_at: Instant::now(),
            taskmgr: None,
            this_app: anabolic_core::taskmgr::app_path().map(|p| p.display().to_string()),
            relaunching: false,
            session_only,
        }
    }

    /// Pulls everything the background threads produced since the last frame.
    fn pump(&mut self, ctx: &egui::Context) {
        let mut new_details = false;
        for e in self.collector.details().try_iter() {
            self.details.insert(e.key, e.details);
            new_details = true;
        }
        let text_sort = !self.settings.sort.numeric() && self.settings.sort != Col::Name;
        if new_details && (!self.table.filter.trim().is_empty() || text_sort) {
            self.table.dirty = true;
        }

        for outcome in self.outcomes.try_iter() {
            match outcome {
                Outcome::Info(msg) => self.toasts.info(msg),
                Outcome::Error(msg) => {
                    self.relaunching = false;
                    let next = next_step(&msg, self.elevated);
                    self.toasts.error_with(msg, next);
                }
                Outcome::Refused { keys, message } => {
                    // The details explain an elevated refusal at length.
                    let next = (!self.elevated)
                        .then(|| next_step(&message, false))
                        .flatten();
                    self.toasts.error_with(message, next);
                    // The selected one shows why at once.
                    if self.table.selected.is_some_and(|k| keys.contains(&k)) {
                        self.detail_tab = DetailTab::General;
                    }
                    self.end_view.refused.extend(keys);
                }
                Outcome::TaskManager(now) => {
                    // The first answer is the startup read, not a change.
                    if self
                        .taskmgr
                        .as_ref()
                        .is_some_and(|t| t.replaced != now.replaced)
                    {
                        self.toasts.info(if now.replaced {
                            "Ctrl+Shift+Esc now opens Anabolic"
                        } else {
                            "Ctrl+Shift+Esc opens Task Manager again"
                        });
                    }
                    self.taskmgr = Some(now);
                }
                Outcome::Relaunched => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Outcome::PowerForGame(previous) => match previous {
                    Some((id, name)) => {
                        self.settings.game.replaced_plan = Some(id);
                        self.game_mode
                            .note(format!("Power plan: High performance (was {name})"));
                    }
                    None => self
                        .game_mode
                        .note("Power plan: already a fast one, left as it is"),
                },
                Outcome::Firewall { path, blocked } => {
                    let file = std::path::Path::new(&path)
                        .file_name()
                        .map_or(path.clone(), |f| f.to_string_lossy().into_owned());
                    self.settings
                        .blocked
                        .retain(|b| !b.eq_ignore_ascii_case(&path));
                    if blocked {
                        self.settings.blocked.push(path);
                        self.toasts
                            .info(format!("{file} is blocked from the network"));
                    } else {
                        self.toasts
                            .info(format!("{file} can use the network again"));
                    }
                }
                Outcome::StillBlocked(paths) => self.settings.blocked = paths,
                Outcome::PowerRestored(name) => {
                    self.settings.game.replaced_plan = None;
                    self.game_mode.note(match name {
                        Some(name) => format!("Power plan: {name} again"),
                        None => "Power plan: changed meanwhile, left as it is".into(),
                    });
                }
                Outcome::Dumped {
                    target,
                    kind,
                    report,
                } => {
                    let frozen = if report.cloned {
                        ""
                    } else {
                        "  It was paused while the file was written."
                    };
                    self.toasts.info_with(
                        format!(
                            "Wrote a {} dump of {} ({}) in {}: {}{frozen}",
                            kind.label(),
                            target.name,
                            fmt::bytes(report.bytes),
                            fmt::millis(report.took),
                            report.path.display()
                        ),
                        "Show in folder",
                        UiCmd::OpenLocation(report.path.to_string_lossy().into_owned()),
                    );
                }
                Outcome::Affinity {
                    target,
                    process_mask,
                    system_mask,
                } => {
                    self.affinity_dialog = Some(AffinityDialog {
                        target,
                        mask: process_mask,
                        system_mask,
                    });
                }
            }
        }

        for inspection in self.inspections.try_iter() {
            match inspection {
                Inspection::Handles(report) => self.handle_view.set_report(report),
                Inspection::Modules(report) => {
                    self.module_view.pending = None;
                    self.module_view.report = Some(report);
                }
                Inspection::WhyAlive { key, report } => {
                    if self.end_view.pending == Some(key) {
                        self.end_view.pending = None;
                    }
                    match report {
                        Ok(report) => {
                            self.end_view.report = Some((report, std::time::Instant::now()));
                            self.end_view.error = None;
                        }
                        Err(e) => self.end_view.error = Some((key, e)),
                    }
                }
                Inspection::Stack { key, tid, report } => {
                    if self.panes.stack_pending == Some((key, tid)) {
                        self.panes.stack_pending = None;
                    }
                    match report {
                        Ok(report) => {
                            self.panes.stack = Some(report);
                            self.panes.stack_error = None;
                        }
                        Err(e) => self.panes.stack_error = Some(((key, tid), e)),
                    }
                }
                Inspection::FileFacts { path, facts } => {
                    if self.panes.facts_pending.as_ref() == Some(&path) {
                        self.panes.facts_pending = None;
                    }
                    self.panes.facts.insert(path, facts);
                }
                Inspection::Holders {
                    key,
                    object,
                    holders,
                } => {
                    if self.panes.holders_pending == Some((key, object)) {
                        self.panes.holders_pending = None;
                    }
                    self.panes.holders = Some((key, object, holders));
                }
                Inspection::WaitProgress { key, done, total } => {
                    if self.wait_view.pending == Some(key) {
                        self.wait_view.progress = Some((done, total));
                    }
                }
                Inspection::WaitChain { key, report } => {
                    if self.wait_view.pending == Some(key) {
                        self.wait_view.pending = None;
                        self.wait_view.progress = None;
                    }
                    match report {
                        Ok(report) => {
                            self.wait_view.report = Some(report);
                            self.wait_view.error = None;
                        }
                        Err(e) => self.wait_view.error = Some((key, e)),
                    }
                }
                Inspection::Found(report) => {
                    self.find.pending = false;
                    self.find.report = Some(report);
                }
                Inspection::Zombies(report) => {
                    self.zombie_view.report = Some(report);
                    self.zombie_view.pending = false;
                }
                Inspection::Failed { key, what, error } => {
                    match key {
                        Some(_) => {
                            self.handle_view.pending = None;
                            self.handle_view.error = Some(error.clone());
                        }
                        None => {
                            self.zombie_view.pending = false;
                            self.find.pending = false;
                        }
                    }
                    let next = next_step(&error, self.elevated);
                    self.toasts
                        .error_with(format!("Could not inspect {what}: {error}"), next);
                }
            }
        }

        for event in self.startup_events.try_iter() {
            match event {
                StartupEvent::List(list) => self.startup.list = Some(list),
                StartupEvent::Done(result) => {
                    self.startup.busy = false;
                    match result {
                        Ok(text) => self.toasts.info(text),
                        Err(text) => {
                            let next = next_step(&text, self.elevated);
                            self.toasts.error_with(text, next);
                        }
                    }
                }
            }
        }
        for event in self.service_events.try_iter() {
            match event {
                ServiceEvent::List(list) => self.services.list = Some(list),
                ServiceEvent::Done {
                    name,
                    op,
                    result,
                    dependents,
                } => {
                    self.services.busy.remove(&name);
                    match result {
                        Ok(text) => self.toasts.info(text),
                        // Others stand in the way: ask about them.
                        Err(_) if !dependents.is_empty() => {
                            let display = self
                                .services
                                .list
                                .as_ref()
                                .and_then(|l| l.services.iter().find(|s| s.name == name))
                                .map_or_else(|| name.clone(), |s| s.display.clone());
                            let restart = matches!(op, ServiceOp::Restart { .. });
                            let dialog = ConfirmDialog {
                                title: format!(
                                    "{} {display} and what depends on it?",
                                    if restart { "Restart" } else { "Stop" }
                                ),
                                lines: vec![
                                    format!(
                                        "These running services need {display} and stop with it{}:",
                                        if restart { ", then start again" } else { "" }
                                    ),
                                    dependents.join(", "),
                                ],
                                confirm: if restart {
                                    "Restart them all".into()
                                } else {
                                    "Stop them all".into()
                                },
                            };
                            let op = if restart {
                                ServiceOp::Restart { dependents: true }
                            } else {
                                ServiceOp::Stop { dependents: true }
                            };
                            self.confirm = Some((
                                dialog,
                                Pending::ServiceWithDependents { name, display, op },
                            ));
                        }
                        Err(text) => {
                            let next = next_step(&text, self.elevated);
                            self.toasts.error_with(text, next);
                        }
                    }
                }
            }
        }

        if !self.paused {
            let latest = self.collector.latest();
            if latest.seq != self.snapshot.seq {
                self.snapshot = latest;
                self.table.dirty = true;
                self.update_alert();
                if self.snapshot.seq.is_multiple_of(30) {
                    self.prune();
                }
                // Wait for the second sample, when CPU figures (and thus the sort) settle.
                if let Some(pid) = self.pending_pid
                    && self.snapshot.seq >= 2
                    && let Some(p) = self
                        .snapshot
                        .processes
                        .iter()
                        .find(|p| p.pid == pid && p.state != ProcState::Exited)
                {
                    let key = p.key;
                    self.pending_pid = None;
                    self.go_to(key);
                    if std::mem::take(&mut self.pending_waits) {
                        self.analyze_waits(key);
                    }
                }
            }
        }
        if self.pending_pane.is_some() && self.pending_pid.is_none() {
            self.open_pending_pane();
        }
        // Nobody sees the table while the window is closed.
        if self.table.dirty && !self.hidden {
            self.table
                .rebuild(&self.snapshot, &self.details, &self.settings);
        }
    }

    /// Forgets details, collapse state and icons of processes long gone.
    fn prune(&mut self) {
        let snap = &self.snapshot;
        self.details.retain(|k, _| snap.find(*k).is_some());
        self.table.collapsed.retain(|k| snap.find(*k).is_some());
        let live: HashSet<u64> = self
            .details
            .values()
            .filter_map(|d| d.icon.as_ref().map(|i| i.id))
            .collect();
        self.icons.retain(|id| live.contains(&id));
    }

    fn update_alert(&mut self) {
        let m = &self.snapshot.system.memory;
        let ram = m.used() as f64 * 100.0 / m.total.max(1) as f64;
        let commit = m.commit as f64 * 100.0 / m.commit_limit.max(1) as f64;
        let threshold = self.settings.memory_alert_pct as f64;
        let peak = ram.max(commit);
        // A little hysteresis so the banner does not flicker around the line.
        let active = if self.alert.is_some() {
            peak >= threshold - 3.0
        } else {
            peak >= threshold
        };
        if !active {
            self.alert = None;
            self.alert_dismissed = false;
            return;
        }
        let (what, pct, metric): (_, _, fn(&ProcessInfo) -> u64) = if ram >= commit {
            ("RAM", ram, |p| p.private_working_set)
        } else {
            ("Commit", commit, |p| p.private_bytes)
        };
        let live = self
            .snapshot
            .processes
            .iter()
            .filter(|p| p.state != ProcState::Exited && !p.is_pseudo() && p.pid != 4);
        let mut apps: HashMap<String, (Arc<str>, u64, usize)> = HashMap::new();
        let mut process: Option<(ProcKey, Arc<str>, u64)> = None;
        for p in live {
            let bytes = metric(p);
            let app = apps
                .entry(p.name.to_ascii_lowercase())
                .or_insert_with(|| (p.name.clone(), 0, 0));
            app.1 += bytes;
            app.2 += 1;
            if process.as_ref().is_none_or(|(_, _, b)| bytes > *b) {
                process = Some((p.key, p.name.clone(), bytes));
            }
        }
        let app = apps.into_values().max_by_key(|a| a.1);
        self.alert = Some(MemoryAlert {
            what,
            pct,
            app,
            process,
        });
    }

    fn target(&self, key: ProcKey) -> Option<Target> {
        self.snapshot.find(key).map(|p| Target {
            key,
            name: p.name.clone(),
        })
    }

    fn select(&mut self, key: Option<ProcKey>) {
        self.table.selected_group = None;
        if self.table.selected != key {
            self.table.selected = key;
            self.focus_thread = None;
            self.collector.send(Command::Select(key));
        }
    }

    fn select_group(&mut self, id: Arc<str>) {
        self.select(None);
        self.table.selected_group = Some(id);
    }

    /// Switches to the process list, selects `key` and makes sure its row is
    /// visible (expanding parents/groups, dropping a filter that hides it).
    fn go_to(&mut self, key: ProcKey) {
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
        if self.table.selected_row(&self.snapshot).is_none() && !self.table.filter.is_empty() {
            self.table.filter.clear();
            self.table
                .rebuild(&self.snapshot, &self.details, &self.settings);
        }
        self.table.scroll_to_selected = true;
    }

    /// The process and everything below it, children first.
    fn tree_targets(&self, root: ProcKey) -> Vec<Target> {
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

    fn confirm_kill(
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

    fn request_kill(&mut self, key: ProcKey, tree: bool) {
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

    fn request_kill_group(&mut self, id: &str) {
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

    fn handle(&mut self, cmds: Vec<UiCmd>) {
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
                UiCmd::RelaunchElevated => {
                    self.relaunching = true;
                    self.actions.run(Action::RelaunchElevated);
                }
                UiCmd::SetStartup(item, on) => {
                    self.startup.busy = true;
                    self.startup_watch.set_enabled(*item, on);
                }
                UiCmd::SelectService(name) => {
                    self.settings.tab = Tab::Services;
                    self.services.selected = Some(name);
                }
                UiCmd::ShowServices(pid) => {
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
                            format!(
                                "Anabolic will ask Windows what thread {tid} of {} (PID {}) is waiting for, and \
                                 follow the chain into other processes; the process' other threads come after \
                                 it. Windows opens the threads for reading; nothing is injected or suspended.",
                                p.name, p.pid
                            ),
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
                UiCmd::ResetColumns => self.settings.columns = Settings::default().columns,
                UiCmd::InspectHandles { key, names } => self.inspect_handles(key, names),
                UiCmd::ConfirmResolveNames(key) => {
                    let Some(p) = self.snapshot.find(key) else {
                        continue;
                    };
                    let dialog = ConfirmDialog {
                        title: "Resolve handle names?".into(),
                        lines: vec![
                            format!(
                                "Anabolic will open {} (PID {}) and duplicate each of its {} handles to read \
                                 their names: file paths, registry keys, object names, and the processes \n                                  and \
                                 threads they point to.",
                                p.name,
                                p.pid,
                                fmt::count(p.handles as u64)
                            ),
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
                    let dialog = ConfirmDialog {
                        title: format!("Write a {} dump of {}?", kind.label(), p.name),
                        lines: vec![
                            what,
                            format!(
                                "Anabolic opens {} (PID {}) to read its memory and copies it \
                                 first, so it pauses only for a moment. The file goes to {dir}.",
                                p.name, p.pid
                            ),
                            "Reading another program's memory is what anti-cheat software \
                             guards against: do not dump a protected game, or the anti-cheat \
                             itself, while you play."
                                .into(),
                        ],
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
                            format!(
                                "Anabolic will ask Windows what each thread of {} (PID {}) is waiting for, \
                                 and follow the chain into other processes. Windows opens the threads for \
                                 reading; nothing is injected or suspended.",
                                p.name, p.pid
                            ),
                            format!(
                                "Windows spends about 60 ms of CPU on each waiting thread. {} has {} threads, so \
                                 this can take up to {} s, on four threads at low priority. Threads that are \
                                 running or idle are left out.",
                                p.name,
                                p.threads,
                                (p.threads as f32 * 0.06 / 4.0).ceil().max(1.0)
                            ),
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
            }
        }
        if self.table.dirty {
            self.table
                .rebuild(&self.snapshot, &self.details, &self.settings);
        }
    }

    fn run_on(&self, key: ProcKey, make: impl FnOnce(Target) -> Action) {
        if let Some(t) = self.target(key) {
            self.actions.run(make(t));
        }
    }

    fn set_view(&mut self, view: View) {
        self.settings.view = view;
        if view != View::Apps {
            self.table.selected_group = None;
        }
        self.table.dirty = true;
    }

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<UiCmd>) {
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
    fn nav_rail(&mut self, ui: &mut egui::Ui) {
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
            let settings =
                widgets::rail_item(ui, theme::icon::SETTINGS, "Settings", false, collapsed);
            self.settings_menu(&settings);
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // The filter and views belong to the process list.
            if self.settings.tab == Tab::Processes {
                self.list_controls(ui);
            }
            self.global_controls(ui);
        });
    }

    fn list_controls(&mut self, ui: &mut egui::Ui) {
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
    fn global_controls(&mut self, ui: &mut egui::Ui) {
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

    fn settings_menu(&mut self, button: &egui::Response) {
        egui::Popup::menu(button).align(egui::RectAlign::RIGHT_END).show(|ui| {
            ui.set_min_width(340.0);
            ui.label(widgets::strong("Settings", theme::BODY));
            ui.separator();
            let mut replaced = self.taskmgr.as_ref().is_some_and(|t| t.replaced);
            let toggle = ui
                .add_enabled(
                    self.elevated && self.taskmgr.is_some(),
                    egui::Checkbox::new(&mut replaced, "Open Anabolic with Ctrl+Shift+Esc"),
                )
                .on_hover_text(
                    "Replaces Task Manager everywhere (Ctrl+Shift+Esc, the taskbar menu, taskmgr). Starts elevated \
                     without a UAC prompt through a scheduled task. Turn it off to get Task Manager back.",
                )
                .on_disabled_hover_text("Needs Anabolic running as administrator.");
            if toggle.changed() {
                self.actions.run(Action::ReplaceTaskManager { on: replaced, launcher: LAUNCHER });
            }
            if let Some(registered) = self.taskmgr.as_ref().filter(|t| t.replaced) {
                if let Some(path) = &registered.app {
                    ui.label(RichText::new(format!("Starts {path}")).size(theme::CAPTION).color(theme::TEXT_WEAK));
                }
                if registered.app.is_some() && registered.app != self.this_app {
                    ui.label(
                        RichText::new("That is another build than this one: turn it off and on to switch.")
                            .size(theme::CAPTION)
                            .color(theme::ELEVATED_TEXT),
                    );
                }
            }
            ui.checkbox(&mut self.settings.confirm_kill, "Ask before terminating processes");
            ui.horizontal(|ui| {
                ui.label("Memory warning at");
                ui.add(egui::Slider::new(&mut self.settings.memory_alert_pct, 50.0..=99.0).suffix(" %").integer());
            });
            ui.horizontal(|ui| {
                let mut changed = ui
                    .checkbox(&mut self.settings.record_history, "Keep a history for")
                    .on_hover_text(
                        "Every 10 seconds while Anabolic runs: the machine's totals and the processes \
                         that led. Look back on the History page (Ctrl+3).",
                    )
                    .changed();
                let days = ui.add_enabled(
                    self.settings.record_history,
                    egui::DragValue::new(&mut self.settings.history_days).range(1..=90).suffix(" days"),
                );
                // Not on every step of a drag: each change restarts the recorder.
                changed |= days.drag_stopped() || (days.changed() && !days.dragged());
                if changed {
                    self.collector.send(Command::Record(recorder_config(&self.settings)));
                }
            });
            ui.horizontal(|ui| {
                ui.label("Graphs show").on_hover_text(
                    "Fit stretches each graph over the history collected so far. \
                     A fixed window keeps its length from the start, and new values flow in from the right.",
                );
                for span in GraphSpan::ALL {
                    ui.selectable_value(&mut self.settings.graph_span, span, span.label());
                }
            });
            ui.checkbox(
                &mut self.settings.close_to_tray,
                "Keep running in the notification area when closed",
            )
            .on_hover_text(
                "Closing the window leaves Anabolic running: it keeps recording the history and \
                 applying your rules. Its icon opens it again; right-click it to quit.",
            );
            if self.settings.close_to_tray && self.tray_failed {
                ui.label(
                    RichText::new("There is no notification area here, so closing still quits.")
                        .size(theme::CAPTION)
                        .color(theme::ELEVATED_TEXT),
                );
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Shortcuts and colours").shortcut_text("F1"))
                .clicked()
            {
                self.help_open = true;
                ui.close();
            }
        });
    }

    fn alert_bar(&mut self, ui: &mut egui::Ui, out: &mut Vec<UiCmd>) {
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

    fn status_bar(&mut self, ui: &mut egui::Ui, out: &mut Vec<UiCmd>) {
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

    fn hardening_report(&self, ui: &mut egui::Ui) {
        ui.label(widgets::strong(
            "Staying responsive under load",
            theme::BODY,
        ));
        ui.label(
            RichText::new(format!("Information level: {:?}", self.snapshot.info_level))
                .color(theme::TEXT_WEAK),
        );
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

    fn inspect_handles(&mut self, key: ProcKey, names: bool) {
        if self.table.selected != Some(key) {
            self.select(Some(key));
            self.table.scroll_to_selected = true;
        }
        self.detail_tab = DetailTab::Handles;
        self.handle_view.pending = Some(key);
        self.handle_view.error = None;
        self.inspector.request(InspectRequest::Handles {
            key,
            resolve_names: names,
        });
    }

    /// Follows what the process' threads wait for, a hung window's thread
    /// first, and shows the answer in its details.
    fn analyze_waits(&mut self, key: ProcKey) {
        let first = self
            .snapshot
            .system
            .hung
            .iter()
            .find(|h| h.pid == key.pid)
            .map(|h| h.tid);
        self.analyze_waits_from(key, first);
    }

    /// Like `analyze_waits`, with `first` analysed and listed first.
    fn analyze_waits_from(&mut self, key: ProcKey, first: Option<u32>) {
        self.wait_view.pending = Some(key);
        self.wait_view.started = Some(std::time::Instant::now());
        self.wait_view.progress = None;
        self.wait_view.error = None;
        self.inspector
            .request(InspectRequest::WaitChain { key, first });
        self.go_to(key);
        self.detail_tab = DetailTab::Waits;
        self.settings.show_details = true;
    }

    /// The notification area icon: there while it is wanted, its clicks,
    /// its tooltip, and what a closed window should not keep running.
    fn tray_logic(&mut self, ctx: &egui::Context) {
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
        // Tracing that only feeds a page on screen stops with the window.
        if self.hidden {
            if self.cores_traced {
                self.cores_traced = false;
                self.collector.send(Command::TraceCores(false));
            }
            if self.disks_traced {
                self.disks_traced = false;
                self.collector.send(Command::TraceDisks(false));
            }
            if self.network_watched {
                self.network_watched = false;
                self.collector.send(Command::NetActivity(false));
            }
        }
    }

    /// `--pane`: waits for what the tab needs, then selects the row.
    fn open_pending_pane(&mut self) {
        let Some(key) = self.table.selected else {
            return;
        };
        let name = self.pending_pane.clone().unwrap_or_default().to_lowercase();
        match self.detail_tab {
            DetailTab::Threads => {
                let Some(first) = self
                    .snapshot
                    .selected
                    .as_ref()
                    .filter(|s| s.key == key)
                    .and_then(|s| s.threads.iter().min_by_key(|t| t.create_time))
                    .map(|t| t.tid)
                else {
                    return;
                };
                self.focus_thread = Some((first, false));
                self.load_modules(key);
                if self.pending_stack {
                    self.panes.stack_ok.insert(key);
                    self.read_stack(key, first);
                }
            }
            DetailTab::Modules => {
                let Some(report) = self.module_view.report.clone().filter(|r| r.key == key) else {
                    self.load_modules(key);
                    return;
                };
                self.module_view.selected = report
                    .modules
                    .iter()
                    .find(|m| m.info.name.to_lowercase() == name)
                    .or(report.modules.get(1))
                    .map(|m| m.info.base);
            }
            DetailTab::Handles => {
                let Some(report) = self.handle_view.report.clone().filter(|r| r.key == key) else {
                    return;
                };
                self.handle_view.selected = report
                    .entries
                    .iter()
                    .find(|e| report.type_name(e.type_index).to_lowercase() == name)
                    .or(report.entries.first())
                    .map(|e| e.value);
            }
            DetailTab::Network => {
                // The busiest connection, once some traffic was seen.
                let Some(c) = self
                    .snapshot
                    .selected
                    .as_ref()
                    .filter(|s| s.key == key)
                    .and_then(|s| s.connections.iter().max_by_key(|c| c.rx_total + c.tx_total))
                else {
                    return;
                };
                self.tables.connection = Some((c.proto, c.remote, c.remote_port, c.local_port));
            }
            _ => {}
        }
        self.pending_pane = None;
    }

    fn read_stack(&mut self, key: ProcKey, tid: u32) {
        self.panes.stack_pending = Some((key, tid));
        self.panes.stack_error = None;
        self.inspector.request(InspectRequest::Stack { key, tid });
    }

    fn load_modules(&mut self, key: ProcKey) {
        if self.module_view.pending == Some(key) {
            return;
        }
        let addresses = self
            .snapshot
            .selected
            .as_ref()
            .filter(|s| s.key == key)
            .map(|s| {
                s.threads
                    .iter()
                    .map(|t| {
                        if t.win32_start_address != 0 {
                            t.win32_start_address
                        } else {
                            t.start_address
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tids = self
            .snapshot
            .selected
            .as_ref()
            .filter(|s| s.key == key)
            .map(|s| s.threads.iter().map(|t| t.tid).collect())
            .unwrap_or_default();
        self.module_view.pending = Some(key);
        self.inspector.request(InspectRequest::Modules {
            key,
            addresses,
            tids,
        });
    }

    fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
    }

    fn find_window(&mut self, ctx: &egui::Context, out: &mut Vec<UiCmd>) {
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
                    ui.add(TextEdit::singleline(&mut find.exclude).hint_text("e.g. BlackDesert64.exe, avp.exe").desired_width(260.0));
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

    fn dialogs(&mut self, ctx: &egui::Context) {
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
    fn maybe_pin(&mut self) {
        if self.pinned.is_some() || self.started.elapsed() < Duration::from_secs(5) {
            return;
        }
        let ws = hardening::own_working_set() as usize;
        let target = ws + ws / 4 + 32 * 1024 * 1024;
        self.pinned = Some(hardening::pin_working_set(target).map(|_| target as u64));
    }
}

/// The flight recorder's setup; None while the history is off.
fn recorder_config(settings: &Settings) -> Option<RecorderConfig> {
    if !settings.record_history {
        return None;
    }
    Some(RecorderConfig {
        dir: crate::data_dir()?.join("history"),
        keep_days: settings.history_days,
    })
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let started = Instant::now();
        crate::bench::frame(ui.ctx(), frame.info().cpu_usage, self.ui_time);
        let page = match self.settings.tab {
            Tab::Processes => "Processes".to_string(),
            Tab::Performance => format!("Performance/{:?}", self.perf_view),
            Tab::History => "History".to_string(),
            Tab::Rules => "Rules".to_string(),
            Tab::Services => "Services".to_string(),
            Tab::Startup => "Startup".to_string(),
            Tab::Stutters => "Stutters".to_string(),
            Tab::Network => "Network".to_string(),
        };
        self.stalls.frame(ui.ctx(), frame.info().cpu_usage, &page);
        if !self.title_bar_styled {
            let rgb = |c: Color32| u32::from_be_bytes([0, c.r(), c.g(), c.b()]);
            self.title_bar_styled =
                anabolic_core::instance::style_title_bar(rgb(theme::PANEL), rgb(theme::BORDER));
        }
        let ctx = ui.ctx().clone();
        // `ui` runs only while the window is on screen; `logic` has pumped.
        self.hidden = false;
        // Closing hides the window when its icon can bring it back.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.tray.is_some()
            && (self.settings.close_to_tray || self.start_in_tray)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden = true;
            if !std::mem::replace(&mut self.told_hidden, true)
                && let Some(tray) = &self.tray
            {
                tray.notify(
                    "Anabolic is still running",
                    "It keeps recording the history and applying your rules. Click the icon to \
                     open it; right-click it to quit.",
                    false,
                );
            }
        }
        // Names for addresses: what programs looked up, as it comes in.
        self.tables
            .names
            .set_forward(self.snapshot.system.dns_names.clone());
        // Every graph reads its time span from here, in samples.
        crate::graph::set_window(self.settings.graph_span.seconds().map_or(0, |s| {
            (s * 1000 / self.settings.interval_ms.max(1)) as usize
        }));

        let mut cmds = Vec::new();
        self.shortcuts(&ctx, &mut cmds);

        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &mut cmds));
        let rail = if self.settings.nav_collapsed {
            48.0
        } else {
            172.0
        };
        egui::Panel::left("nav")
            .exact_size(rail)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(4, 0)),
            )
            .show(ui, |ui| self.nav_rail(ui));
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(3.0);
            self.top_bar(ui);
            ui.add_space(1.0);
        });
        if self.alert.is_some() && !self.alert_dismissed {
            egui::Panel::top("memory-alert")
                .frame(
                    Frame::new()
                        .fill(theme::BAD_FILL)
                        .inner_margin(Margin::symmetric(10, 5)),
                )
                .show(ui, |ui| self.alert_bar(ui, &mut cmds));
        }

        match self.settings.tab {
            Tab::Processes => {
                let group = self
                    .table
                    .selected_group
                    .as_deref()
                    .and_then(|id| self.table.group(id))
                    .cloned();
                if self.settings.show_details && (group.is_some() || self.table.selected.is_some())
                {
                    egui::Panel::bottom("details")
                        .resizable(true)
                        // The side panes of the tabs want room; a stored size wins.
                        .default_size((ui.available_height() * 0.42).max(260.0))
                        .min_size(110.0)
                        .show(ui, |ui| {
                            // Fill the panel whatever the tab holds, so
                            // switching tabs never changes its height.
                            ui.set_min_height(ui.available_height());
                            match (&group, self.table.selected) {
                                (Some(g), _) => details::show_group(
                                    ui,
                                    g,
                                    &self.snapshot,
                                    &self.details,
                                    &mut self.icons,
                                    &mut cmds,
                                ),
                                (None, Some(key)) => details::show(
                                    ui,
                                    &mut self.detail_tab,
                                    key,
                                    &self.snapshot,
                                    &self.details,
                                    &mut self.icons,
                                    &mut self.handle_view,
                                    &mut self.module_view,
                                    &self.panes,
                                    &mut self.tables,
                                    &self.wait_view,
                                    &self.end_view,
                                    &mut self.focus_thread,
                                    &self.services.hosted(key.pid),
                                    &self.growth,
                                    &self.settings.blocked,
                                    &mut cmds,
                                ),
                                (None, None) => {}
                            }
                        });
                }
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    proc_table::show(
                        ui,
                        &mut self.table,
                        &self.snapshot,
                        &self.details,
                        &mut self.icons,
                        &self.settings,
                        &mut cmds,
                    );
                });
            }
            Tab::Performance => {
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    perf::show(
                        ui,
                        &mut self.perf_view,
                        &mut self.perf_core,
                        &self.snapshot,
                        &self.cpu_name,
                        &self.hardware,
                        &self.zombie_view,
                        &mut cmds,
                    );
                });
            }
            Tab::Rules => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            crate::gamemode::card(ui, &mut self.settings.game, &self.game_mode);
                            ui.add_space(12.0);
                            crate::rules::show(
                                ui,
                                &mut self.rules_page,
                                &mut self.settings.rules,
                                &self.snapshot,
                            );
                            crate::rules::blocked_list(ui, &self.settings.blocked, &mut cmds);
                        });
                });
            }
            Tab::Services => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    crate::services_page::show(
                        ui,
                        &mut self.services,
                        &self.snapshot,
                        self.elevated,
                        &mut cmds,
                    );
                });
            }
            Tab::Startup => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    crate::startup_page::show(
                        ui,
                        &mut self.startup,
                        &self.snapshot,
                        &self.details,
                        self.elevated,
                        &mut cmds,
                    );
                });
            }
            Tab::Stutters => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    crate::stutters_page::show(
                        ui,
                        &mut self.stutters,
                        &self.snapshot,
                        self.elevated,
                        &mut self.settings.watch_stutters,
                        self.relaunching,
                        &mut cmds,
                    );
                });
            }
            Tab::Network => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    crate::network_page::show(
                        ui,
                        &mut self.network,
                        &self.snapshot,
                        &self.details,
                        &mut self.tables.names,
                        &self.settings.blocked,
                        &mut cmds,
                    );
                });
            }
            Tab::History => {
                let recording = self.settings.record_history;
                let days = self.settings.history_days;
                let in_tray = self.settings.close_to_tray;
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            crate::history_page::show(
                                ui,
                                &mut self.history,
                                &self.snapshot,
                                crate::data_dir().map(|d| d.join("history")),
                                recording,
                                days,
                                in_tray,
                                &mut cmds,
                            );
                        });
                });
            }
        }

        self.find_window(&ctx, &mut cmds);
        self.handle(cmds);
        // The service list: every refresh while its page is open; otherwise
        // now and then, for the services a process' details name.
        let every = if self.settings.tab == Tab::Services {
            Duration::from_millis(self.settings.interval_ms.max(1000))
        } else {
            Duration::from_secs(15)
        };
        if self.services_at.is_none_or(|at| at.elapsed() >= every) {
            self.services_at = Some(Instant::now());
            self.service_watch.refresh();
        }
        // Startup entries change rarely: read on opening the page, then
        // every half minute while it stays open.
        if self.settings.tab == Tab::Startup
            && self
                .startup_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(30))
        {
            self.startup_at = Some(Instant::now());
            self.startup_watch.refresh();
        } else if self.settings.tab != Tab::Startup {
            self.startup_at = None;
        }
        // Every connection, with its socket tables read each refresh: only
        // while the Network page is on screen.
        let want = self.settings.tab == Tab::Network;
        if want != self.network_watched {
            self.network_watched = want;
            self.collector.send(Command::NetActivity(want));
        }
        // Context switch tracing costs CPU; run it only while it is on screen.
        let want = self.settings.tab == Tab::Performance && self.perf_view == PerfView::Cpu;
        if want != self.cores_traced {
            self.cores_traced = want;
            self.collector.send(Command::TraceCores(want));
        }
        let want = self.settings.tab == Tab::Performance
            && matches!(self.perf_view, PerfView::Disk | PerfView::PhysicalDisk(_));
        if want != self.disks_traced {
            self.disks_traced = want;
            self.collector.send(Command::TraceDisks(want));
        }
        self.dialogs(&ctx);
        crate::help::show(&ctx, &mut self.help_open);
        let mut later = Vec::new();
        self.toasts.show(&ctx, &mut later);
        self.handle(later);
        self.maybe_pin();
        #[cfg(debug_assertions)]
        crate::shot::frame(&ctx);
        self.ui_time = started.elapsed();
    }

    /// Runs before every frame, and while the window is closed into the
    /// notification area whenever new data arrives (no frame is drawn
    /// then): what must go on without a window lives here.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);
        // Rules: standing orders go to the action thread quietly; alerts
        // become toasts, or notifications while the window is closed.
        let (actions, alerts) = self.rule_engine.step(&self.settings.rules, &self.snapshot);
        for action in actions {
            self.actions.run(Action::Quiet(Box::new(action)));
        }
        for alert in alerts {
            match &self.tray {
                Some(tray) if self.hidden => tray.notify("Anabolic rule", &alert, true),
                _ => self.toasts.error(alert),
            }
        }
        self.tray_logic(ctx);
        // Frames and DPCs: while their page is on screen, or always when
        // asked to keep watching (a game played with the window closed).
        let want =
            self.settings.watch_stutters || (!self.hidden && self.settings.tab == Tab::Stutters);
        if want != self.stutters_traced {
            self.stutters_traced = want;
            self.collector.send(Command::TraceFrames(want));
            self.collector.send(Command::TraceLatency(want));
        }
        self.stutters.observe(&self.snapshot);
        self.growth.observe(&self.snapshot);
        for action in self.game_mode.step(
            &mut self.settings.game,
            &self.snapshot,
            &self.settings.rules,
        ) {
            self.actions.run(action);
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::bench::inject(raw_input);
    }

    fn on_exit(&mut self) {
        // Stops the ETW session before the process goes away.
        self.collector.shutdown();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.session_only {
            return;
        }
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }
}

#[cfg(test)]
mod tests {
    use super::next_step;

    #[test]
    fn failures_say_what_to_do() {
        let denied = "Could not terminate x.exe (12): Access is denied.";
        assert!(next_step(denied, false).unwrap().contains("administrator"));
        assert!(next_step(denied, true).unwrap().contains("protects it"));
        assert!(
            next_step("Could not read: The parameter is incorrect.", true)
                .unwrap()
                .contains("exited")
        );
        assert_eq!(next_step("Something odd happened", true), None);
    }
}
