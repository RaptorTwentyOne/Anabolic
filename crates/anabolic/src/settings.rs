//! User preferences, persisted by eframe between runs.

use serde::{Deserialize, Serialize};

/// Bumped when stored settings need migrating.
const VERSION: u32 = 4;

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
            Col::Company => "Company",
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
    Processes,
    Performance,
    History,
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
        self.columns.dedup();
        if !self.columns.contains(&Col::Name) {
            self.columns.insert(0, Col::Name);
        }
    }
}
