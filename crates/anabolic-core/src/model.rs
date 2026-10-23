// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Plain data published by the collector. Everything here is immutable once a
//! snapshot is built, so the UI can read it without any locking.

use std::sync::Arc;
use std::time::Duration;

use crate::gpu::AdapterGpuStats;
use crate::history::History;
use crate::net::NetStats;
use crate::nettrace::Proto;

/// Identifies one process instance. PIDs get reused; (pid, create time) does not.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ProcKey {
    pub pid: u32,
    pub create_time: i64,
}

/// Real PIDs are multiples of four, so this can never collide with one.
pub const INTERRUPTS_PID: u32 = 0xFFFF_FFFE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcState {
    Running,
    /// Every thread is suspended (manually, by a debugger or by PLM for UWP apps).
    Suspended,
    /// Gone since the last refresh; kept for a moment so it can be highlighted.
    Exited,
}

/// Which process information class the kernel agreed to give us.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InfoLevel {
    /// SystemFullProcessInformation: adds disk counters and user SIDs (admin only).
    Full,
    /// SystemExtendedProcessInformation: adds thread stack/Win32 start addresses.
    Extended,
    Basic,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct IoCounters {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub other_bytes: u64,
    pub read_ops: u64,
    pub write_ops: u64,
    pub other_ops: u64,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct DiskCounters {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub read_ops: u64,
    pub write_ops: u64,
    pub flush_ops: u64,
}

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub key: ProcKey,
    pub pid: u32,
    pub ppid: u32,
    pub name: Arc<str>,
    pub session_id: u32,
    pub state: ProcState,
    /// Snapshot sequence number this process was first seen at (0 = already
    /// running when Anabolic started).
    pub first_seen: u64,
    /// Snapshot sequence number this process disappeared at, for `Exited`.
    pub exited_at: u64,
    pub base_priority: i32,
    pub threads: u32,
    pub handles: u32,

    /// Share of total machine CPU capacity, 0..=100.
    pub cpu: f32,
    pub cpu_kernel: f32,
    /// Kernel + user time, in 100 ns units.
    pub cpu_time: u64,
    pub cycles: u64,
    pub cycles_delta: u64,

    pub private_bytes: u64,
    pub working_set: u64,
    pub private_working_set: u64,
    pub peak_working_set: u64,
    pub virtual_size: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub page_faults: u32,
    pub page_faults_delta: u32,
    pub hard_faults: u32,

    /// All I/O issued by the process (files, devices, network), cumulative.
    pub io: IoCounters,
    /// Bytes per second over the last interval.
    pub io_read_rate: f64,
    pub io_write_rate: f64,
    pub io_other_rate: f64,

    /// Physical disk I/O attributed to the process. Needs `InfoLevel::Full`.
    pub disk: Option<DiskCounters>,
    pub disk_read_rate: f64,
    pub disk_write_rate: f64,

    pub context_switches_delta: u64,
    /// Smoothed change of the handle count per minute; a steady positive
    /// value is a leak.
    pub handles_per_min: f32,
    /// Busiest GPU engine share, 0..=100 (sampled on its own thread).
    pub gpu: f32,
    /// Network bytes per second and totals since Anabolic started (ETW, admin).
    pub net_rx_rate: f64,
    pub net_tx_rate: f64,
    pub net_rx_total: u64,
    pub net_tx_total: u64,
    pub user_sid: Option<Arc<[u8]>>,
    /// One of its windows has taken no message for five seconds.
    pub not_responding: bool,
    /// Ending has begun and not finished: it has no threads left and
    /// stays while something holds it (see `ending::why_alive`).
    pub ending: bool,
}

impl ProcessInfo {
    /// A process with no activity; used for pseudo processes and tests.
    pub fn blank(key: ProcKey, ppid: u32, name: Arc<str>) -> Self {
        Self {
            key,
            pid: key.pid,
            ppid,
            name,
            session_id: 0,
            state: ProcState::Running,
            first_seen: 0,
            exited_at: 0,
            base_priority: 8,
            threads: 0,
            handles: 0,
            cpu: 0.0,
            cpu_kernel: 0.0,
            cpu_time: 0,
            cycles: 0,
            cycles_delta: 0,
            private_bytes: 0,
            working_set: 0,
            private_working_set: 0,
            peak_working_set: 0,
            virtual_size: 0,
            paged_pool: 0,
            nonpaged_pool: 0,
            page_faults: 0,
            page_faults_delta: 0,
            hard_faults: 0,
            io: IoCounters::default(),
            io_read_rate: 0.0,
            io_write_rate: 0.0,
            io_other_rate: 0.0,
            disk: None,
            disk_read_rate: 0.0,
            disk_write_rate: 0.0,
            context_switches_delta: 0,
            handles_per_min: 0.0,
            gpu: 0.0,
            net_rx_rate: 0.0,
            net_tx_rate: 0.0,
            net_rx_total: 0,
            net_tx_total: 0,
            user_sid: None,
            not_responding: false,
            ending: false,
        }
    }

