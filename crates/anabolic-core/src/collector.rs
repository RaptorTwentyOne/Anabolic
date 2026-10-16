//! The collector thread samples everything once per tick and publishes an
//! immutable [`Snapshot`]. Publishing is a lock-free pointer swap: the UI never
//! waits on the collector and the collector never waits on the UI.

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::THREAD_PRIORITY_ABOVE_NORMAL;

use crate::coretrace::CoreTracer;
use crate::cpu::CpuSampler;
use crate::disk::DiskSampler;
use crate::disktrace::DiskTracer;
use crate::diskusage::DiskUsage;
use crate::enrich::{self, EnrichRequest, Enriched};
use crate::frames::FrameTracer;
use crate::gpu::{self, GpuProcessMap, GpuSampler};
use crate::hardening::set_current_thread_priority;
use crate::history::History;
use crate::hwinfo::{PageFile, page_files};
use crate::latency::LatencyTracer;
use crate::memory::MemorySampler;
use crate::model::*;
use crate::net::NetSampler;
use crate::nettrace::{ConnKey, NetTracer, Traffic};
use crate::recorder::{Recorder, RecorderConfig};
use crate::scan::{Entry, ProcessScanner};

#[derive(Clone, Debug)]
pub struct CollectorConfig {
    pub interval: Duration,
    pub history_len: usize,
    /// How many refreshes new and exited processes stay highlighted.
    pub highlight_ticks: u64,
    /// Where the flight recorder keeps its days; None records nothing.
    pub recorder: Option<RecorderConfig>,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            history_len: 600,
            highlight_ticks: 2,
            recorder: None,
        }
    }
}

#[derive(Debug)]
pub enum Command {
    SetInterval(Duration),
    RefreshNow,
    /// Collect per-thread data for this process (or stop doing so).
    Select(Option<ProcKey>),
    /// Trace context switches to tell who runs on each core (costly; only
    /// while the CPU view is open).
    TraceCores(bool),
    /// Trace disk I/O to tell who reads and writes each disk (only while a
    /// disk view is open).
    TraceDisks(bool),
    /// Start or stop the flight recorder.
    Record(Option<RecorderConfig>),
    /// Follow the frames of the programs that draw (games): present events
    /// from the graphics runtimes, no handle to any of them.
    TraceFrames(bool),
    /// Time every DPC and ISR per driver (a kernel trace; only while the
    /// latency view is open).
    TraceLatency(bool),
    /// Publish every connection and listening port (the Network page).
    NetActivity(bool),
    Shutdown,
}

pub struct Collector {
    latest: Arc<ArcSwap<Snapshot>>,
    commands: Sender<Command>,
    details: Receiver<Enriched>,
    /// Signalled once the collector has stopped (and its ETW session with it).
    done: Receiver<()>,
}

impl Collector {
    /// Starts the collector and enrichment threads. `notify` runs after every
    /// published snapshot (the UI passes its repaint request here).
    pub fn start(config: CollectorConfig, notify: impl Fn() + Send + 'static) -> Self {
        let cpu = CpuSampler::new();
        let gpu = GpuSampler::new();
        let history = History::new(config.history_len, cpu.core_count(), &gpu.layout());
        let latest = Arc::new(ArcSwap::from_pointee(Snapshot::empty(
            cpu.core_count(),
            Arc::new(history.clone()),
        )));
        let gpu_procs = Arc::new(ArcSwap::from_pointee(GpuProcessMap::new()));
        let interval_ms = Arc::new(AtomicU64::new(config.interval.as_millis() as u64));
        gpu::spawn_process_sampler(&gpu, gpu_procs.clone(), interval_ms.clone());
        let (commands, command_rx) = unbounded();
        let (details_tx, details) = unbounded();
        let enrich = enrich::spawn(details_tx);

        let publish = latest.clone();
        let (done_tx, done) = crossbeam_channel::bounded(1);
        thread::Builder::new()
            .name("anabolic-collector".into())
            .spawn(move || {
                set_current_thread_priority(THREAD_PRIORITY_ABOVE_NORMAL);
                let mut state = State::new(
                    config,
                    Samplers { cpu, gpu, history },
                    enrich,
                    gpu_procs,
                    interval_ms,
                );
                state.run(&command_rx, &publish, &notify);
                drop(state);
                let _ = done_tx.send(());
            })
            .expect("failed to spawn the collector thread");

        Self {
            latest,
            commands,
            details,
            done,
        }
    }

    /// Stops the collector and waits (briefly) for it, so the ETW session is
    /// not left running after the process exits.
    pub fn shutdown(&self) {
        if self.commands.send(Command::Shutdown).is_ok() {
            let _ = self.done.recv_timeout(Duration::from_secs(2));
        }
    }

