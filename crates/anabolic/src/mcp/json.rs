// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Anabolic's data as JSON for MCP clients: numbers with their unit in the
//! field name (`_bytes`, `_bps`, `_percent`), and the same in words where a
//! person would read it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anabolic_core::Snapshot;
use anabolic_core::actions::Priority;
use anabolic_core::model::{CpuTimes, ProcState, ProcessInfo};
use serde_json::{Value, json};

use crate::fmt;

/// FILETIME ticks between 1601 and 1970.
const UNIX_IN_FILETIME: i64 = 116_444_736_000_000_000;

/// A FILETIME as local time, or null.
pub fn filetime(ft: i64) -> Value {
    anabolic_core::sysinfo::local_time(ft).map_or(Value::Null, Value::String)
}

/// A wall clock moment as local time.
pub fn system_time(at: SystemTime) -> Value {
    let unix = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    filetime(unix.as_nanos() as i64 / 100 + UNIX_IN_FILETIME)
}

/// Unix seconds as local time.
pub fn unix(secs: u64) -> Value {
    filetime(secs as i64 * 10_000_000 + UNIX_IN_FILETIME)
}

pub fn secs(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round() / 1000.0
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

pub fn pct(x: f32) -> f64 {
    round(x as f64, 1)
}

pub fn state(p: &ProcessInfo) -> &'static str {
    match p.state {
        ProcState::Running => "running",
        ProcState::Suspended => "suspended",
        ProcState::Exited => "exited",
    }
}

pub fn priority(base: i32) -> &'static str {
    Priority::from_base(base).map_or("unknown", Priority::label)
}

/// Account names by SID, looked up once per call.
#[derive(Default)]
pub struct Users(HashMap<Arc<[u8]>, String>);

impl Users {
    pub fn name(&mut self, sid: &Option<Arc<[u8]>>) -> Value {
        let Some(sid) = sid else {
            return Value::Null;
        };
        let name = self
            .0
            .entry(sid.clone())
            .or_insert_with(|| anabolic_core::enrich::user_name(sid).unwrap_or_default());
        if name.is_empty() {
            Value::Null
        } else {
            Value::String(name.clone())
        }
    }
}

/// One line of the process list.
pub fn process_row(p: &ProcessInfo, users: Option<&mut Users>) -> Value {
    let mut row = json!({
        "pid": p.pid,
        "parent_pid": p.ppid,
        "name": &*p.name,
        "state": state(p),
        "cpu_percent": pct(p.cpu),
        "private_bytes": p.private_bytes,
        "memory": fmt::bytes(p.private_bytes),
        "working_set_bytes": p.working_set,
        "threads": p.threads,
        "handles": p.handles,
        "gpu_percent": pct(p.gpu),
        "disk_read_bps": p.disk_read_rate.round(),
        "disk_write_bps": p.disk_write_rate.round(),
        "net_receive_bps": p.net_rx_rate.round(),
        "net_send_bps": p.net_tx_rate.round(),
        "priority": priority(p.base_priority),
        "started": filetime(p.key.create_time),
    });
    if p.not_responding {
        row["not_responding"] = json!(true);
    }
    if p.ending {
        row["ending_but_cannot_finish"] = json!(true);
    }
    if let Some(users) = users {
        row["user"] = users.name(&p.user_sid);
    }
    row
}

/// Everything the snapshot holds on a process.
pub fn process_counters(p: &ProcessInfo) -> Value {
    json!({
        "pid": p.pid,
        "parent_pid": p.ppid,
        "name": &*p.name,
        "state": state(p),
        "session": p.session_id,
        "started": filetime(p.key.create_time),
        "create_time_filetime": p.key.create_time,
        "priority": priority(p.base_priority),
        "base_priority": p.base_priority,
        "not_responding": p.not_responding,
        "ending_but_cannot_finish": p.ending,
        "cpu": {
            "percent": pct(p.cpu),
            "kernel_percent": pct(p.cpu_kernel),
            "time_seconds": round(p.cpu_time as f64 / 1e7, 2),
            "time": fmt::cpu_time(p.cpu_time),
            "cycles_last_interval": p.cycles_delta,
            "context_switches_last_interval": p.context_switches_delta,
        },
        "memory": {
            "private_bytes": p.private_bytes,
            "private": fmt::bytes(p.private_bytes),
            "working_set_bytes": p.working_set,
            "private_working_set_bytes": p.private_working_set,
            "peak_working_set_bytes": p.peak_working_set,
            "virtual_bytes": p.virtual_size,
            "paged_pool_bytes": p.paged_pool,
            "nonpaged_pool_bytes": p.nonpaged_pool,
            "page_faults_last_interval": p.page_faults_delta,
            "hard_faults": p.hard_faults,
        },
        "threads": p.threads,
        "handles": p.handles,
        "handles_per_minute": round(p.handles_per_min as f64, 1),
        "io": {
            "read_bps": p.io_read_rate.round(),
            "write_bps": p.io_write_rate.round(),
            "other_bps": p.io_other_rate.round(),
            "read_bytes_total": p.io.read_bytes,
            "write_bytes_total": p.io.write_bytes,
            "other_bytes_total": p.io.other_bytes,
            "read_ops_total": p.io.read_ops,
            "write_ops_total": p.io.write_ops,
        },
        "disk": {
            "read_bps": p.disk_read_rate.round(),
            "write_bps": p.disk_write_rate.round(),
            "read_bytes_total": p.disk.as_ref().map(|d| d.read_bytes),
            "write_bytes_total": p.disk.as_ref().map(|d| d.write_bytes),
        },
        "network": {
            "receive_bps": p.net_rx_rate.round(),
            "send_bps": p.net_tx_rate.round(),
            "received_bytes_total": p.net_rx_total,
            "sent_bytes_total": p.net_tx_total,
        },
        "gpu_percent": pct(p.gpu),
    })
}

pub fn cpu_times(c: &CpuTimes) -> Value {
    json!({
        "total_percent": pct(c.total),
        "user_percent": pct(c.user),
        "kernel_percent": pct(c.kernel),
        "dpc_percent": pct(c.dpc),
        "interrupt_percent": pct(c.interrupt),
        "mhz": c.mhz.round(),
    })
}

/// The live process with this PID (the newest, when the PID was reused).
pub fn find(snap: &Snapshot, pid: u32) -> Option<&ProcessInfo> {
    snap.processes
        .iter()
        .filter(|p| p.pid == pid)
        .max_by_key(|p| (p.state != ProcState::Exited, p.key.create_time))
}

pub fn no_process(pid: u32) -> String {
    format!("No process with PID {pid} is running (list_processes shows what runs now).")
}
