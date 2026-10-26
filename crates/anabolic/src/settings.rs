// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! User preferences, persisted by eframe between runs.

use serde::{Deserialize, Serialize};

/// Bumped when stored settings need migrating.
const VERSION: u32 = 4;
/// Commands Run new task remembers.
pub const RUN_HISTORY: usize = 20;
/// Characters a watched port's name keeps.
pub const PORT_NAME: usize = 40;

/// A port kept an eye on: free, or who holds it. Its name says what it is
/// for, when given ("my API").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortWatch {
    pub port: u16,
    #[serde(default)]
    pub name: String,
}

/// Tables whose columns the settings keep, at most, and columns each.
const TABLES: usize = 64;
const TABLE_COLUMNS: usize = 64;

/// A table's columns as they were left (see `table::Columns`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableLayout {
    /// The table's column numbers in the order they show; empty: the usual
    /// order.
    pub order: Vec<u8>,
    /// Each column's width, by its number; 0: as wide as it starts.
    pub widths: Vec<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Col {
    Name,
    Pid,
    Cpu,
    CpuTime,
    Private,
    WorkingSet,
    PrivateWs,
    Disk,
    Net,
    Io,
    Gpu,
    Threads,
    Handles,
    HandleTrend,
    Priority,
    User,
    Integrity,
    Arch,
    Session,
    Description,
    Company,
    Version,
    Path,
    CommandLine,
}

impl Col {
    pub const ALL: [Col; 24] = [
        Col::Name,
        Col::Pid,
        Col::Cpu,
        Col::CpuTime,
        Col::PrivateWs,
        Col::Private,
        Col::WorkingSet,
        Col::Disk,
        Col::Net,
        Col::Io,
        Col::Gpu,
        Col::Threads,
        Col::Handles,
        Col::HandleTrend,
        Col::Priority,
        Col::User,
        Col::Integrity,
        Col::Arch,
        Col::Session,
        Col::Description,
        Col::Company,
        Col::Version,
        Col::Path,
        Col::CommandLine,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Pid => "PID",
            Col::Cpu => "CPU",
            Col::CpuTime => "CPU time",
            Col::Private => "Private bytes",
            Col::WorkingSet => "Working set",
            Col::PrivateWs => "Memory",
            Col::Disk => "Disk",
            Col::Net => "Network",
            Col::Io => "I/O",
            Col::Gpu => "GPU",
            Col::Threads => "Threads",
            Col::Handles => "Handles",
            Col::HandleTrend => "Handles/min",
            Col::Priority => "Priority",
            Col::User => "User",
            Col::Integrity => "Integrity",
            Col::Arch => "Arch",
            Col::Session => "Session",
            Col::Description => "Description",
            // On Linux the package a program came with.
            Col::Company => {
                if cfg!(windows) {
                    "Company"
                } else {
                    "Package"
                }
            }
            Col::Version => "Version",
            Col::Path => "Path",
            Col::CommandLine => "Command line",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Col::Cpu => "Share of the whole machine. One saturated core is 100 / logical CPUs.",
            Col::PrivateWs => {
                "Private working set: physical RAM only this process uses. Task Manager calls this \"Memory\"."
            }
            Col::Private => {
                "Private committed memory, resident or paged out. What a leak grows; what the commit limit counts."
            }
            Col::WorkingSet => {
                "Physical memory currently mapped by the process, shared pages included."
            }
            Col::Disk => "Physical disk reads + writes attributed to the process (needs admin).",
            Col::Io => "All I/O the process issues: files, devices, pipes and network.",
            Col::Net => {
                "TCP/UDP bytes sent + received per second (kernel network trace, needs admin)."
            }
            Col::Gpu => "Busiest GPU engine the process uses, across all GPUs (like Task Manager).",
            Col::HandleTrend => {
                "Smoothed change of the handle count per minute. A steady positive value is a leak."
            }
            _ => "",
        }
    }

    pub fn numeric(self) -> bool {
        matches!(
            self,
            Col::Pid
                | Col::Cpu
                | Col::CpuTime
                | Col::Private
                | Col::WorkingSet
                | Col::PrivateWs
                | Col::Disk
                | Col::Net
                | Col::Io
                | Col::Gpu
                | Col::Threads
                | Col::Handles
                | Col::HandleTrend
                | Col::Session
        )
    }

    /// Numbers read best largest-first; text A to Z.
    pub fn default_descending(self) -> bool {
        self.numeric() && self != Col::Pid && self != Col::Session
    }

    pub fn initial_width(self) -> f32 {
        match self {
            Col::Name => 250.0,
            Col::Pid | Col::Session | Col::Arch => 56.0,
            Col::Cpu | Col::Gpu => 62.0,
            Col::Threads | Col::Handles => 66.0,
            Col::HandleTrend => 84.0,
            Col::Private
            | Col::WorkingSet
            | Col::PrivateWs
            | Col::Disk
            | Col::Net
            | Col::Io
            | Col::CpuTime => 92.0,
            Col::Priority | Col::Integrity => 90.0,
            Col::Version => 110.0,
            Col::User => 170.0,
            Col::Company => 170.0,
            Col::Description => 230.0,
            Col::Path | Col::CommandLine => 360.0,
        }
    }
}

