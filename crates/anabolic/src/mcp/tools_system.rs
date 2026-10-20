//! Tools about the whole machine: the overview, CPU, memory, disks, GPUs,
//! sensors, monitors, hardware, frame times and driver latency, and the
//! history.

use std::sync::Arc;
use std::time::Duration;

use anabolic_core::Snapshot;
use anabolic_core::model::ProcState;
use anabolic_core::sensors::{self, Heat, Sensor, SensorKind};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Trace;
use super::handler::{AnabolicMcp, blocking, fail, ok, progress};
use super::json::{self, pct};
use crate::fmt;
use crate::settings::McpKind;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Overview {
    /// How many of the busiest processes to list per resource (default 5).
    pub top: Option<u32>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Traced {
    /// Seconds to trace before answering (0 to 30, default 0): turns on the
    /// trace that tells who uses the resource, which costs some CPU while it
    /// runs and stops by itself afterwards.
    pub trace_seconds: Option<u32>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Watch {
    /// Seconds to watch before answering (0 to 60, default 5): frame times
    /// need some frames, driver timing some interrupts.
    pub watch_seconds: Option<u32>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct HistoryQuery {
    /// The day, YYYY-MM-DD (default today); list_history_days lists them.
    pub date: Option<String>,
    /// Local time of day to start at, HH:MM (default the day's start).
    pub from: Option<String>,
    /// Local time of day to end at, HH:MM (default the day's end).
    pub to: Option<String>,
    /// Processes listed per sample (default 3, at most 10).
    pub top: Option<u32>,
    /// At most this many samples, the latest kept (default 120, at most 2000).
    pub limit: Option<u32>,
}

/// The `n` processes with the most of `value`, above zero.
fn top_by(
    snap: &Snapshot,
    n: usize,
    value: impl Fn(&anabolic_core::ProcessInfo) -> f64,
) -> Vec<Value> {
    let mut rows: Vec<_> = snap
        .processes
        .iter()
        .filter(|p| p.state != ProcState::Exited && p.pid != 0)
        .map(|p| (value(p), p))
        .filter(|(v, _)| *v > 0.0)
        .collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    rows.into_iter()
        .take(n)
        .map(|(v, p)| json!({ "pid": p.pid, "name": &*p.name, "value": (v * 10.0).round() / 10.0 }))
        .collect()
}

fn memory_json(snap: &Snapshot) -> Value {
    let m = &snap.system.memory;
    let used = m.total.saturating_sub(m.available);
    json!({
        "total_bytes": m.total,
        "used_bytes": used,
        "available_bytes": m.available,
        "used_percent": if m.total > 0 { (used as f64 * 1000.0 / m.total as f64).round() / 10.0 } else { 0.0 },
        "used": format!("{} of {}", fmt::bytes(used), fmt::bytes(m.total)),
        "commit_bytes": m.commit,
        "commit_limit_bytes": m.commit_limit,
        "commit_peak_bytes": m.commit_peak,
        "file_cache_bytes": m.system_cache,
        "paged_pool_bytes": m.paged_pool,
        "nonpaged_pool_bytes": m.nonpaged_pool,
        "lists": m.lists.as_ref().map(|l| json!({
            "standby_bytes": l.standby,
            "modified_bytes": l.modified,
            "free_bytes": l.free,
            "zeroed_bytes": l.zeroed,
        })),
    })
}

fn disks_json(snap: &Snapshot) -> Vec<Value> {
    snap.system
        .disks
        .iter()
        .map(|d| {
            json!({
                "disk": d.number,
                "model": d.model,
                "bus": d.bus,
                "ssd": d.ssd,
                "size_bytes": d.size,
                "active_percent": pct(d.active),
                "read_bps": d.read_rate.round(),
                "write_bps": d.write_rate.round(),
                "read_iops": d.read_iops.round(),
                "write_iops": d.write_iops.round(),
                "response_ms": (d.response_ms * 10.0).round() / 10.0,
                "queue": d.queue,
                "temperature_c": d.temperature,
                "volumes": d.volumes.iter().map(|v| json!({
                    "letter": v.letter,
                    "label": v.label,
                    "total_bytes": v.total,
                    "free_bytes": v.free,
                })).collect::<Vec<_>>(),
                "partition_style": d.style.name(),
                "partitions": d.partitions.iter().map(|p| json!({
                    "number": p.number,
                    "device": (!p.device.is_empty()).then_some(&p.device),
                    "offset_bytes": p.offset,
                    "size_bytes": p.size,
                    "type": p.kind,
                    "role": p.role.name(),
                    "name": p.name,
                    "label": p.label,
                    "file_system": p.file_system,
                    "mounted_at": p.mounts,
                    "total_bytes": (p.total > 0).then_some(p.total),
                    "free_bytes": (p.total > 0).then_some(p.free),
                    "holds_running_system": p.system,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn sensor_json(s: &Sensor) -> Value {
    json!({
        "device": &*s.device,
        "sensor": &*s.label,
        "kind": match s.kind {
            SensorKind::Temperature => "temperature",
            SensorKind::Fan => "fan",
            SensorKind::Power => "power",
        },
        "value": pct(s.value),
        "unit": match s.kind {
            SensorKind::Temperature => "celsius",
            SensorKind::Fan => "rpm",
            SensorKind::Power => "watts",
        },
        "high": s.high.map(pct),
        "critical": s.critical.map(pct),
        "heat": (s.kind == SensorKind::Temperature).then(|| match s.heat() {
            Heat::Normal => "normal",
            Heat::Warm => "warm",
            Heat::Hot => "hot",
        }),
    })
}

fn gpus_json(snap: &Snapshot) -> Vec<Value> {
    snap.system
        .gpus
        .iter()
        .map(|g| {
            json!({
                "name": &*g.name,
                "usage_percent": pct(g.usage),
                "dedicated_used_bytes": g.dedicated_used,
                "dedicated_total_bytes": g.dedicated_total,
                "shared_used_bytes": g.shared_used,
                "temperature_c": g.temperature.map(pct),
                "sensors": g.sensors.iter().map(sensor_json).collect::<Vec<_>>(),
                "engines": g.engines.iter().filter(|e| e.usage > 0.0).map(|e| json!({
                    "engine": &*e.name,
                    "usage_percent": pct(e.usage),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn network_json(snap: &Snapshot) -> Value {
    let n = &snap.system.net;
    json!({
        "receive_bps": n.rx_rate.round(),
        "send_bps": n.tx_rate.round(),
        "adapters": n.adapters.iter().map(|a| json!({
            "name": a.name,
            "description": a.description,
            "link_bits_per_second": a.link_speed,
            "receive_bps": a.rx_rate.round(),
            "send_bps": a.tx_rate.round(),
            "received_bytes_total": a.rx_total,
            "sent_bytes_total": a.tx_total,
        })).collect::<Vec<_>>(),
    })
}

/// Parses HH:MM into seconds since midnight.
fn time_of_day(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 3600 + m * 60)
}

#[tool_router(router = system_tools, vis = "pub(super)")]
impl AnabolicMcp {
    /// Turns a trace on (through the UI, which owns them) and waits for its
    /// data; the latest snapshot after.
    pub(super) async fn traced(
        &self,
        trace: Trace,
        secs: u32,
        context: &RequestContext<RoleServer>,
    ) -> Arc<Snapshot> {
        let secs = if self.shared.allowed(McpKind::Trace).is_ok() {
            secs
        } else {
            0
        };
        if secs > 0 {
            self.shared
                .wants
                .want(trace, Duration::from_secs(secs as u64 + 5));
            crate::repaint(&self.shared.ctx);
            for done in 0..secs {
                progress(context, done as f64, Some(secs as f64), "tracing").await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        self.snapshot().await
    }

    #[tool(
        description = "The state of the whole PC right now: CPU, memory, disks, network, GPUs, \
                       process/thread/handle counts, uptime, the busiest processes per resource, \
                       and anything that needs attention (hung windows, processes that cannot \
                       end, memory pressure, leaks). The place to start.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_system_overview(
        &self,
        Parameters(args): Parameters<Overview>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let n = args.top.unwrap_or(5).clamp(1, 30) as usize;
        let s = &snap.system;
        let m = &s.memory;
        let mut attention = Vec::new();
        if m.total > 0 && m.available * 10 < m.total {
            attention.push(format!(
                "Memory is short: {} of {} available.",
                fmt::bytes(m.available),
                fmt::bytes(m.total)
            ));
        }
        if m.commit_limit > 0 && m.commit * 100 > m.commit_limit * 90 {
            attention.push(format!(
                "Commit charge is at {} of its {} limit.",
                fmt::bytes(m.commit),
                fmt::bytes(m.commit_limit)
            ));
        }
        for p in snap.processes.iter().filter(|p| p.not_responding) {
            attention.push(format!("{} (PID {}) is not responding.", p.name, p.pid));
        }
        for p in snap.processes.iter().filter(|p| p.ending) {
            attention.push(format!(
                "{} (PID {}) is ending but cannot finish (why_process_will_not_end says why).",
                p.name, p.pid
            ));
        }
        for d in s.disks.iter().filter(|d| d.active >= 90.0) {
            attention.push(format!(
                "Disk {} ({}) is {:.0} % active.",
                d.number, d.model, d.active
            ));
        }
        for leak in self.shared.leaks.load().iter() {
            attention.push(fmt::leak(leak));
        }
        for t in s.sensors.iter().filter(|t| t.heat() == Heat::Hot) {
            attention.push(format!(
                "{} {} is at {:.0} °C{}.",
                t.device,
                t.label,
                t.value,
                t.critical.map_or(String::new(), |c| format!(
                    ", close to its critical {c:.0} °C"
                ))
            ));
        }
        ok(json!({
            "anabolic_elevated": self.shared.elevated,
            "uptime_seconds": s.uptime.as_secs(),
            "uptime": fmt::duration(s.uptime),
            "processes": s.processes,
            "threads": s.threads,
            "handles": s.handles,
            "cpu": json::cpu_times(&s.cpu),
            "cpu_temperature_c": s.cpu_temperature.map(pct),
            "hottest_sensor": s
                .sensors
                .iter()
                .filter(|t| t.kind == SensorKind::Temperature)
                .max_by(|a, b| a.value.total_cmp(&b.value))
                .map(sensor_json),
            "logical_processors": s.cores.len(),
            "memory": memory_json(&snap),
            "disks": disks_json(&snap),
            "network": network_json(&snap),
            "gpus": gpus_json(&snap),
            "busiest": {
                "cpu_percent": top_by(&snap, n, |p| p.cpu as f64),
                "private_bytes": top_by(&snap, n, |p| p.private_bytes as f64),
                "disk_bps": top_by(&snap, n, |p| p.disk_read_rate + p.disk_write_rate),
                "network_bps": top_by(&snap, n, |p| p.net_rx_rate + p.net_tx_rate),
                "gpu_percent": top_by(&snap, n, |p| p.gpu as f64),
                "handles": top_by(&snap, n, |p| p.handles as f64),
            },
            "attention": attention,
            "sampled_every_ms": snap.interval.as_millis() as u64,
        }))
    }

    #[tool(
        description = "CPU: total and per logical processor (user, kernel, DPC, interrupt, \
                       clock), interrupt and context switch rates, the processor's name and \
                       topology. With trace_seconds > 0, also who ran on each core (threads and \
                       processes with their share), from a context switch trace.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_cpu(
        &self,
        Parameters(args): Parameters<Traced>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let secs = args.trace_seconds.unwrap_or(0).min(30);
        let snap = self.traced(Trace::Cores, secs, &context).await;
        let s = &snap.system;
        let topology = blocking(anabolic_core::topology::topology).await;
        let cores: Vec<Value> =
            s.cores
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut core = json::cpu_times(c);
                    core["cpu"] = json!(i);
                    if let Some(t) = topology.cpus.get(i) {
                        core["physical_core"] = json!(t.core);
                        core["efficiency_class"] = json!(t.efficiency_class);
                        core["numa_node"] = json!(t.numa);
                    }
                    if let Some(a) = s.core_activity.get(i) {
                        core["processes"] =
                            json!(a.processes.iter().take(5).map(|u| json!({
                        "pid": u.pid, "name": &*u.name, "share_percent": pct(u.share),
                    })).collect::<Vec<_>>());
                        core["threads"] = json!(
                            a.threads
                                .iter()
                                .take(5)
                                .map(|u| json!({
                                    "pid": u.pid, "tid": u.tid, "process": &*u.name,
                                    "share_percent": pct(u.share),
                                }))
                                .collect::<Vec<_>>()
                        );
                    }
                    core
                })
                .collect();
        ok(json!({
            "name": anabolic_core::sysinfo::cpu_name(),
            "total": json::cpu_times(&s.cpu),
            "interrupts_per_second": s.cpu.interrupt_rate.round(),
            "dpcs_per_second": s.cpu.dpc_rate.round(),
            "context_switches_per_second": s.cpu.context_switch_rate.round(),
            "temperature_c": s.cpu_temperature.map(pct),
            "temperatures": s
                .sensors
                .iter()
                .filter(|t| &*t.device == sensors::CPU)
                .map(sensor_json)
                .collect::<Vec<_>>(),
            "physical_cores": topology.cores,
            "logical_processors": s.cores.len(),
            "per_logical_processor": cores,
            "who_ran_traced": !s.core_activity.is_empty(),
            "trace_error": s.core_trace_error,
            "busiest_processes": top_by(&snap, 10, |p| p.cpu as f64),
        }))
    }

    #[tool(
        description = "Memory: used, available, commit charge and limit, file cache, pools, the \
                       standby/modified/free lists, page files, and the processes with the most \
                       private memory.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_memory(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let mut memory = memory_json(&snap);
        memory["page_files"] = json!(snap.system.page_files.iter().map(|p| json!({
            "path": p.path, "size_bytes": p.size, "used_bytes": p.used, "peak_bytes": p.peak,
        })).collect::<Vec<_>>());
        memory["most_private_bytes"] = json!(top_by(&snap, 15, |p| p.private_bytes as f64));
        memory["most_working_set_bytes"] = json!(top_by(&snap, 10, |p| p.working_set as f64));
        ok(memory)
    }

    #[tool(
        description = "Disks: every physical disk with its partitions (type, file system, \
                       where mounted, space) and volumes, activity, throughput, IOPS, \
                       response time, queue and temperature. With trace_seconds > 0, also which \
                       processes read and write each disk and which files, from a disk I/O \
                       trace.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_disks(
        &self,
        Parameters(args): Parameters<Traced>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let secs = args.trace_seconds.unwrap_or(0).min(30);
        let snap = self.traced(Trace::Disks, secs, &context).await;
        let mut disks = disks_json(&snap);
        for disk in &mut disks {
            let number = disk["disk"].as_u64().unwrap_or(u64::MAX) as u32;
            if let Some(a) = snap.system.disk_activity.iter().find(|a| a.disk == number) {
                disk["processes"] = json!(a.processes.iter().take(15).map(|u| json!({
                    "pid": u.pid, "name": &*u.name,
                    "read_bps": u.io.read_rate.round(), "write_bps": u.io.write_rate.round(),
                    "response_ms": (u.io.response_ms * 10.0).round() / 10.0,
                })).collect::<Vec<_>>());
                disk["files"] = json!(a.files.iter().take(25).map(|f| json!({
                    "path": &*f.path, "pid": f.pid, "process": &*f.process,
                    "read_bps": f.io.read_rate.round(), "write_bps": f.io.write_rate.round(),
                    "response_ms": (f.io.response_ms * 10.0).round() / 10.0,
                })).collect::<Vec<_>>());
            }
        }
        ok(json!({
            "disks": disks,
            "read_bps": snap.system.disk_read_rate.round(),
            "write_bps": snap.system.disk_write_rate.round(),
            "per_process_traced": snap.system.disk_tracing,
            "trace_error": snap.system.disk_trace_error,
            "busiest_processes": top_by(&snap, 10, |p| p.disk_read_rate + p.disk_write_rate),
        }))
    }

    #[tool(
        description = "Temperatures, fan speeds and power draw: every sensor Anabolic reads (the \
                       processor's package and cores, GPUs, disks, memory modules, network \
                       adapters, thermal zones, fans), each with the hardware's high and \
                       critical marks and how close it is to them. On Windows only drivers may \
                       read the processor's own temperature: the firmware's thermal zones stand \
                       in where it has any (many desktops have none), and the processor's power \
                       comes from its energy meter.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_sensors(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let s = &snap.system;
        ok(json!({
            "cpu_temperature_c": s.cpu_temperature.map(pct),
            "sensors": s.sensors.iter().map(sensor_json).collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "GPUs: each adapter's usage, engines (3D, copy, video...), dedicated and \
                       shared memory, temperature, and the processes using the GPU most.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_gpus(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        ok(json!({
            "gpus": gpus_json(&snap),
            "busiest_processes": top_by(&snap, 10, |p| p.gpu as f64),
        }))
    }

    #[tool(
        description = "Monitors: each one's own name, GPU, connection, position, mode and refresh \
                       rate against the best it offers, native resolution, HDR and scaling. \
                       Points out a monitor left below its refresh rate or resolution.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_displays(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let displays = blocking(anabolic_core::displays::displays).await;
        ok(json!({ "displays": displays.iter().map(|d| json!({
            "name": d.name,
            "device": d.device,
            "gpu": d.gpu,
            "connection": d.connection,
            "primary": d.primary,
            "position": [d.x, d.y],
            "resolution": [d.width, d.height],
            "native_resolution": [d.native.0, d.native.1],
            "refresh_hz": (d.refresh_hz * 100.0).round() / 100.0,
            "max_refresh_hz_at_this_resolution": d.max_hz_here,
            "max_refresh_hz": d.max_hz,
            "below_max_refresh": d.below_max(),
            "below_native_resolution": d.below_native(),
            "hdr_supported": d.hdr_supported,
            "hdr_on": d.hdr_on,
            "bits_per_color": d.bits_per_color,
            "scale_percent": d.scale_pct,
        })).collect::<Vec<_>>() }))
    }

    #[tool(
        description = "Hardware: the processor (model, cores, caches, clocks, features, \
                       virtualisation), the board and BIOS, memory modules (size, speed and \
                       configured speed, maker, part number) and the core topology.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_hardware(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let hw = blocking(anabolic_core::hwinfo::hardware_info).await;
        let c = &hw.cpu;
        let mut classes: Vec<u8> = hw
            .topology
            .cpus
            .iter()
            .map(|c| c.efficiency_class)
            .collect();
        classes.sort_unstable();
        classes.dedup();
        ok(json!({
            "cpu": {
                "brand": c.brand,
                "vendor": c.vendor,
                "family": c.family, "model": c.model, "stepping": c.stepping,
                "socket": c.socket,
                "sockets": c.sockets,
                "cores": c.cores,
                "threads": c.threads,
                "numa_nodes": c.numa_nodes,
                "base_mhz": c.base_mhz,
                "max_mhz": c.max_mhz,
                "caches": c.caches.iter().map(|(name, kb, count)| json!({
                    "cache": name, "size_kb": kb, "count": count,
                })).collect::<Vec<_>>(),
                "features": c.features,
                "virtualization_enabled": c.virtualization_enabled,
                "hypervisor_present": c.hypervisor_present,
                "microcode": c.microcode,
            },
            "board": {
                "manufacturer": hw.board.manufacturer,
                "product": hw.board.product,
                "bios_vendor": hw.board.bios_vendor,
                "bios_version": hw.board.bios_version,
                "bios_date": hw.board.bios_date,
            },
            "memory": {
                "installed_bytes": hw.memory.installed,
                "slots": hw.memory.slots,
                "max_capacity_bytes": hw.memory.max_capacity,
                "error_correction": hw.memory.error_correction,
                "modules": hw.memory.modules.iter().map(|m| json!({
                    "slot": m.locator, "bank": m.bank, "size_bytes": m.size, "kind": m.kind,
                    "form_factor": m.form_factor, "rated_mts": m.speed,
                    "running_mts": m.configured_speed, "manufacturer": m.manufacturer,
                    "part_number": m.part_number, "rank": m.rank, "voltage_mv": m.voltage_mv,
                })).collect::<Vec<_>>(),
            },
            "topology": {
                "physical_cores": hw.topology.cores,
                "logical_processors": hw.topology.cpus.len(),
                "efficiency_classes": classes,
            },
        }))
    }

    #[tool(
        description = "Stutters: frame rate, 1 % lows, frame times and stutter counts of every \
                       program that draws (games), the latest stutters, and DPC/ISR time per \
                       driver (LatencyMon style) with the longest spikes. Watches for \
                       watch_seconds first (default 5) if the trace was not already running. No \
                       game is opened.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_stutters(
        &self,
        Parameters(args): Parameters<Watch>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let secs = args.watch_seconds.unwrap_or(5).min(60);
        let snap = self.traced(Trace::Frames, secs, &context).await;
        let s = &snap.system;
        let frames = s.frames.as_ref().map(|f| {
            json!({
                "programs": f.processes.iter().map(|p| json!({
                    "pid": p.pid, "name": &*p.name, "api": format!("{:?}", p.api),
                    "fps": (p.fps * 10.0).round() / 10.0,
                    "low_1_percent_fps": (p.low_fps * 10.0).round() / 10.0,
                    "average_frame_ms": (p.avg_ms * 100.0).round() / 100.0,
                    "p99_frame_ms": (p.p99_ms * 100.0).round() / 100.0,
                    "worst_frame_ms": (p.max_ms * 100.0).round() / 100.0,
                    "stutters": p.stutters,
                    "window": p.window.as_ref().map(|w| json!({
                        "monitor": &*w.monitor, "refresh_hz": w.refresh_hz,
                        "size": [w.width, w.height], "fullscreen": w.fullscreen,
                        "in_front": w.foreground,
                    })),
                })).collect::<Vec<_>>(),
                "latest_stutters": f.stutters.iter().rev().take(30).map(|st| json!({
                    "at": json::system_time(st.at), "pid": st.pid, "name": &*st.name,
                    "frame_ms": (st.frame_ms * 10.0).round() / 10.0,
                    "typical_ms": (st.typical_ms * 10.0).round() / 10.0,
                })).collect::<Vec<_>>(),
                "lost_events": f.lost_events,
            })
        });
        let latency = s.latency.as_ref().map(|l| json!({
            "measured_seconds": json::secs(l.measured),
            "drivers": l.drivers.iter().take(20).map(|d| json!({
                "driver": &*d.driver, "path": &*d.path,
                "dpc": { "count": d.dpc.count, "per_second": d.dpc.per_sec.round(),
                         "busy_percent": pct(d.dpc.busy * 100.0), "longest_us": d.dpc.highest_us.round() },
                "isr": { "count": d.isr.count, "per_second": d.isr.per_sec.round(),
                         "busy_percent": pct(d.isr.busy * 100.0), "longest_us": d.isr.highest_us.round() },
            })).collect::<Vec<_>>(),
            "longest_spikes": l.spikes.iter().take(20).map(|sp| json!({
                "at": json::system_time(sp.at), "driver": &*sp.driver,
                "kind": format!("{:?}", sp.kind), "cpu": sp.cpu, "us": sp.us.round(),
            })).collect::<Vec<_>>(),
            "highest_us_per_cpu": l.per_cpu_highest_us.iter().map(|u| u.round()).collect::<Vec<_>>(),
        }));
        ok(json!({
            "frames": frames,
            "frame_trace_error": s.frame_trace_error,
            "driver_latency": latency,
            "latency_trace_error": s.latency_trace_error,
        }))
    }

    #[tool(
        description = "The days the history (flight recorder) holds: every 10 seconds while \
                       Anabolic runs, the machine's totals and the processes that led.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_history_days(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let Some(dir) = self.shared.data_dir.as_ref().map(|d| d.join("history")) else {
            return fail("Anabolic has no data folder here.");
        };
        let days = blocking(move || anabolic_core::recorder::days(&dir)).await;
        ok(
            json!({ "days": days.iter().map(|d| json!({ "date": d.date, "bytes": d.bytes })).collect::<Vec<_>>() }),
        )
    }

    #[tool(
        description = "What happened at a time: the history's samples for a day (or part of one), \
                       each with CPU, memory, commit, disk and network totals, the GPU, the \
                       processes that led, and the processes that started, exited or hung. \
                       Answers \"why was the PC slow at 14:32\".",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_history(
        &self,
        Parameters(args): Parameters<HistoryQuery>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let Some(dir) = self.shared.data_dir.as_ref().map(|d| d.join("history")) else {
            return fail("Anabolic has no data folder here.");
        };
        let date = args.date.clone().unwrap_or_else(|| {
            anabolic_core::recorder::local_day(anabolic_core::recorder::unix_now()).0
        });
        let from = match args.from.as_deref().map(time_of_day) {
            Some(None) => return fail("from must be HH:MM (24-hour, local time)."),
            Some(Some(s)) => s,
            None => 0,
        };
        let to = match args.to.as_deref().map(time_of_day) {
            Some(None) => return fail("to must be HH:MM (24-hour, local time)."),
            Some(Some(s)) => s + 59,
            None => 86_400,
        };
        let top = args.top.unwrap_or(3).min(10) as usize;
        let limit = args.limit.unwrap_or(120).clamp(1, 2000) as usize;
        let day = date.clone();
        let buckets = match blocking(move || anabolic_core::recorder::load(&dir, &day)).await {
            Ok(b) => b,
            Err(e) => return fail(format!("No history for {date}: {e}")),
        };
        let chosen: Vec<_> = buckets
            .iter()
            .filter(|b| {
                let (_, secs) = anabolic_core::recorder::local_day(b.start);
                (from..=to).contains(&secs)
            })
            .collect();
        let skipped = chosen.len().saturating_sub(limit);
        let samples: Vec<Value> = chosen[skipped..]
            .iter()
            .map(|b| {
                let mut procs: Vec<_> = b.processes.iter().collect();
                procs.sort_by(|a, b| b.cpu_avg.total_cmp(&a.cpu_avg));
                json!({
                    "at": json::unix(b.start),
                    "seconds": b.secs,
                    "cpu_average_percent": pct(b.cpu_avg),
                    "cpu_max_percent": pct(b.cpu_max),
                    "memory_used_bytes": b.memory_used,
                    "memory_total_bytes": b.memory_total,
                    "commit_bytes": b.commit,
                    "disk_read_bytes": b.disk_read,
                    "disk_write_bytes": b.disk_write,
                    "disk_active_percent": pct(b.disk_active),
                    "net_received_bytes": b.net_rx,
                    "net_sent_bytes": b.net_tx,
                    "gpu_max_percent": pct(b.gpu_max),
                    "leading_processes": procs.iter().take(top).map(|p| json!({
                        "pid": p.pid, "name": &*p.name,
                        "cpu_average_percent": pct(p.cpu_avg), "cpu_max_percent": pct(p.cpu_max),
                        "private_bytes": p.private, "io_bytes": p.io, "disk_bytes": p.disk,
                        "net_bytes": p.net, "gpu_average_percent": pct(p.gpu_avg),
                    })).collect::<Vec<_>>(),
                    "started": b.started.iter().map(|e| json!({ "pid": e.pid, "name": &*e.name, "count": e.count })).collect::<Vec<_>>(),
                    "exited": b.exited.iter().map(|e| json!({ "pid": e.pid, "name": &*e.name, "count": e.count })).collect::<Vec<_>>(),
                    "not_responding": b.hung.iter().map(|e| json!({ "pid": e.pid, "name": &*e.name })).collect::<Vec<_>>(),
                })
            })
            .collect();
        ok(json!({
            "date": date,
            "samples": samples,
            "earlier_samples_left_out": skipped,
        }))
    }
}
