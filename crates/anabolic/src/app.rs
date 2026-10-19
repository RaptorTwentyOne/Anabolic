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
use eframe::egui::{self, Align, Frame, Key, Layout, Margin, Modifiers, RichText, TextEdit};

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

mod chrome;
mod commands;
mod inspect;
pub(crate) mod mcp;
mod settings_page;
mod update;
mod window;

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

/// What to do about a failure, from what the system said; None when there
/// is nothing useful to add.
fn next_step(message: &str, elevated: bool) -> Option<String> {
    let m = message.to_lowercase();
    let says = |what: &str| m.contains(what);
    let denied = says("access is denied")
        || says("required privilege is not held")
        || says("not permitted")
        || says("permission denied");
    let step = if denied {
        if elevated && cfg!(windows) {
            "Windows or a security program protects it; its details (General) say what Anabolic can tell."
        } else if elevated {
            "The system or a security policy protects it; its details (General) say what Anabolic can tell."
        } else {
            crate::platform::ELEVATE_AND_RETRY
        }
    } else if says("parameter is incorrect")
        || says("has exited")
        || says("no longer")
        || says("no such process")
    {
        "It has probably exited already; the list catches up on the next refresh."
    } else if says("being used by another process") || says("clipboard") {
        "Another program is holding it; try again in a moment."
    } else if says("cannot find the file") || says("cannot find the path") {
        "The file has been moved or deleted since it was loaded."
    } else if says("not supported") {
        if cfg!(windows) {
            "This Windows version does not offer it."
        } else {
            "This kernel does not offer it."
        }
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
    /// Command lines, kept after their processes exit: a parent that is
    /// gone is still described in "Started by".
    cmdlines: HashMap<ProcKey, Arc<str>>,
    /// The monitor a full-screen game filled at startup: the window moves
    /// off it once it exists.
    #[cfg(windows)]
    avoid_monitor: Option<isize>,
    /// The window's monitor was last checked (one unplugged or switched
    /// off strands a window).
    #[cfg(windows)]
    placed_at: Instant,
    /// The monitors, read on a thread of their own while their view is open.
    displays: Option<Arc<Vec<anabolic_core::displays::Display>>>,
    displays_rx: Option<Receiver<Vec<anabolic_core::displays::Display>>>,
    displays_at: Option<Instant>,
    /// What draws the window: the GPU, or Windows' software renderer.
    renderer: Option<String>,
    game_mode: crate::gamemode::GameMode,
    /// Frame and DPC tracing are on.
    stutters_traced: bool,
    network: crate::network_page::NetworkPage,
    /// Recording what a program does, step by step.
    activity: crate::activity_page::ActivityPage,
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
    #[cfg(windows)]
    title_bar_styled: bool,
    pinned: Option<Result<u64, String>>,
    focus_search: bool,
    /// The hotkey held from Windows, the choice it was asked for, and why
    /// Windows refused it. A `--new` instance leaves it to the first one.
    hotkey: Option<anabolic_core::hotkey::Hotkey>,
    hotkey_for: Option<crate::settings::Hotkey>,
    hotkey_error: Option<String>,
    serves_hotkey: bool,
    /// The MCP server and what it reported, for the MCP page.
    mcp: mcp::McpState,
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
    /// New releases: checked for, installed, announced.
    update: update::UpdateState,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        hardening: Vec<HardeningItem>,
        elevated: bool,
        #[cfg_attr(not(windows), allow(unused_variables))] avoid_monitor: Option<isize>,
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
                    | "--activity"
                    | "--activity-filter"
                    | "--record"
                    | "--mcp-port"
                    | "--mcp"
                    | "--settings"
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
        if args.iter().any(|a| a == "--activity") {
            settings.tab = Tab::Activity;
        }
        if args.iter().any(|a| a == "--mcp") {
            settings.tab = Tab::Mcp;
        }
        if args.iter().any(|a| a == "--settings") {
            settings.tab = Tab::Settings;
        }
        // `--record PID` or `--record all`: record from the start (tests).
        let mut activity = crate::activity_page::ActivityPage::default();
        match value("--record") {
            Some("all") => {
                activity.record_everything();
                settings.tab = Tab::Activity;
            }
            Some(v) => {
                if let Ok(pid) = v.parse::<u32>() {
                    activity.record_pids(&[pid], format!("PID {pid} and what it starts"));
                    settings.tab = Tab::Activity;
                }
            }
            None => {}
        }
        if let Some(text) = value("--activity-filter") {
            activity.set_filter(text);
        }
        // `--mcp-port N [--mcp-token KEY] [--mcp-allow read,trace,...|all]`:
        // the MCP server on for this session (tests). A `--new` instance
        // serves only when given it.
        let mcp_port = value("--mcp-port").and_then(|p| p.parse::<u16>().ok());
        if let Some(port) = mcp_port {
            settings.mcp.enabled = true;
            settings.mcp.port = port;
            settings.mcp.token =
                value("--mcp-token").map_or_else(|| anabolic_core::random_hex(32), str::to_string);
            if let Some(list) = value("--mcp-allow") {
                settings.mcp.allowed = crate::settings::McpKind::parse_list(list);
            }
        }
        let serves_mcp = !args.iter().any(|a| a == "--new") || mcp_port.is_some();
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
                Some("displays") => perf_view = PerfView::Displays,
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
        let mcp_state = mcp::McpState::new(
            &collector,
            &cc.egui_ctx,
            &settings.mcp,
            elevated,
            serves_mcp,
        );
        let repaint = cc.egui_ctx.clone();
        let (actions, outcomes) = ActionRunner::start(move || crate::stalls::wake(&repaint));
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
            activity,
            growth: Default::default(),
            cmdlines: HashMap::new(),
            #[cfg(windows)]
            avoid_monitor,
            #[cfg(windows)]
            placed_at: Instant::now(),
            displays: None,
            displays_rx: None,
            displays_at: None,
            renderer: None,
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
            #[cfg(windows)]
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
            hotkey: None,
            hotkey_for: None,
            hotkey_error: None,
            serves_hotkey: !args.iter().any(|a| a == "--new"),
            mcp: mcp_state,
            relaunching: false,
            session_only,
            update: update::UpdateState::new(&cc.egui_ctx),
        }
    }

    /// Pulls everything the background threads produced since the last frame.
    fn pump(&mut self, ctx: &egui::Context) {
        let mut new_details = false;
        for e in self.collector.details().try_iter() {
            if let Some(cmd) = &e.details.command_line {
                // A long session forgets the oldest at once, rarely.
                if self.cmdlines.len() > 50_000 {
                    self.cmdlines.clear();
                }
                self.cmdlines.insert(e.key, cmd.as_str().into());
            }
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
                Outcome::Relaunched => {
                    // A real quit, not a close into the notification area:
                    // the elevated instance waits for this one to end.
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
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
            Tab::Activity => "Activity".to_string(),
            Tab::Mcp => "MCP".to_string(),
            Tab::Settings => "Settings".to_string(),
        };
        self.stalls.frame(ui.ctx(), frame.info().cpu_usage, &page);
        // The desktop's own decorations draw Linux windows' title bars.
        #[cfg(windows)]
        if !self.title_bar_styled {
            let rgb = |c: egui::Color32| u32::from_be_bytes([0, c.r(), c.g(), c.b()]);
            self.title_bar_styled =
                anabolic_core::instance::style_title_bar(rgb(theme::PANEL), rgb(theme::BORDER));
        }
        if self.renderer.is_none()
            && let Some(renderer) = match frame.wgpu_render_state() {
                Some(state) => {
                    let info = state.adapter.get_info();
                    Some(if info.device_type == eframe::wgpu::DeviceType::Cpu {
                        if cfg!(windows) {
                            "Windows' software renderer (no graphics driver with Direct3D 12)"
                                .to_string()
                        } else {
                            format!("{} (a software renderer)", info.name)
                        }
                    } else if cfg!(windows) {
                        format!("{} (Direct3D 12)", info.name)
                    } else {
                        format!("{} (Vulkan)", info.name)
                    })
                }
                #[cfg(windows)]
                None => frame
                    .gl()
                    .map(|_| "OpenGL (the graphics driver has no Direct3D 12)".to_string()),
                // The driver's own name for the GPU: "Mesa Intel(R) Graphics",
                // or llvmpipe when Mesa draws without one.
                #[cfg(target_os = "linux")]
                None => frame.gl().map(|gl| {
                    use eframe::glow::HasContext;
                    let name = unsafe { gl.get_parameter_string(eframe::glow::RENDERER) };
                    format!("{name} (OpenGL)")
                }),
            }
        {
            // ANABOLIC_RENDERER_LOG=<file>: say which, for tests.
            if let Some(path) = std::env::var_os("ANABOLIC_RENDERER_LOG") {
                let _ = std::fs::write(path, &renderer);
            }
            self.renderer = Some(renderer);
        }
        let ctx = ui.ctx().clone();
        // `ui` runs only while the window is on screen (`logic` has pumped),
        // unless `logic` has just hidden it on a close.
        if !(self.hidden && ctx.input(|i| i.viewport().close_requested())) {
            self.hidden = false;
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
        if self.update.bar_wanted() {
            egui::Panel::top("update")
                .frame(
                    Frame::new()
                        .fill(theme::RAISED)
                        .inner_margin(Margin::symmetric(10, 5)),
                )
                .show(ui, |ui| self.update_bar(ui));
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
                                    &crate::details::Origin {
                                        cmdlines: &self.cmdlines,
                                        services: self.services.list.as_deref(),
                                    },
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
                        self.displays.as_deref().map(Vec::as_slice),
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
            Tab::Settings => {
                egui::CentralPanel::default_margins().show(ui, |ui| self.settings_page(ui));
            }
            Tab::Mcp => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    crate::mcp_page::show(
                        ui,
                        &mut self.settings.mcp,
                        &mut self.mcp,
                        self.elevated,
                        &mut cmds,
                    );
                });
            }
            Tab::Activity => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    crate::activity_page::show(
                        ui,
                        &mut self.activity,
                        &self.snapshot,
                        self.elevated,
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
        #[cfg(windows)]
        self.place_window();
        // The monitors: every 5 s while their view is open.
        if let Some(rx) = &self.displays_rx
            && let Ok(list) = rx.try_recv()
        {
            self.displays = Some(Arc::new(list));
            self.displays_rx = None;
        }
        let want = self.settings.tab == Tab::Performance && self.perf_view == PerfView::Displays;
        if want
            && self.displays_rx.is_none()
            && self
                .displays_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
        {
            self.displays_at = Some(Instant::now());
            let (tx, rx) = crossbeam_channel::bounded(1);
            let wake = ctx.clone();
            let _ = std::thread::Builder::new()
                .name("anabolic-displays".into())
                .spawn(move || {
                    let _ = tx.send(anabolic_core::displays::displays());
                    wake.request_repaint();
                });
            self.displays_rx = Some(rx);
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
        self.hotkey_logic(ctx);
        self.update_logic(ctx);
        // Closing hides the window when its icon can bring it back. Here
        // rather than in `ui`: while eframe takes the window for minimized
        // or hidden it runs only `logic`, and a close not cancelled there
        // ends Anabolic, icon and all.
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
        self.mcp_logic();
        self.trace_logic();
        self.stutters.observe(&self.snapshot);
        self.growth.observe(&self.snapshot);
        // A recording goes on with the window closed.
        self.activity.poll(&self.snapshot);
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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
        assert_eq!(
            next_step(denied, false).as_deref(),
            Some(crate::platform::ELEVATE_AND_RETRY)
        );
        assert!(next_step(denied, true).unwrap().contains("protects it"));
        assert!(
            next_step("Could not read: The parameter is incorrect.", true)
                .unwrap()
                .contains("exited")
        );
        assert_eq!(next_step("Something odd happened", true), None);
    }
}