/// How much time a history graph spans.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum GraphSpan {
    /// Stretch over what has been collected so far, up to the full history.
    #[default]
    Fit,
    /// A fixed window: new samples flow in from the right.
    Minute,
    FiveMinutes,
    TenMinutes,
}

impl GraphSpan {
    pub const ALL: [GraphSpan; 4] = [
        GraphSpan::Fit,
        GraphSpan::Minute,
        GraphSpan::FiveMinutes,
        GraphSpan::TenMinutes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GraphSpan::Fit => "Fit",
            GraphSpan::Minute => "1 min",
            GraphSpan::FiveMinutes => "5 min",
            GraphSpan::TenMinutes => "10 min",
        }
    }

    pub fn seconds(self) -> Option<u64> {
        match self {
            GraphSpan::Fit => None,
            GraphSpan::Minute => Some(60),
            GraphSpan::FiveMinutes => Some(300),
            GraphSpan::TenMinutes => Some(600),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Tab {
    Overview,
    Processes,
    Performance,
    History,
    Rules,
    Services,
    Startup,
    Stutters,
    Network,
    Activity,
    Mcp,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum View {
    /// Parent/child tree.
    Tree,
    /// Every process on its own, sorted globally.
    List,
    /// Grouped by executable with totals, like Task Manager's apps.
    Apps,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Tree => "Tree",
            View::List => "List",
            View::Apps => "Apps",
        }
    }

    pub fn next(self) -> Self {
        match self {
            View::Tree => View::List,
            View::List => View::Apps,
            View::Apps => View::Tree,
        }
    }
}

/// What Anabolic does about newer releases on GitHub.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Updates {
    /// Downloads and installs them; they run from the next start.
    #[default]
    Install,
    /// Says one is out, and installs it when asked.
    Ask,
    /// Does not look (a check from Settings still does).
    Off,
}

impl Updates {
    pub const ALL: [Updates; 3] = [Updates::Install, Updates::Ask, Updates::Off];

    pub fn label(self) -> &'static str {
        match self {
            Updates::Install => "Install automatically",
            Updates::Ask => "Ask first",
            Updates::Off => "Don't check",
        }
    }
}

/// The keys that bring Anabolic to the front from any program, while it
/// runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Hotkey {
    Off,
    #[default]
    CtrlAltEsc,
    CtrlShiftAltEsc,
    CtrlShiftF12,
}