    /// The most recent snapshot. Wait-free.
    pub fn latest(&self) -> Arc<Snapshot> {
        self.latest.load_full()
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Process details trickling in from the enrichment worker.
    pub fn details(&self) -> &Receiver<Enriched> {
        &self.details
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
struct ConnState {
    rx_total: u64,
    tx_total: u64,
    rx_rate: f64,
    tx_rate: f64,
    last_seq: u64,
}

#[derive(Clone, Copy, Default)]
struct Previous {
    cpu_time: u64,
    kernel_time: u64,
    cycles: u64,
    io: IoCounters,
    disk: Option<DiskCounters>,
    context_switches: u64,
    page_faults: u32,
    handles: u32,
}

struct Tracked {
    name: Arc<str>,
    sid: Option<Arc<[u8]>>,
    first_seen: u64,
    last_seen: u64,
    exited_at: Option<u64>,
    prev: Previous,
    /// `prev` holds a real observation (false right after insertion).
    observed: bool,
    /// Smoothed handle count change, per minute.
    handle_rate: f32,
    net_rx_total: u64,
    net_tx_total: u64,
}

struct Samplers {
    cpu: CpuSampler,
    gpu: GpuSampler,
    history: History,
}

struct State {
    config: CollectorConfig,
    seq: u64,
    scanner: ProcessScanner,
    cpu: CpuSampler,
    memory: MemorySampler,
    net: NetSampler,
    disk: DiskSampler,
    page_files: Vec<PageFile>,
    gpu: GpuSampler,
    /// Per-process GPU share from the GPU thread.
    gpu_procs: Arc<ArcSwap<GpuProcessMap>>,
    /// Shared with the GPU thread so it samples at the same pace.
    interval_ms: Arc<AtomicU64>,
    net_trace: Option<NetTracer>,
    net_trace_error: Option<String>,
    conns: HashMap<ConnKey, ConnState>,
    core_tracer: Option<CoreTracer>,
    core_trace_error: Option<String>,
    disk_tracer: Option<DiskTracer>,
    disk_trace_error: Option<String>,
    disk_usage: DiskUsage,
    frame_tracer: Option<FrameTracer>,
    frame_trace_error: Option<String>,
    latency_tracer: Option<LatencyTracer>,
    latency_trace_error: Option<String>,
    net_activity: bool,
    /// Address -> the name a program looked up for it.
    dns_names: Arc<HashMap<std::net::IpAddr, Arc<str>>>,
    dns_lookups: Arc<Vec<crate::nettrace::DnsLookup>>,
    /// What Windows' DNS cache holds, read now and then on a thread of its
    /// own: names for connections looked up before Anabolic started.
    dns_cache: Receiver<HashMap<std::net::IpAddr, Arc<str>>>,
    /// pid -> (image name, last seen) for processes the core trace saw start,
    /// so a process that lived for less than a refresh still has a name.
    traced_images: HashMap<u32, (Arc<str>, u64)>,
    /// tid -> (pid, last seen) while tracing cores or disks.
    thread_owners: HashMap<u32, (u32, u64)>,
    tracked: HashMap<ProcKey, Tracked>,
    history: History,
    published_history: Arc<History>,
    last_tick: Instant,
    previous: Option<Arc<Snapshot>>,
    selected: Option<ProcKey>,
    /// tid -> (create time, cpu time, context switches) for the selected process.
    thread_prev: HashMap<u32, (i64, u64, u32)>,
    thread_prev_for: Option<ProcKey>,
    enrich: Sender<EnrichRequest>,
    recorder: Option<Recorder>,
}

impl State {
    fn new(
        config: CollectorConfig,
        samplers: Samplers,
        enrich: Sender<EnrichRequest>,
        gpu_procs: Arc<ArcSwap<GpuProcessMap>>,
        interval_ms: Arc<AtomicU64>,
    ) -> Self {
        let Samplers { cpu, gpu, history } = samplers;
        let (net_trace, net_trace_error) = if !crate::hardening::is_elevated() {
            (None, Some("needs administrator rights".to_string()))
        } else {
            match NetTracer::start() {
                Ok(tracer) => (Some(tracer), None),
                Err(e) => (None, Some(e)),
            }
        };
        Self {
            net: NetSampler::default(),
            disk: DiskSampler::new(),
            page_files: Vec::new(),
            gpu,
            gpu_procs,
            interval_ms,
            net_trace,
            net_trace_error,
            conns: HashMap::new(),
            core_tracer: None,
            core_trace_error: None,
            disk_tracer: None,
            disk_trace_error: None,
            disk_usage: DiskUsage::default(),
            frame_tracer: None,
            frame_trace_error: None,
            latency_tracer: None,
            latency_trace_error: None,
            net_activity: false,
            dns_names: Arc::default(),
            dns_lookups: Arc::default(),
            dns_cache: {
                let (tx, rx) = crossbeam_channel::bounded(1);
                let _ = thread::Builder::new()
                    .name("anabolic-dnscache".into())
                    .spawn(move || {
                        set_current_thread_priority(
                            windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
                        );
                        while tx.send(crate::dnscache::names()).is_ok() {
                            thread::sleep(Duration::from_secs(120));
                        }
                    });
                rx
            },
            thread_owners: HashMap::new(),
            traced_images: HashMap::new(),
            seq: 0,
            scanner: ProcessScanner::new(),
            memory: MemorySampler::new(),
            tracked: HashMap::with_capacity(1024),
            published_history: Arc::new(history.clone()),
            history,
            last_tick: Instant::now(),
            previous: None,
            selected: None,
            thread_prev: HashMap::new(),
            thread_prev_for: None,
            enrich,
            cpu,
            recorder: config.recorder.clone().map(Recorder::start),
            config,
        }
    }

    fn trace_cores(&mut self, on: bool) {
        match (on, self.core_tracer.is_some()) {
            (true, false) => match CoreTracer::start(self.cpu.core_count()) {
                Ok(tracer) => {
                    self.core_tracer = Some(tracer);
                    self.core_trace_error = None;
                }
                Err(e) => self.core_trace_error = Some(e),
            },
            (false, true) => {
                self.core_tracer = None;
                self.traced_images.clear();
                if self.disk_tracer.is_none() {
                    self.thread_owners.clear();
                }
            }
            _ => {}
        }
    }

    fn trace_disks(&mut self, on: bool) {
        match (on, self.disk_tracer.is_some()) {
            (true, false) if !crate::hardening::is_elevated() => {
                self.disk_trace_error = Some("needs administrator rights".into());
            }
            (true, false) => match DiskTracer::start() {
                Ok(tracer) => {
                    self.disk_tracer = Some(tracer);
                    self.disk_trace_error = None;
                }
                Err(e) => self.disk_trace_error = Some(e),
            },
            (false, true) => {
                self.disk_tracer = None;
                self.disk_usage = DiskUsage::default();
                if self.core_tracer.is_none() {
                    self.thread_owners.clear();
                }
            }
            _ => {}
        }
    }

    fn trace_frames(&mut self, on: bool) {
        match (on, self.frame_tracer.is_some()) {
            (true, false) if !crate::hardening::is_elevated() => {
                self.frame_trace_error = Some("needs administrator rights".into());
            }
            (true, false) => match FrameTracer::start(true) {
                Ok(tracer) => {
                    self.frame_tracer = Some(tracer);
                    self.frame_trace_error = None;
                }
                Err(e) => self.frame_trace_error = Some(e),
            },
            (false, true) => self.frame_tracer = None,
            _ => {}
        }
    }

    fn trace_latency(&mut self, on: bool) {
        match (on, self.latency_tracer.is_some()) {
            (true, false) if !crate::hardening::is_elevated() => {
                self.latency_trace_error = Some("needs administrator rights".into());
            }
            (true, false) => match LatencyTracer::start() {
                Ok(tracer) => {
                    self.latency_tracer = Some(tracer);
                    self.latency_trace_error = None;
                }
                Err(e) => self.latency_trace_error = Some(e),
            },
            (false, true) => self.latency_tracer = None,
            _ => {}
        }
    }

    /// Turns per-processor samples into named shares. Samples only say who
    /// shared the busy time; the measured utilisation says how much there was.
    fn core_activity(
        &mut self,
        processes: &[ProcessInfo],
        cores: &[CpuTimes],
        seq: u64,
    ) -> Vec<CoreActivity> {
        let Some(tracer) = &self.core_tracer else {
            return Vec::new();
        };
        let taken = tracer.take();
        for (tid, pid) in taken.owners {
            self.thread_owners.insert(tid, (pid, seq));
        }
        for (pid, image) in taken.images {
            self.traced_images.insert(pid, (image, seq));
        }
        self.traced_images.retain(|_, (_, seen)| seq - *seen < 120);

        let unknown: Arc<str> = Arc::from("<unknown thread>");
        let idle: Arc<str> = Arc::from("Idle");
        let names: HashMap<u32, &ProcessInfo> = processes
            .iter()
            .filter(|p| p.state != ProcState::Exited)
            .map(|p| (p.pid, p))
            .collect();
        // Busiest first, idle always last: it is rarely the interesting part.
        let by_share = |a: &CoreUser, b: &CoreUser| {
            (a.pid == 0).cmp(&(b.pid == 0)).then(
                b.share
                    .partial_cmp(&a.share)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        };

        taken
            .per_cpu
            .into_iter()
            .enumerate()
            .map(|(cpu, samples)| {
                let utilization = cores.get(cpu).map_or(0.0, |c| c.total);
                let busy: u32 = samples
                    .iter()
                    .filter(|(tid, _)| *tid != 0)
                    .map(|(_, n)| n)
                    .sum();
                let mut threads: Vec<CoreUser> = samples
                    .iter()
                    .filter(|(tid, _)| *tid != 0 && busy > 0)
                    .map(|&(tid, n)| {
                        let pid = self.thread_owners.get(&tid).map(|&(pid, _)| pid);
                        let name = match pid {
                            Some(pid) => match names.get(&pid) {
                                Some(p) => p.name.clone(),
                                None => match self.traced_images.get(&pid) {
                                    Some((image, _)) => image.clone(),
                                    None => Arc::from(format!("<exited {pid}>")),
                                },
                            },
                            None => unknown.clone(),
                        };
                        CoreUser {
                            tid,
                            pid: pid.unwrap_or(u32::MAX),
                            name,
                            share: n as f32 / busy as f32 * utilization,
                        }
                    })
                    .collect();
                threads.push(CoreUser {
                    tid: 0,
                    pid: 0,
                    name: idle.clone(),
                    share: (100.0 - utilization).max(0.0),
                });
                threads.sort_by(by_share);

                let mut by_pid: HashMap<u32, CoreUser> = HashMap::new();
                for t in &threads {
                    by_pid
                        .entry(t.pid)
                        .or_insert_with(|| CoreUser {
                            tid: 0,
                            pid: t.pid,
                            name: t.name.clone(),
                            share: 0.0,
                        })
                        .share += t.share;
                }
                let mut processes: Vec<CoreUser> = by_pid.into_values().collect();
                processes.sort_by(by_share);
                threads.truncate(16);
                processes.truncate(16);
                CoreActivity { threads, processes }
            })
            .collect()
    }

    /// Lookups kept for the Network page.
    const LOOKUPS: usize = 2000;
    /// Names kept; past this the map starts over (addresses still in use
    /// are looked up again soon enough).
    const NAMES: usize = 50_000;

    fn update_lookups(&mut self, lookups: Vec<crate::nettrace::DnsLookup>) {
        // The cache's names fill gaps; what programs looked up wins.
        for cached in self.dns_cache.try_iter() {
            let names = Arc::make_mut(&mut self.dns_names);
            for (ip, name) in cached {
                names.entry(ip).or_insert(name);
            }
        }
        // Anabolic's own lookups are the reverse names its Network page
        // asks for: a side effect of looking, not something to show.
        let own = std::process::id();
        let lookups: Vec<_> = lookups.into_iter().filter(|l| l.pid != own).collect();
        if lookups.is_empty() {
            return;
        }
        let names = Arc::make_mut(&mut self.dns_names);
        if names.len() > Self::NAMES {
            names.clear();
        }
        for l in lookups.iter().filter(|l| l.status == 0) {
            for ip in &l.addresses {
                names.insert(*ip, l.name.clone());
            }
        }
        let log = Arc::make_mut(&mut self.dns_lookups);
        log.extend(lookups);
        if log.len() > Self::LOOKUPS * 5 / 4 {
            log.drain(..log.len() - Self::LOOKUPS);
        }
    }

    fn update_connections(&mut self, traffic: Traffic, seq: u64, secs: f64) {
        self.update_lookups(traffic.lookups);
        for (key, (rx, tx)) in traffic.per_conn {
            let c = self.conns.entry(key).or_default();
            c.rx_total += rx;
            c.tx_total += tx;
            c.rx_rate = rx as f64 / secs;
            c.tx_rate = tx as f64 / secs;
            c.last_seq = seq;
        }
        for c in self.conns.values_mut().filter(|c| c.last_seq != seq) {
            c.rx_rate = 0.0;
            c.tx_rate = 0.0;
        }
        // Forget endpoints quiet for a few minutes.
        self.conns.retain(|_, c| seq - c.last_seq < 300);
    }

    /// Every connection and listening port: Windows' socket tables (open
    /// connections, listening ports, their owners) joined with the traffic
    /// seen on each.
    fn all_connections(&self, seq: u64) -> NetActivity {
        let (sockets, error) = crate::sockets::sockets();
        let mut connections = Vec::new();
        let mut listeners = Vec::new();
        let mut listed = std::collections::HashSet::new();
        for s in sockets {
            if s.listening() {
                listeners.push(Listener {
                    pid: s.pid,
                    proto: s.proto,
                    local: s.local,
                    port: s.local_port,
                });
                continue;
            }
            // Closed connections waiting out TIME_WAIT belong to no process.
            let (Some((remote, remote_port)), true) = (s.remote, s.pid != 0) else {
                continue;
            };
            let key = ConnKey {
                pid: s.pid,
                proto: s.proto,
                remote,
                remote_port,
                local_port: s.local_port,
            };
            let seen = self.conns.get(&key);
            connections.push(Connection {
                pid: s.pid,
                proto: s.proto,
                remote,
                remote_port,
                local_port: s.local_port,
                state: s.state,
                rx_rate: seen.map_or(0.0, |c| c.rx_rate),
                tx_rate: seen.map_or(0.0, |c| c.tx_rate),
                rx_total: seen.map_or(0, |c| c.rx_total),
                tx_total: seen.map_or(0, |c| c.tx_total),
                idle: seen.map_or(0, |c| seq - c.last_seq),
                traffic: seen.is_some(),
            });
            listed.insert(key);
        }
        // Traffic the tables do not list: UDP, and TCP since closed.
        for (k, c) in self.conns.iter().filter(|(k, _)| !listed.contains(*k)) {
            connections.push(Connection {
                pid: k.pid,
                proto: k.proto,
                remote: k.remote,
                remote_port: k.remote_port,
                local_port: k.local_port,
                state: (k.proto == crate::nettrace::Proto::Tcp)
                    .then_some(crate::sockets::TcpState::Closed),
                rx_rate: c.rx_rate,
                tx_rate: c.tx_rate,
                rx_total: c.rx_total,
                tx_total: c.tx_total,
                idle: seq - c.last_seq,
                traffic: true,
            });
        }
        NetActivity {
            connections,
            listeners,
            error,
        }
    }

    fn connections_of(&self, pid: u32, seq: u64) -> Vec<Connection> {
        let mut list: Vec<Connection> = self
            .conns
            .iter()
            .filter(|(k, _)| k.pid == pid)
            .map(|(k, c)| Connection {
                pid,
                proto: k.proto,
                remote: k.remote,
                remote_port: k.remote_port,
                local_port: k.local_port,
                state: None,
                rx_rate: c.rx_rate,
                tx_rate: c.tx_rate,
                rx_total: c.rx_total,
                tx_total: c.tx_total,
                idle: seq - c.last_seq,
                traffic: true,
            })
            .collect();
        list.sort_by(|a, b| {
            (b.rx_rate + b.tx_rate)
                .partial_cmp(&(a.rx_rate + a.tx_rate))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then((b.rx_total + b.tx_total).cmp(&(a.rx_total + a.tx_total)))
        });
        list
    }

    fn run(
        &mut self,
        commands: &Receiver<Command>,
        publish: &ArcSwap<Snapshot>,
        notify: &dyn Fn(),
    ) {
        let mut next = Instant::now();
        let mut scheduled = true;
        loop {
            match catch_unwind(AssertUnwindSafe(|| self.tick(scheduled))) {
                Ok(snapshot) => {
                    if let Some(recorder) = &mut self.recorder {
                        recorder.observe(&snapshot);
                    }
                    let snapshot = Arc::new(snapshot);
                    self.previous = Some(snapshot.clone());
                    publish.store(snapshot);
                    notify();
                }
                Err(_) => eprintln!("anabolic: collector tick panicked; continuing"),
            }
            if scheduled {
                next += self.config.interval;
                let now = Instant::now();
                if next < now {
                    // Fell behind (machine stalled us); do not burst to catch up.
                    next = now + self.config.interval;
                }
            }

            scheduled = loop {
                match commands.recv_timeout(next.saturating_duration_since(Instant::now())) {
                    Err(RecvTimeoutError::Timeout) => break true,
                    Err(RecvTimeoutError::Disconnected) | Ok(Command::Shutdown) => return,
                    Ok(Command::RefreshNow) => break false,
                    Ok(Command::Select(key)) => {
                        if self.selected != key {
                            self.selected = key;
                            break false;
                        }
                    }
                    Ok(Command::TraceCores(on)) => {
                        self.trace_cores(on);
                        if on {
                            break false;
                        }
                    }
                    Ok(Command::TraceDisks(on)) => self.trace_disks(on),
                    Ok(Command::TraceFrames(on)) => self.trace_frames(on),
                    Ok(Command::TraceLatency(on)) => self.trace_latency(on),
                    Ok(Command::NetActivity(on)) => self.net_activity = on,
                    // Dropping the old one writes what it holds.
                    Ok(Command::Record(config)) => self.recorder = config.map(Recorder::start),
                    Ok(Command::SetInterval(interval)) => {
                        let interval = interval.max(Duration::from_millis(100));
                        next = next - self.config.interval + interval;
                        self.config.interval = interval;
                        self.interval_ms
                            .store(interval.as_millis() as u64, Ordering::Relaxed);
                    }
                }
            };
        }
    }

    fn tick(&mut self, scheduled: bool) -> Snapshot {
        let started = Instant::now();
        let interval = started - self.last_tick;
        self.last_tick = started;
        self.seq += 1;
        let seq = self.seq;
        let primed = seq > 1;
        let secs = interval.as_secs_f64().max(1e-3);

        let cpu = self.cpu.sample();
        let total_cpu_time = cpu.elapsed_cpu_time as f64;
        let cpu_pct = |delta: u64| {
            if total_cpu_time > 0.0 {
                (delta as f64 * 100.0 / total_cpu_time).clamp(0.0, 100.0) as f32
            } else {
                0.0
            }
        };
        let rate = |delta: u64| if primed { delta as f64 / secs } else { 0.0 };

        let mut processes = Vec::with_capacity(self.tracked.len() + 16);
        let gpu_share = self.gpu_procs.load_full();
        let traffic = self
            .net_trace
            .as_ref()
            .map(|t| t.take())
            .unwrap_or_default();
        let tracing_cores = self.core_tracer.is_some();
        let disk_sample = self.disk_tracer.as_ref().map(DiskTracer::take);
        let tracing_threads = tracing_cores || disk_sample.is_some();
        let mut selected = None;

        if self.scanner.refresh().is_err() {
            // Keep showing the last good data rather than an empty list.
            let mut snapshot = self.previous.as_deref().cloned().unwrap_or_else(|| {
                Snapshot::empty(cpu.cores.len(), self.published_history.clone())
            });
            snapshot.seq = seq;
            snapshot.collect_time = started.elapsed();
            return snapshot;
        }
        let level = self.scanner.level();

        for e in self.scanner.entries() {
            let spi = e.spi;
            let pid = e.pid();
            let key = ProcKey {
                pid,
                create_time: spi.CreateTime,
            };
            let tracked = match self.tracked.entry(key) {
                MapEntry::Occupied(o) => o.into_mut(),
                MapEntry::Vacant(v) => {
                    let sid: Option<Arc<[u8]>> = e.user_sid().map(Arc::from);
                    let image = String::from_utf16_lossy(&e.image_name());
                    if pid != 0 {
                        // The full information class reports the NT path of the image.
                        let nt_path = image.starts_with('\\').then(|| image.clone());
                        let _ = self.enrich.send(EnrichRequest {
                            key,
                            sid: sid.clone(),
                            nt_path,
                        });
                    }
                    v.insert(Tracked {
                        name: process_name(pid, &image),
                        sid,
                        first_seen: if primed { seq } else { 0 },
                        last_seen: seq,
                        exited_at: None,
                        prev: Previous::default(),
                        observed: false,
                        handle_rate: 0.0,
                        net_rx_total: 0,
                        net_tx_total: 0,
                    })
                }
            };
            tracked.last_seen = seq;
            if tracing_threads {
                for t in e.threads() {
                    self.thread_owners
                        .insert(t.info.ClientId.UniqueThread as u32, (pid, seq));
                }
            }
            let (net_rx, net_tx) = traffic.per_pid.get(&pid).copied().unwrap_or_default();
            tracked.net_rx_total += net_rx;
            tracked.net_tx_total += net_tx;

            let ext = e.extension();
            let io = IoCounters {
                read_bytes: spi.ReadTransferCount as u64,
                write_bytes: spi.WriteTransferCount as u64,
                other_bytes: spi.OtherTransferCount as u64,
                read_ops: spi.ReadOperationCount as u64,
                write_ops: spi.WriteOperationCount as u64,
                other_ops: spi.OtherOperationCount as u64,
            };
            let disk = ext.map(|x| DiskCounters {
                read_bytes: x.DiskCounters.BytesRead,
                write_bytes: x.DiskCounters.BytesWritten,
                read_ops: x.DiskCounters.ReadOperationCount,
                write_ops: x.DiskCounters.WriteOperationCount,
                flush_ops: x.DiskCounters.FlushOperationCount,
            });
            let context_switches = match ext {
                Some(x) => x.ContextSwitches,
                None => e.threads().map(|t| t.info.ContextSwitches as u64).sum(),
            };
            let now = Previous {
                cpu_time: (spi.KernelTime + spi.UserTime) as u64,
                kernel_time: spi.KernelTime as u64,
                cycles: spi.CycleTime,
                io,
                disk,
                context_switches,
                page_faults: spi.PageFaultCount,
                handles: spi.HandleCount,
            };
            // A process born during this interval did all its work in it, so
            // diffing against zero is exact.
            let before = std::mem::replace(&mut tracked.prev, now);
            if tracked.observed {
                // Exponential average with a one minute time constant: steady
                // leaks show up, bursts of open/close do not.
                let per_minute = (now.handles as f32 - before.handles as f32) * 60.0 / secs as f32;
                let alpha = 1.0 - (-(secs as f32) / 60.0).exp();
                tracked.handle_rate += alpha * (per_minute - tracked.handle_rate);
            }
            tracked.observed = true;
            let d = |a: u64, b: u64| a.saturating_sub(b);
            let disk_before = before.disk.unwrap_or_default();

            let info = ProcessInfo {
                key,
                pid,
                ppid: spi.InheritedFromUniqueProcessId as u32,
                name: tracked.name.clone(),
                session_id: spi.SessionId,
                state: if pid > 4 && e.is_suspended() {
                    ProcState::Suspended
                } else {
                    ProcState::Running
                },
                first_seen: tracked.first_seen,
                exited_at: 0,
                base_priority: spi.BasePriority,
                threads: spi.NumberOfThreads,
                handles: spi.HandleCount,
                cpu: if primed {
                    cpu_pct(d(now.cpu_time, before.cpu_time))
                } else {
                    0.0
                },
                cpu_kernel: if primed {
                    cpu_pct(d(now.kernel_time, before.kernel_time))
                } else {
                    0.0
                },
                cpu_time: now.cpu_time,
                cycles: now.cycles,
                cycles_delta: if primed {
                    d(now.cycles, before.cycles)
                } else {
                    0
                },
                private_bytes: spi.PagefileUsage as u64,
                working_set: spi.WorkingSetSize as u64,
                private_working_set: spi.WorkingSetPrivateSize as u64,
                peak_working_set: spi.PeakWorkingSetSize as u64,
                virtual_size: spi.VirtualSize as u64,
                paged_pool: spi.QuotaPagedPoolUsage as u64,
                nonpaged_pool: spi.QuotaNonPagedPoolUsage as u64,
                page_faults: spi.PageFaultCount,
                page_faults_delta: if primed {
                    now.page_faults.saturating_sub(before.page_faults)
                } else {
                    0
                },
                hard_faults: spi.HardFaultCount,
                io,
                io_read_rate: rate(d(io.read_bytes, before.io.read_bytes)),
                io_write_rate: rate(d(io.write_bytes, before.io.write_bytes)),
                io_other_rate: rate(d(io.other_bytes, before.io.other_bytes)),
                disk,
                disk_read_rate: disk.map_or(0.0, |x| rate(d(x.read_bytes, disk_before.read_bytes))),
                disk_write_rate: disk
                    .map_or(0.0, |x| rate(d(x.write_bytes, disk_before.write_bytes))),
                context_switches_delta: if primed {
                    d(now.context_switches, before.context_switches)
                } else {
                    0
                },
                handles_per_min: tracked.handle_rate,
                gpu: gpu_share.get(&key).copied().unwrap_or(0.0),
                net_rx_rate: net_rx as f64 / secs,
                net_tx_rate: net_tx as f64 / secs,
                net_rx_total: tracked.net_rx_total,
                net_tx_total: tracked.net_tx_total,
                user_sid: tracked.sid.clone(),
                not_responding: false,
                // Only threadless processes are asked: rare, and one
                // limited query each.
                ending: spi.NumberOfThreads == 0 && pid > 4 && crate::ending::is_ending(pid),
            };

            if self.selected == Some(key) {
                let threads = collect_threads(
                    &mut self.thread_prev,
                    &mut self.thread_prev_for,
                    key,
                    &e,
                    &cpu_pct,
                );
                selected = Some(SelectedThreads {
                    key,
                    threads,
                    connections: Vec::new(),
                });
            }
            processes.push(info);
        }

        // Interrupt and DPC time is not charged to any process; show it the
        // way Process Explorer does, as a pseudo process under System.
        processes.push(interrupts_pseudo(cpu.total.dpc + cpu.total.interrupt));

        self.collect_ghosts(seq, &mut processes);
        processes.sort_unstable_by_key(|p| p.key);

        if self.selected.is_none() || selected.is_none() {
            self.thread_prev.clear();
            self.thread_prev_for = None;
        }

        // Window state only: no process is opened for this.
        let hung = crate::waits::hung_windows();
        for p in processes
            .iter_mut()
            .filter(|p| p.state != ProcState::Exited)
        {
            p.not_responding = hung.iter().any(|h| h.pid == p.pid);
        }
        let mut system = SystemStats {
            cpu: cpu.total,
            cores: cpu.cores,
            hung,
            ..Default::default()
        };
        for p in processes
            .iter()
            .filter(|p| p.state != ProcState::Exited && !p.is_pseudo())
        {
            system.processes += 1;
            system.threads += p.threads;
            system.handles += p.handles;
            system.io_read_rate += p.io_read_rate;
            system.io_write_rate += p.io_write_rate;
            system.io_other_rate += p.io_other_rate;
            system.disk_read_rate += p.disk_read_rate;
            system.disk_write_rate += p.disk_write_rate;
        }
        system.memory = self.memory.sample();
        system.disks = self.disk.sample();
        if !system.disks.is_empty() {
            // Physical counters include what no process is charged for (paging, the cache manager).
            system.disk_read_rate = system.disks.iter().map(|d| d.read_rate).sum();
            system.disk_write_rate = system.disks.iter().map(|d| d.write_rate).sum();
        }
        if seq % 10 == 1 {
            self.page_files = page_files();
        }
        system.page_files = self.page_files.clone();
        system.net = self.net.sample(secs);
        system.gpus = self.gpu.sample();
        system.net_trace_error = self.net_trace_error.clone();
        system.core_trace_error = self.core_trace_error.clone();
        if tracing_cores {
            system.core_activity = self.core_activity(&processes, &system.cores, seq);
        }
        system.disk_trace_error = self.disk_trace_error.clone();
        system.disk_tracing = disk_sample.is_some();
        if let Some(sample) = disk_sample {
            for (&tid, &pid) in &sample.owners {
                self.thread_owners.insert(tid, (pid, seq));
            }
            let names: HashMap<u32, Arc<str>> = processes
                .iter()
                .filter(|p| p.state != ProcState::Exited && !p.is_pseudo())
                .map(|p| (p.pid, p.name.clone()))
                .collect();
            let owners = &self.thread_owners;
            system.disk_activity = self.disk_usage.update(
                sample,
                |tid| owners.get(&tid).map(|&(pid, _)| pid),
                |pid| names.get(&pid).cloned(),
                seq,
                secs,
            );
        }
        if tracing_threads {
            // Keep owners of exited threads for a little while: events from
            // their last moments arrive after they are gone.
            self.thread_owners.retain(|_, (_, seen)| seq - *seen < 30);
        }
        system.frame_trace_error = self.frame_trace_error.clone();
        if let Some(tracer) = &mut self.frame_tracer {
            // Sorted by key, so by pid first.
            let name_of = |pid: u32| {
                let at = processes.partition_point(|p| p.pid < pid);
                processes
                    .get(at)
                    .filter(|p| p.pid == pid)
                    .map(|p| p.name.clone())
            };
            system.frames = Some(Arc::new(tracer.take(name_of)));
        }
        system.latency_trace_error = self.latency_trace_error.clone();
        if let Some(tracer) = &mut self.latency_tracer {
            system.latency = Some(Arc::new(tracer.take()));
        }
        self.update_connections(traffic, seq, secs);
        if let Some(sel) = &mut selected {
            sel.connections = self.connections_of(sel.key.pid, seq);
        }
        if self.net_activity {
            system.net_activity = Some(Arc::new(self.all_connections(seq)));
        }
        system.dns_names = self.dns_names.clone();
        system.dns_lookups = self.dns_lookups.clone();
        system.uptime = Duration::from_millis(unsafe { GetTickCount64() });
        // Live processes plus exited ones somebody still holds a handle to.
        system.process_objects = crate::handles::object_types()
            .into_values()
            .find(|t| &*t.name == "Process")
            .map_or(0, |t| t.objects);

        if scheduled && primed {
            let h = &mut self.history;
            h.cpu.push(system.cpu.total);
            h.cpu_kernel.push(system.cpu.kernel);
            for (ring, core) in h.cores.iter_mut().zip(&system.cores) {
                ring.push(core.total);
            }
            for (ring, core) in h.core_kernel.iter_mut().zip(&system.cores) {
                ring.push(core.kernel);
            }
            h.memory_used.push(system.memory.used() as f32);
            h.commit.push(system.memory.commit as f32);
            h.disk_read.push(system.disk_read_rate as f32);
            h.disk_write.push(system.disk_write_rate as f32);
            h.io_read.push(system.io_read_rate as f32);
            h.io_write.push(system.io_write_rate as f32);
            h.net_rx.push(system.net.rx_rate as f32);
            h.net_tx.push(system.net.tx_rate as f32);
            for d in &system.disks {
                let dh = h.disk_mut(d.number);
                dh.read.push(d.read_rate as f32);
                dh.write.push(d.write_rate as f32);
                dh.active.push(d.active);
            }
            for a in &system.net.adapters {
                let ah = h.adapter_mut(a.luid);
                ah.rx.push(a.rx_rate as f32);
                ah.tx.push(a.tx_rate as f32);
            }
            for (gh, gs) in h.gpus.iter_mut().zip(&system.gpus) {
                gh.usage.push(gs.usage);
                for (ring, engine) in gh.engines.iter_mut().zip(&gs.engines) {
                    ring.push(engine.usage);
                }
                gh.dedicated.push(gs.dedicated_used as f32);
            }
            self.published_history = Arc::new(self.history.clone());
        }

        Snapshot {
            seq,
            interval,
            collect_time: started.elapsed(),
            info_level: level,
            processes,
            system,
            history: self.published_history.clone(),
            selected,
            own_pid: std::process::id(),
        }
    }

    /// Keeps processes that just exited around (marked) for a couple of ticks.
    fn collect_ghosts(&mut self, seq: u64, out: &mut Vec<ProcessInfo>) {
        let keep_for = self.config.highlight_ticks;
        let previous = self.previous.clone();
        self.tracked.retain(|key, t| {
            if t.last_seen == seq {
                return true;
            }
            let exited_at = *t.exited_at.get_or_insert(seq);
            if seq - exited_at >= keep_for {
                return false;
            }
            if let Some(last) = previous.as_ref().and_then(|s| s.find(*key)) {
                let mut ghost = last.clone();
                ghost.state = ProcState::Exited;
                ghost.exited_at = exited_at;
                ghost.cpu = 0.0;
                ghost.cpu_kernel = 0.0;
                ghost.io_read_rate = 0.0;
                ghost.io_write_rate = 0.0;
                ghost.io_other_rate = 0.0;
                ghost.disk_read_rate = 0.0;
                ghost.disk_write_rate = 0.0;
                out.push(ghost);
            }
            true
        });
    }
}

fn collect_threads(
    thread_prev: &mut HashMap<u32, (i64, u64, u32)>,
    thread_prev_for: &mut Option<ProcKey>,
    key: ProcKey,
    e: &Entry<'_>,
    cpu_pct: &dyn Fn(u64) -> f32,
) -> Vec<ThreadInfo> {
    let has_baseline = *thread_prev_for == Some(key);
    let mut next_prev = HashMap::with_capacity(e.spi.NumberOfThreads as usize);
    let threads = e
        .threads()
        .map(|t| {
            let i = t.info;
            let tid = i.ClientId.UniqueThread as u32;
            let cpu_time = (i.KernelTime + i.UserTime) as u64;
            let (prev_cpu, prev_cs) = match thread_prev.get(&tid) {
                Some(&(created, cpu, cs)) if created == i.CreateTime => (cpu, cs),
                // New thread: everything it did happened during this interval.
                _ if has_baseline => (0, 0),
                _ => (cpu_time, i.ContextSwitches),
            };
            next_prev.insert(tid, (i.CreateTime, cpu_time, i.ContextSwitches));
            ThreadInfo {
                tid,
                create_time: i.CreateTime,
                start_address: i.StartAddress as u64,
                win32_start_address: t.ext.map_or(0, |x| x.Win32StartAddress as u64),
                priority: i.Priority,
                base_priority: i.BasePriority,
                state: i.ThreadState,
                wait_reason: i.WaitReason,
                cpu: cpu_pct(cpu_time.saturating_sub(prev_cpu)),
                kernel_time: i.KernelTime as u64,
                user_time: i.UserTime as u64,
                context_switches: i.ContextSwitches,
                context_switches_delta: i.ContextSwitches.saturating_sub(prev_cs),
                stack_base: t.ext.map_or(0, |x| x.StackBase as u64),
                stack_limit: t.ext.map_or(0, |x| x.StackLimit as u64),
                teb: t.ext.map_or(0, |x| x.TebBase as u64),
            }
        })
        .collect();
    *thread_prev = next_prev;
    *thread_prev_for = Some(key);
    threads
}

fn process_name(pid: u32, image: &str) -> Arc<str> {
    match (pid, image.rsplit('\\').next()) {
        (0, _) => "System Idle Process".into(),
        (_, Some(name)) if !name.is_empty() => name.into(),
        _ => "<unknown>".into(),
    }
}

fn interrupts_pseudo(cpu: f32) -> ProcessInfo {
    let mut p = ProcessInfo::blank(
        ProcKey {
            pid: INTERRUPTS_PID,
            create_time: 0,
        },
        4,
        Arc::from("Interrupts"),
    );
    p.base_priority = 0;
    p.cpu = cpu;
    p.cpu_kernel = cpu;
    p
}