    pub fn is_pseudo(&self) -> bool {
        self.pid == 0 || self.pid == INTERRUPTS_PID
    }

    pub fn io_total_rate(&self) -> f64 {
        self.io_read_rate + self.io_write_rate + self.io_other_rate
    }

    pub fn net_total_rate(&self) -> f64 {
        self.net_rx_rate + self.net_tx_rate
    }

    pub fn disk_total_rate(&self) -> f64 {
        self.disk_read_rate + self.disk_write_rate
    }
}

#[derive(Clone, Debug)]
pub struct ThreadInfo {
    pub tid: u32,
    pub create_time: i64,
    pub start_address: u64,
    pub win32_start_address: u64,
    pub priority: i32,
    pub base_priority: i32,
    pub state: u32,
    pub wait_reason: u32,
    pub cpu: f32,
    pub kernel_time: u64,
    pub user_time: u64,
    pub context_switches: u32,
    pub context_switches_delta: u32,
    pub stack_base: u64,
    pub stack_limit: u64,
    pub teb: u64,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct CpuTimes {
    /// Percentages of the measured interval, 0..=100.
    pub total: f32,
    pub kernel: f32,
    pub user: f32,
    pub dpc: f32,
    pub interrupt: f32,
    /// Hardware interrupts per second.
    pub interrupt_rate: f32,
    pub context_switch_rate: f32,
    /// Deferred procedure calls per second.
    pub dpc_rate: f32,
    /// Effective clock in MHz (0 when the processor idled throughout).
    pub mhz: f32,
}

/// A thread (or, with `tid` 0 in `CoreActivity::processes`, a process) and
/// its share of one logical processor.
#[derive(Clone, Debug)]
pub struct CoreUser {
    pub tid: u32,
    pub pid: u32,
    pub name: Arc<str>,
    /// Percent of the interval spent on this processor.
    pub share: f32,
}

/// Who ran on one logical processor during the last interval.
#[derive(Clone, Debug, Default)]
pub struct CoreActivity {
    /// Busiest first.
    pub threads: Vec<CoreUser>,
    pub processes: Vec<CoreUser>,
}

/// Disk traffic, smoothed over a few seconds.
#[derive(Clone, Copy, Default, Debug)]
pub struct DiskUse {
    pub read_rate: f64,
    pub write_rate: f64,
    /// Average completion time of recent requests, milliseconds.
    pub response_ms: f32,
    /// Since tracing started.
    pub read_total: u64,
    pub write_total: u64,
}

impl DiskUse {
    pub fn rate(&self) -> f64 {
        self.read_rate + self.write_rate
    }
}

/// One process's traffic on one physical disk.
#[derive(Clone, Debug)]
pub struct DiskUser {
    /// `u32::MAX` when the issuing thread could not be traced to a process.
    pub pid: u32,
    pub name: Arc<str>,
    pub io: DiskUse,
}

/// Traffic to one file on one physical disk, by one process.
#[derive(Clone, Debug)]
pub struct DiskFile {
    /// DOS path where there is one, else the kernel's name or a description.
    pub path: Arc<str>,
    pub pid: u32,
    pub process: Arc<str>,
    pub io: DiskUse,
}

/// Who reads and writes one physical disk; busiest first.
#[derive(Clone, Debug, Default)]
pub struct DiskActivity {
    pub disk: u32,
    pub processes: Vec<DiskUser>,
    pub files: Vec<DiskFile>,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct MemoryLists {
    pub zeroed: u64,
    pub free: u64,
    pub modified: u64,
    pub standby: u64,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct MemoryStats {
    pub total: u64,
    pub available: u64,
    pub commit: u64,
    /// 0 when no limit is enforced: Linux overcommits unless told not to
    /// (vm.overcommit_memory = 2).
    pub commit_limit: u64,
    pub commit_peak: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub system_cache: u64,
    /// Needs SeProfileSingleProcessPrivilege (admin).
    pub lists: Option<MemoryLists>,
}

impl MemoryStats {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemStats {
    pub cpu: CpuTimes,
    pub cores: Vec<CpuTimes>,
    pub memory: MemoryStats,
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
    pub io_read_rate: f64,
    pub io_write_rate: f64,
    pub io_other_rate: f64,
    pub disk_read_rate: f64,
    pub disk_write_rate: f64,
    pub uptime: Duration,
    /// Process objects in the kernel; anything above `processes` is an exited
    /// process kept alive by an open handle (a zombie).
    pub process_objects: u32,
    pub net: NetStats,
    pub gpus: Vec<AdapterGpuStats>,
    /// Temperatures, fans and power draw, grouped by device, the
    /// processor's first.
    pub sensors: Vec<crate::sensors::Sensor>,
    /// °C: the processor's package (Intel) or control temperature (AMD).
    /// None where only drivers may read it (Windows).
    pub cpu_temperature: Option<f32>,
    /// Why per-process network traffic is unavailable, if it is.
    pub net_trace_error: Option<String>,
    /// Per logical processor; empty unless core tracing is on.
    pub core_activity: Vec<CoreActivity>,
    pub core_trace_error: Option<String>,
    pub disks: Vec<crate::disk::DiskStats>,
    /// Per physical disk; empty unless disk tracing is on.
    pub disk_activity: Vec<DiskActivity>,
    /// Disk tracing is on (`disk_activity` may still be empty: idle disks).
    pub disk_tracing: bool,
    pub disk_trace_error: Option<String>,
    pub page_files: Vec<crate::hwinfo::PageFile>,
    /// Windows that are not responding, one per thread.
    pub hung: Vec<crate::waits::HungWindow>,
    /// Frame times of the programs drawing; None unless frame tracing is on.
    pub frames: Option<Arc<crate::frames::FrameStats>>,
    pub frame_trace_error: Option<String>,
    /// DPC and ISR times per driver; None unless latency tracing is on.
    pub latency: Option<Arc<crate::latency::LatencyStats>>,
    pub latency_trace_error: Option<String>,
    /// Every connection and listening port; None unless asked for.
    pub net_activity: Option<Arc<NetActivity>>,
    /// The name programs looked up for each address (forward DNS: what
    /// they asked for, not what the address says of itself).
    pub dns_names: Arc<std::collections::HashMap<std::net::IpAddr, Arc<str>>>,
    /// The names looked up lately, oldest first.
    pub dns_lookups: Arc<Vec<crate::nettrace::DnsLookup>>,
}

#[derive(Clone, Debug)]
pub struct SelectedThreads {
    pub key: ProcKey,
    pub threads: Vec<ThreadInfo>,
    /// Endpoints the process exchanged traffic with recently.
    pub connections: Vec<Connection>,
    /// Its parent, grandparent and so on, as far as this session knows.
    pub ancestry: Vec<crate::lineage::Ancestor>,
    /// Its own start record (image path, elevation), when seen.
    pub origin: Option<crate::lineage::Ancestor>,
}

#[derive(Clone, Debug)]
pub struct Connection {
    /// The process it belongs to.
    pub pid: u32,
    pub proto: Proto,
    pub remote: std::net::IpAddr,
    pub remote_port: u16,
    pub local_port: u16,
    /// From Windows' TCP table; None for UDP, and when only traffic was seen.
    pub state: Option<crate::sockets::TcpState>,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    /// Refreshes since the last packet.
    pub idle: u64,
    /// Traffic was seen on it since Anabolic started watching; an open
    /// connection that carried nothing since has none.
    pub traffic: bool,
}

/// A port open for others to connect to: a listening TCP port, or a bound
/// UDP one.
#[derive(Clone, Debug)]
pub struct Listener {
    pub pid: u32,
    pub proto: Proto,
    /// 0.0.0.0 or :: for every network, a loopback address for this
    /// computer only, else that one network.
    pub local: std::net::IpAddr,
    pub port: u16,
}

/// Everything on the network, machine-wide: published while the Network
/// page is open.
#[derive(Clone, Debug, Default)]
pub struct NetActivity {
    /// Open TCP connections (with or without traffic), and the endpoints
    /// traffic was seen on lately (UDP, and TCP since closed).
    pub connections: Vec<Connection>,
    pub listeners: Vec<Listener>,
    /// A socket table that could not be read.
    pub error: Option<String>,
}

/// One refresh worth of data.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub seq: u64,
    /// Wall-clock length of the interval the rates were computed over.
    pub interval: Duration,
    /// How long the collector spent producing this snapshot.
    pub collect_time: Duration,
    pub info_level: InfoLevel,
    /// Sorted by PID. Includes exited ghosts and the Interrupts pseudo process.
    pub processes: Vec<ProcessInfo>,
    pub system: SystemStats,
    pub history: Arc<History>,
    pub selected: Option<SelectedThreads>,
    pub own_pid: u32,
}

impl Snapshot {
    pub fn empty(cores: usize, history: Arc<History>) -> Self {
        Self {
            seq: 0,
            interval: Duration::ZERO,
            collect_time: Duration::ZERO,
            info_level: InfoLevel::Basic,
            processes: Vec::new(),
            system: SystemStats {
                cores: vec![CpuTimes::default(); cores],
                ..Default::default()
            },
            history,
            selected: None,
            own_pid: std::process::id(),
        }
    }

    pub fn find(&self, key: ProcKey) -> Option<&ProcessInfo> {
        self.processes
            .binary_search_by(|p| p.key.cmp(&key))
            .ok()
            .map(|i| &self.processes[i])
    }
}