impl Hotkey {
    pub const ALL: [Hotkey; 4] = [
        Hotkey::Off,
        Hotkey::CtrlAltEsc,
        Hotkey::CtrlShiftAltEsc,
        Hotkey::CtrlShiftF12,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Hotkey::Off => "Off",
            Hotkey::CtrlAltEsc => "Ctrl+Alt+Esc",
            Hotkey::CtrlShiftAltEsc => "Ctrl+Shift+Alt+Esc",
            Hotkey::CtrlShiftF12 => "Ctrl+Shift+F12",
        }
    }

    /// Modifiers and virtual key, as Windows registers them.
    pub fn keys(self) -> Option<(u32, u32)> {
        use anabolic_core::hotkey::{MOD_ALT, MOD_CONTROL, MOD_SHIFT};
        const ESCAPE: u32 = 0x1B;
        const F12: u32 = 0x7B;
        match self {
            Hotkey::Off => None,
            Hotkey::CtrlAltEsc => Some((MOD_CONTROL | MOD_ALT, ESCAPE)),
            Hotkey::CtrlShiftAltEsc => Some((MOD_CONTROL | MOD_SHIFT | MOD_ALT, ESCAPE)),
            Hotkey::CtrlShiftF12 => Some((MOD_CONTROL | MOD_SHIFT, F12)),
        }
    }
}

/// The MCP server: off until turned on on the MCP page.
/// What MCP clients may do, chosen on the MCP page when the server is
/// turned on. Each kind is allowed or not; nothing waits for an answer in
/// Anabolic.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum McpKind {
    /// Read what the PC is doing: processes, performance, history,
    /// network, services, startup programs.
    Read,
    /// Turn a kernel trace on for a while (who runs on each core, which
    /// files each disk serves, frame times): some CPU while it runs.
    Trace,
    /// Open a process to look inside: handles, modules, thread stacks,
    /// wait chains. Each call also needs force=true after a warning.
    Inspect,
    /// Write a memory dump of a process. Also needs force=true.
    Dump,
    /// Record what processes do, step by step (files, registry, network,
    /// programs started), from a kernel trace.
    Record,
    /// End processes.
    End,
    /// Suspend and resume processes, change priority, cores, efficiency mode.
    Control,
    /// Start, stop, pause and restart services, change how they start.
    Services,
    /// Close connections; block programs from the network and allow them
    /// again (Windows Firewall rules).
    Network,
    /// Turn startup programs on and off.
    Startup,
}

impl McpKind {
    /// "read,trace,end" or "all" (a command line's list); unknown names
    /// are left out.
    pub fn parse_list(list: &str) -> Vec<McpKind> {
        if list.trim().eq_ignore_ascii_case("all") {
            return McpKind::ALL.to_vec();
        }
        list.split(',')
            .filter_map(|name| {
                McpKind::ALL
                    .into_iter()
                    .find(|k| format!("{k:?}").eq_ignore_ascii_case(name.trim()))
            })
            .collect()
    }

    pub const ALL: [McpKind; 10] = [
        McpKind::Read,
        McpKind::Trace,
        McpKind::Inspect,
        McpKind::Dump,
        McpKind::Record,
        McpKind::End,
        McpKind::Control,
        McpKind::Services,
        McpKind::Network,
        McpKind::Startup,
    ];

