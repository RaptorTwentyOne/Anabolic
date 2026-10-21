//! Tools about processes, read without opening them: the list, one
//! process in full (what started it, what it hosts, its connections), the
//! tree, why one will not end, and the ones that keep growing.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use anabolic_core::model::ProcState;
use anabolic_core::{ProcessInfo, Snapshot};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ErrorData, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::handler::{AnabolicMcp, blocking, fail, ok};
use super::json::{self, Users, pct};
use crate::fmt;
use crate::settings::McpKind;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ListArgs {
    /// Only processes whose name contains this (any case), or whose PID it is.
    pub filter: Option<String>,
    /// Order: cpu (default), memory, disk, network, gpu, io, handles, threads,
    /// name, pid or started (newest first).
    pub sort: Option<String>,
    /// At most this many (default 40, at most 500).
    pub limit: Option<u32>,
    /// Also the processes that exited since the last refresh.
    pub include_exited: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Pid {
    /// The process ID.
    pub pid: u32,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct TreeArgs {
    /// The process at the top; the whole machine when left out.
    pub pid: Option<u32>,
    /// Levels below the top (default 6, at most 20).
    pub depth: Option<u32>,
}

/// The value a list is ordered by, the largest first.
fn order_value(p: &ProcessInfo, sort: &str) -> f64 {
    match sort {
        "memory" => p.private_bytes as f64,
        "disk" => p.disk_read_rate + p.disk_write_rate,
        "network" => p.net_rx_rate + p.net_tx_rate,
        "gpu" => p.gpu as f64,
        "io" => p.io_total_rate(),
        "handles" => p.handles as f64,
        "threads" => p.threads as f64,
        "started" => p.key.create_time as f64,
        _ => p.cpu as f64,
    }
}

fn alive(p: &ProcessInfo) -> bool {
    p.state != ProcState::Exited
}

/// Names the other ends of connections: what programs looked up first.
fn remote_name(snap: &Snapshot, ip: IpAddr) -> Value {
    // Not what a hosts file calls 127.0.0.1.
    if ip.is_loopback() {
        return Value::String("this computer".into());
    }
    snap.system
        .dns_names
        .get(&ip)
        .map_or(Value::Null, |n| Value::String(n.to_string()))
}

/// A process' connections and listening ports: from the network trace's
/// table when it runs (with traffic), else read from Windows' socket tables.
async fn connections_of(snap: &Snapshot, pid: u32) -> (Vec<Value>, Vec<Value>) {
    if let Some(activity) = &snap.system.net_activity {
        let connections = activity
            .connections
            .iter()
            .filter(|c| c.pid == pid)
            .map(|c| {
                json!({
                    "protocol": format!("{:?}", c.proto),
                    "remote": c.remote.to_string(),
                    "remote_name": remote_name(snap, c.remote),
                    "remote_port": c.remote_port,
                    "local_port": c.local_port,
                    "state": c.state.map(|s| s.label()),
                    "receive_bps": c.rx_rate.round(),
                    "send_bps": c.tx_rate.round(),
                    "received_bytes": c.rx_total,
                    "sent_bytes": c.tx_total,
                })
            })
            .collect();
        let listening = activity
            .listeners
            .iter()
            .filter(|l| l.pid == pid)
            .map(|l| json!({ "protocol": format!("{:?}", l.proto), "address": l.local.to_string(), "port": l.port }))
            .collect();
        return (connections, listening);
    }
    let (sockets, _) = blocking(anabolic_core::sockets::sockets).await;
    let mut connections = Vec::new();
    let mut listening = Vec::new();
    for s in sockets.iter().filter(|s| s.pid == pid) {
        if s.listening() {
            listening.push(json!({
                "protocol": format!("{:?}", s.proto),
                "address": s.local.to_string(),
                "port": s.local_port,
            }));
        } else if let Some((ip, port)) = s.remote {
            connections.push(json!({
                "protocol": format!("{:?}", s.proto),
                "remote": ip.to_string(),
                "remote_name": remote_name(snap, ip),
                "remote_port": port,
                "local_port": s.local_port,
                "state": s.state.map(|s| s.label()),
            }));
        }
    }
    (connections, listening)
}

/// The tree below `index`, `depth` levels deep, at most `left` nodes.
fn subtree(
    snap: &Snapshot,
    children: &HashMap<usize, Vec<usize>>,
    index: usize,
    depth: u32,
    left: &mut usize,
) -> Value {
    let p = &snap.processes[index];
    *left = left.saturating_sub(1);
    let mut node = json!({
        "pid": p.pid,
        "name": &*p.name,
        "cpu_percent": pct(p.cpu),
        "private_bytes": p.private_bytes,
    });
    if let Some(kids) = children.get(&index) {
        if depth == 0 || *left == 0 {
            node["children_not_shown"] = json!(kids.len());
        } else {
            let mut shown = Vec::new();
            for &kid in kids {
                if *left == 0 {
                    break;
                }
                shown.push(subtree(snap, children, kid, depth - 1, left));
            }
            if shown.len() < kids.len() {
                node["children_not_shown"] = json!(kids.len() - shown.len());
            }
            node["children"] = Value::Array(shown);
        }
    }
    node
}

#[tool_router(router = process_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = "The processes running now, one line each: PID, parent, name, CPU, \
                       private memory, disk, network and GPU rates, threads, handles, priority, \
                       user, start time, and whether it is not responding. Filter by name or PID, \
                       order by a resource.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_processes(
        &self,
        Parameters(args): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let sort = args.sort.as_deref().unwrap_or("cpu").to_lowercase();
        let filter = args
            .filter
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_lowercase();
        let limit = args.limit.unwrap_or(40).clamp(1, 500) as usize;
        let with_exited = args.include_exited.unwrap_or(false);
        let mut rows: Vec<&ProcessInfo> = snap
            .processes
            .iter()
            .filter(|p| with_exited || alive(p))
            // Idle time is not a process at work; it shows when asked for.
            .filter(|p| p.pid != 0 || filter == "0")
            .filter(|p| {
                filter.is_empty()
                    || p.name.to_lowercase().contains(&filter)
                    || p.pid.to_string() == filter
            })
            .collect();
        match sort.as_str() {
            "name" => rows.sort_by_key(|p| p.name.to_lowercase()),
            "pid" => rows.sort_by_key(|p| p.pid),
            _ => rows.sort_by(|a, b| order_value(b, &sort).total_cmp(&order_value(a, &sort))),
        }
        let total = rows.len();
        let picked: Vec<ProcessInfo> = rows.into_iter().take(limit).cloned().collect();
        let processes = blocking(move || {
            let mut users = Users::default();
            picked
                .iter()
                .map(|p| json::process_row(p, Some(&mut users)))
                .collect::<Vec<_>>()
        })
        .await;
        ok(json!({
            "matching": total,
            "shown": processes.len(),
            "sorted_by": sort,
            "processes": processes,
        }))
    }

    #[tool(
        description = text::PROCESS,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_process(
        &self,
        Parameters(Pid { pid }): Parameters<Pid>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot_with(pid).await;
        let Some(p) = json::find(&snap, pid).cloned() else {
            return fail(json::no_process(pid));
        };
        let key = p.key;
        let sid = p.user_sid.clone();
        let details = blocking(move || anabolic_core::enrich::details_of(key, sid)).await;
        let (reply, answer) = crossbeam_channel::bounded(1);
        let _ = self
            .shared
            .collector
            .send(anabolic_core::collector::Command::Ancestry(key, reply));
        let ancestry = blocking(move || answer.recv_timeout(Duration::from_secs(3)).ok())
            .await
            .unwrap_or_default();
        let services = blocking(anabolic_core::services::query).await;
        let hosted: Vec<Value> = services
            .services
            .iter()
            .filter(|s| s.pid == pid && pid != 0)
            .map(|s| json!({ "name": &*s.name, "display_name": &*s.display, "state": s.state.label() }))
            .collect();
        let how = crate::origin::how(&p.name, pid, &ancestry.chain, Some(&services));
        let (connections, listening) = connections_of(&snap, pid).await;
        let children: Vec<Value> = snap
            .processes
            .iter()
            .filter(|c| c.ppid == pid && alive(c) && c.key.create_time >= p.key.create_time)
            .map(|c| json!({ "pid": c.pid, "name": &*c.name }))
            .collect();
        let leaks: Vec<Value> = self
            .shared
            .leaks
            .load()
            .iter()
            .filter(|l| l.key == key)
            .map(|l| json!({ "metric": l.metric.label(), "per_hour": l.per_hour.round(), "in_words": fmt::leak(l) }))
            .collect();
        let hung = snap
            .system
            .hung
            .iter()
            .find(|h| h.pid == pid)
            .map(|h| &h.title);
        let mut out = json::process_counters(&p);
        out["user"] = json!(details.user);
        out["file"] = json!({
            "path": details.path,
            "description": details.description,
            "company": details.company,
            "version": details.version,
        });
        out["command_line"] = json!(details.command_line);
        out["architecture"] = json!(details.arch.map(|a| a.label()));
        out["elevated"] = json!(details.elevated);
        out["integrity"] = json!(details.integrity.map(|i| i.label()));
        out["critical_to_windows"] = json!(details.critical);
        if let Some(e) = &details.open_error {
            out["could_not_read"] = json!(e);
        }
        out["started_by"] = json!(
            ancestry
                .chain
                .iter()
                .map(|a| json!({
                    "pid": a.key.pid,
                    "name": &*a.name,
                    "file": a.image.as_deref(),
                    "started": json::filetime(a.key.create_time),
                    "exited": a.exited.map(|(at, _)| json::filetime(at)),
                    "exit_code": a.exited.and_then(|(_, code)| code),
                    "elevated": a.elevated,
                }))
                .collect::<Vec<_>>()
        );
        out["how_it_was_started"] = json!(how);
        out["started_processes"] = json!(children);
        out["services_hosted"] = json!(hosted);
        out["connections"] = json!(connections);
        out["listening"] = json!(listening);
        out["growing"] = json!(leaks);
        if let Some(title) = hung {
            out["not_responding_window"] = json!(title);
        }
        ok(out)
    }

    #[tool(
        description = "The process tree: who started whom, with CPU and private memory, from a \
                       process down (or the whole machine). Parents that exited are left out; a \
                       reused PID does not make a false parent.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_process_tree(
        &self,
        Parameters(args): Parameters<TreeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = match args.pid {
            Some(pid) => self.snapshot_with(pid).await,
            None => self.snapshot().await,
        };
        let parents = crate::tree::parents(&snap.processes);
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut roots = Vec::new();
        for (i, parent) in parents.iter().enumerate() {
            if !alive(&snap.processes[i]) {
                continue;
            }
            match parent {
                Some(parent) => children.entry(*parent as usize).or_default().push(i),
                None => roots.push(i),
            }
        }
        let depth = args.depth.unwrap_or(6).min(20);
        let mut left = 400;
        let tops: Vec<usize> = match args.pid {
            Some(pid) => match snap.processes.iter().position(|p| p.pid == pid && alive(p)) {
                Some(i) => vec![i],
                None => return fail(json::no_process(pid)),
            },
            None => roots,
        };
        let tree: Vec<Value> = tops
            .into_iter()
            .map(|i| subtree(&snap, &children, i, depth, &mut left))
            .collect();
        ok(json!({ "tree": tree, "truncated": left == 0 }))
    }

    #[tool(
        description = text::WHY_ALIVE,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn why_process_will_not_end(
        &self,
        Parameters(Pid { pid }): Parameters<Pid>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot_with(pid).await;
        let Some(p) = json::find(&snap, pid) else {
            return fail(json::no_process(pid));
        };
        let key = p.key;
        let report = match blocking(move || anabolic_core::ending::why_alive(key)).await {
            Ok(r) => r,
            Err(e) => return fail(format!("Could not look into PID {pid}: {e}")),
        };
        ok(json!({
            "pid": pid,
            "name": &*p.name,
            "being_deleted": report.flags.deleting,
            "protected": report.flags.protected,
            "protection": anabolic_core::ending::protection_label(report.protection),
            "exit_status": format!("{:#x}", report.flags.exit_status),
            "threads": report.threads,
            "can_be_ended": report.can_end,
            "could_not_open": report.open_error,
            "held_by": report.holders.iter().map(|h| json!({
                "pid": h.pid, "name": &*h.name, "handles": h.handles.len(),
                "access": format!("{:#x}", h.access),
            })).collect::<Vec<_>>(),
            "guarded_by": report.guards.iter().map(|(driver, what)| json!({
                "driver": driver, "what": what,
            })).collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "Processes whose handles, private memory or threads keep growing in a \
                       straight line (sampled every minute for two hours, flagged after ten \
                       minutes or more): the usual sign of a leak.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_leaks(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let leaks = self.shared.leaks.load();
        ok(json!({ "growing": leaks.iter().map(|l| json!({
            "pid": l.key.pid,
            "name": &*l.name,
            "metric": l.metric.label(),
            "per_hour": l.per_hour.round(),
            "from": l.from.round(),
            "to": l.to.round(),
            "over_seconds": l.span.as_secs(),
            "in_words": fmt::leak(l),
        })).collect::<Vec<_>>() }))
    }
}

/// Tool descriptions that differ between Windows and Linux.
mod text {
    #[cfg(windows)]
    pub const PROCESS: &str = "One process in full: every counter (CPU, memory in detail, I/O, disk, \
                       network, handles), its file, command line, publisher, version, user, \
                       elevation and integrity; who started it and their parents (named even \
                       after they exited) and how it was most likely started; the processes it \
                       started; the services it hosts; its connections and listening ports; and \
                       whether its handles, memory or threads keep growing. Nothing is injected; \
                       the process is only queried for its file and command line.";
    #[cfg(target_os = "linux")]
    pub const PROCESS: &str = "One process in full: every counter (CPU, memory in detail, I/O, disk, network, open \
     files), its file, command line, package, version, user and rights; who started it and \
     their parents (named even after they exited) and how it was most likely started; the \
     processes it started; the services it runs; its connections and listening ports; and \
     whether its open files, memory or threads keep growing. Nothing is injected; its files in \
     /proc are only read.";
    #[cfg(windows)]
    pub const WHY_ALIVE: &str = "Why a process does not go away: whether Windows is still ending it, its \
                       threads, whether Windows protects it, whether it can be ended at all, which \
                       programs hold handles to it (keeping it as a zombie), and which drivers \
                       guard it (anti-cheat and security software, named). Reads the system's \
                       handle table; the process is only queried.";
    #[cfg(target_os = "linux")]
    pub const WHY_ALIVE: &str = "Why a process does not go away: whether it has ended and waits for its parent to collect \
     it (a zombie, and which parent), whether it waits inside the kernel where no signal \
     reaches it (and where), whether Anabolic may end it at all, and whether it is the \
     system's own. The process is only read about, not opened.";
}