    pub fn label(self) -> &'static str {
        match self {
            McpKind::Read => "Read what the PC is doing",
            McpKind::Trace => "Turn kernel traces on for a while",
            McpKind::Inspect => "Look inside a process",
            McpKind::Dump => "Write memory dumps",
            McpKind::Record => "Record what processes do",
            McpKind::End => "End processes",
            McpKind::Control => "Suspend, resume and reprioritise processes",
            McpKind::Services => "Control services",
            McpKind::Network => "Close connections, block programs",
            McpKind::Startup => "Turn startup programs on and off",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            McpKind::Read => {
                "Processes, CPU, memory, disks, GPUs, network, history, services, startup \
                 programs. Nothing is opened or changed."
            }
            McpKind::Trace => {
                "Who runs on each core, which files each disk serves, every connection, frame \
                 times and driver latency: a few seconds of kernel tracing, some CPU meanwhile."
            }
            McpKind::Inspect => {
                "Handles, modules, thread stacks and wait chains. The process is opened, which \
                 anti-cheat software can take badly: every call is refused until the client \
                 repeats it with force=true, after its user agreed to the risk it was told."
            }
            McpKind::Dump => {
                "All of a process' memory written to a file (passwords and keys it holds \
                 included). Also needs force=true after the warning; never for lsass."
            }
            McpKind::Record => {
                "Every file, registry key, connection and program a process touches, from a \
                 kernel trace: nothing is opened, but recordings can hold private paths."
            }
            McpKind::End => "Ends processes, and the processes under them when asked.",
            McpKind::Control => {
                "Suspends and resumes processes, sets priority, cores and efficiency mode."
            }
            McpKind::Services => {
                "Starts, stops, pauses and restarts services; sets their startup type."
            }
            McpKind::Network => {
                "Closes TCP connections; blocks a program with Windows Firewall rules and \
                 removes them."
            }
            McpKind::Startup => {
                "Turns Run keys, Startup folder entries and sign-in tasks on and off."
            }
        }
    }
}

/// The MCP server: off until turned on on the MCP page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
    /// Every request carries it (Authorization: Bearer); made when the
    /// server is first turned on.
    pub token: String,
    /// What clients may do.
    pub allowed: Vec<McpKind>,
}

impl McpSettings {
    pub fn allows(&self, kind: McpKind) -> bool {
        self.allowed.contains(&kind)
    }
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 47123,
            token: String::new(),
            allowed: vec![McpKind::Read],
        }
    }
}

fn unversioned() -> u32 {
    0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(default = "unversioned")]
    pub version: u32,
    pub interval_ms: u64,
    pub view: View,
    pub sort: Col,
    pub descending: bool,
    pub columns: Vec<Col>,
    pub confirm_kill: bool,
    pub tab: Tab,
    pub show_details: bool,
    /// Warn when physical memory or commit use crosses this percentage.
    pub memory_alert_pct: f32,
    /// The navigation rail shows icons only.
    pub nav_collapsed: bool,
    pub graph_span: GraphSpan,
    /// Keep the flight recorder's history.
    pub record_history: bool,
    /// Days of history kept.
    pub history_days: u32,
    pub rules: Vec<crate::rules::Rule>,
    /// Closing the window keeps Anabolic running in the notification area,
    /// recording the history and applying rules.
    pub close_to_tray: bool,
    /// The keys that bring Anabolic to the front from any program.
    pub hotkey: Hotkey,
    pub mcp: McpSettings,
    /// Frame times and driver timing are traced with the Stutters page
    /// closed too, so the stutters of a game played meanwhile are caught.
    pub watch_stutters: bool,
    /// Memory dumps of lsass.exe, which holds the sign-in secrets, may be
    /// written (from the process menu only, never by MCP clients). Off: an
    /// antivirus takes reading it for credential theft.
    pub dump_sign_in_secrets: bool,
    pub game: crate::gamemode::GameModeSettings,
    /// Programs Anabolic cut off the network (Windows Firewall rules), by
    /// path.
    pub blocked: Vec<String>,
    pub updates: Updates,
    /// The version that ran last: what changed since is shown once (see
    /// `whats_new`). None from before there was this.
    pub last_version: Option<String>,
    /// Warn when one program runs in this many copies from one starter
    /// (see `runaway`); 0 never.
    pub runaway_at: u32,
    /// Programs never warned about for that, by lowercase name.
    pub runaway_ignored: Vec<String>,
    /// Ports to keep an eye on (Network > Development ports, the Overview,
    /// Settings): free, or who holds them.
    pub watched_ports: Vec<PortWatch>,
    /// The notification area icon shows how busy the CPU is.
    pub tray_cpu: bool,
    /// What Run new task ran lately, the latest first.
    pub run_history: Vec<String>,
    /// Run new task asks for administrator rights.
    pub run_elevated: bool,
    /// The tables' columns: the order their titles were dragged into and
    /// the widths they were given, by table. The process list keeps its
    /// order in `columns`, and its widths here.
    pub tables: std::collections::BTreeMap<String, TableLayout>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            interval_ms: 1000,
            view: View::Tree,
            sort: Col::Cpu,
            descending: true,
            columns: vec![
                Col::Name,
                Col::Pid,
                Col::Cpu,
                Col::PrivateWs,
                Col::Private,
                Col::Disk,
                Col::Net,
                Col::Gpu,
                Col::Threads,
                Col::Handles,
                Col::User,
                Col::Description,
            ],
            confirm_kill: true,
            tab: Tab::Processes,
            show_details: true,
            memory_alert_pct: 90.0,
            nav_collapsed: false,
            graph_span: GraphSpan::Fit,
            record_history: true,
            history_days: 7,
            rules: Vec::new(),
            close_to_tray: false,
            hotkey: Hotkey::default(),
            mcp: McpSettings::default(),
            watch_stutters: false,
            dump_sign_in_secrets: false,
            game: Default::default(),
            blocked: Vec::new(),
            updates: Updates::default(),
            last_version: None,
            runaway_at: crate::runaway::DEFAULT_AT,
            runaway_ignored: Vec::new(),
            watched_ports: Vec::new(),
            tray_cpu: true,
            run_history: Vec::new(),
            run_elevated: false,
            tables: Default::default(),
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        if self.version < 2 {
            // v2 made "Memory" (private working set) a default column.
            self.columns = Settings::default().columns;
        }
        if self.version < 3 && !self.columns.contains(&Col::Gpu) {
            // v3 added the GPU column; slot it in without touching the rest.
            let at = self
                .columns
                .iter()
                .position(|&c| c == Col::Io)
                .map_or(self.columns.len(), |i| i + 1);
            self.columns.insert(at, Col::Gpu);
        }
        if self.version < 4 && !self.columns.contains(&Col::Net) {
            // v4 added the network column.
            let at = self
                .columns
                .iter()
                .position(|&c| c == Col::Disk)
                .map_or(self.columns.len(), |i| i + 1);
            self.columns.insert(at, Col::Net);
        }
        self.version = VERSION;
        self.interval_ms = self.interval_ms.clamp(100, 60_000);
        self.memory_alert_pct = self.memory_alert_pct.clamp(50.0, 99.0);
        self.history_days = self.history_days.clamp(1, 90);
        if self.runaway_at != 0 {
            self.runaway_at = self.runaway_at.clamp(10, 1000);
        }
        self.watched_ports.retain(|w| w.port != 0);
        self.watched_ports.sort_by_key(|w| w.port);
        self.watched_ports.dedup_by_key(|w| w.port);
        for w in &mut self.watched_ports {
            w.name = w.name.trim().chars().take(PORT_NAME).collect();
        }
        self.run_history.truncate(RUN_HISTORY);
        while self.tables.len() > TABLES {
            self.tables.pop_last();
        }
        for t in self.tables.values_mut() {
            t.order.truncate(TABLE_COLUMNS);
            t.widths.truncate(TABLE_COLUMNS);
            for w in &mut t.widths {
                *w = if w.is_finite() {
                    w.clamp(0.0, 4000.0)
                } else {
                    0.0
                };
            }
        }
        // Columns dragged into another order: a column twice is not
        // necessarily next to itself.
        let mut seen = std::collections::HashSet::new();
        self.columns.retain(|&c| seen.insert(c));
        if !self.columns.contains(&Col::Name) {
            self.columns.insert(0, Col::Name);
        }
    }
}
