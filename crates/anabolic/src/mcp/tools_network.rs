//! Tools about the network: every connection with its program and the
//! other end's name, the ports open for others to reach, who sends and
//! receives how much and where to, and the DNS lookups programs made
//! (Windows). From the system's socket tables and its network accounting;
//! no process is opened.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anabolic_core::Snapshot;
use anabolic_core::sockets::Socket;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Trace;
use super::handler::{AnabolicMcp, blocking, fail, ok, progress};
use crate::settings::McpKind;

#[cfg(windows)]
const DNS_LOOKUPS: &str = "DNS lookups programs made (from the DNS client's events): when, which \
                           program, the name asked, the addresses answered and any error. The \
                           latest first. Lookups are seen while the network trace runs: give \
                           trace_seconds to watch for a while.";
/// Linux keeps no record of them: the tool says so.
#[cfg(target_os = "linux")]
const DNS_LOOKUPS: &str = "DNS lookups programs made. Not available on Linux: nothing records \
                           the names programs look up system-wide.";

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ConnectionArgs {
    /// Only this process' connections.
    pub pid: Option<u32>,
    /// Only connections whose program, remote address or remote name
    /// contains this (any case).
    pub filter: Option<String>,
    /// At most this many (default 200, at most 2000).
    pub limit: Option<u32>,
    /// Seconds to trace first (0 to 30, default 0): adds how much each
    /// connection sends and receives. Needs "Turn kernel traces on".
    pub trace_seconds: Option<u32>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct UsageArgs {
    /// Seconds to trace first (0 to 30, default 5): where the traffic goes
    /// needs the network trace. Needs "Turn kernel traces on".
    pub trace_seconds: Option<u32>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct LookupArgs {
    /// Only lookups made by this process.
    pub pid: Option<u32>,
    /// Only names containing this (any case).
    pub filter: Option<String>,
    /// At most this many, the latest first (default 100, at most 1000).
    pub limit: Option<u32>,
    /// Seconds to watch first (0 to 30, default 0): lookups are seen while
    /// the network trace runs (the Network page open, or this). Needs "Turn
    /// kernel traces on".
    pub trace_seconds: Option<u32>,
}

fn names(snap: &Snapshot) -> HashMap<u32, Arc<str>> {
    snap.processes
        .iter()
        .map(|p| (p.pid, p.name.clone()))
        .collect()
}

fn remote_name(snap: &Snapshot, ip: IpAddr) -> Option<String> {
    // Not what a hosts file calls 127.0.0.1.
    if ip.is_loopback() {
        return Some("this computer".into());
    }
    snap.system.dns_names.get(&ip).map(|n| n.to_string())
}

/// Who can reach a port open on this address.
fn reach(ip: IpAddr) -> &'static str {
    if ip.is_unspecified() {
        "every network this PC is on"
    } else if ip.is_loopback() {
        "this computer only"
    } else {
        "the network of that address"
    }
}

impl AnabolicMcp {
    /// Traces the network for `secs` when traces are allowed: whether it
    /// did, and the snapshot after.
    async fn network_traced(
        &self,
        secs: u32,
        context: &RequestContext<RoleServer>,
    ) -> (bool, Arc<Snapshot>) {
        let allowed = self.shared.allowed(McpKind::Trace).is_ok();
        if secs == 0 || !allowed {
            return (false, self.snapshot().await);
        }
        self.shared
            .wants
            .want(Trace::Network, Duration::from_secs(secs as u64 + 5));
        crate::repaint(&self.shared.ctx);
        for done in 0..secs {
            progress(
                context,
                done as f64,
                Some(secs as f64),
                "tracing the network",
            )
            .await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        (true, self.snapshot().await)
    }
}

#[tool_router(router = network_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = "Every TCP connection and UDP endpoint with its program, the other end's \
                       address and name (from what programs looked up, else reverse DNS), ports \
                       and TCP state. With trace_seconds > 0, also each connection's traffic. \
                       Filter by process or by text.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_connections(
        &self,
        Parameters(args): Parameters<ConnectionArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let secs = args.trace_seconds.unwrap_or(0).min(30);
        let (traced, snap) = self.network_traced(secs, &context).await;
        let names = names(&snap);
        let filter = args.filter.as_deref().unwrap_or("").to_lowercase();
        let limit = args.limit.unwrap_or(200).clamp(1, 2000) as usize;
        let process = |pid: u32| names.get(&pid).map(|n| n.to_string());
        let wanted = |pid: u32, ip: &IpAddr, name: &Option<String>| {
            args.pid.is_none_or(|p| p == pid)
                && (filter.is_empty()
                    || process(pid).is_some_and(|n| n.to_lowercase().contains(&filter))
                    || ip.to_string().contains(&filter)
                    || name
                        .as_ref()
                        .is_some_and(|n| n.to_lowercase().contains(&filter)))
        };
        let mut rows: Vec<(f64, Value)> = Vec::new();
        if let Some(activity) = &snap.system.net_activity {
            for c in &activity.connections {
                let name = remote_name(&snap, c.remote);
                if !wanted(c.pid, &c.remote, &name) {
                    continue;
                }
                rows.push((
                    c.rx_rate + c.tx_rate,
                    json!({
                        "pid": c.pid, "process": process(c.pid),
                        "protocol": format!("{:?}", c.proto),
                        "local_port": c.local_port,
                        "remote": c.remote.to_string(), "remote_name": name,
                        "remote_port": c.remote_port,
                        "state": c.state.map(|s| s.label()),
                        "receive_bps": c.rx_rate.round(), "send_bps": c.tx_rate.round(),
                        "received_bytes": c.rx_total, "sent_bytes": c.tx_total,
                    }),
                ));
            }
        } else {
            let (sockets, error) = blocking(anabolic_core::sockets::sockets).await;
            if let Some(e) = error {
                return super::handler::fail(format!("Could not read the socket tables: {e}"));
            }
            for s in sockets.iter().filter(|s: &&Socket| !s.listening()) {
                let Some((ip, port)) = s.remote else { continue };
                let name = remote_name(&snap, ip);
                if !wanted(s.pid, &ip, &name) {
                    continue;
                }
                rows.push((
                    0.0,
                    json!({
                        "pid": s.pid, "process": process(s.pid),
                        "protocol": format!("{:?}", s.proto),
                        "local_port": s.local_port,
                        "remote": ip.to_string(), "remote_name": name, "remote_port": port,
                        "state": s.state.map(|s| s.label()),
                    }),
                ));
            }
        }
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        let total = rows.len();
        ok(json!({
            "connections": rows.into_iter().take(limit).map(|(_, v)| v).collect::<Vec<_>>(),
            "matching": total,
            "with_traffic": snap.system.net_activity.is_some(),
            "traced_seconds": if traced { secs } else { 0 },
            "trace_error": snap.system.net_trace_error,
        }))
    }

    #[tool(
        description = "The ports open on this PC for others to connect to (TCP listening, UDP \
                       bound), with the program behind each and who can reach it: every network, \
                       this computer only, or one network.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_listening_ports(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let names = names(&snap);
        let (sockets, error) = blocking(anabolic_core::sockets::sockets).await;
        let mut ports: Vec<&Socket> = sockets.iter().filter(|s| s.listening()).collect();
        ports.sort_by_key(|s| (s.local_port, s.pid));
        ports.dedup_by(|a, b| {
            a.local_port == b.local_port
                && a.pid == b.pid
                && a.proto == b.proto
                && a.local == b.local
        });
        ok(json!({
            "ports": ports.iter().map(|s| json!({
                "port": s.local_port,
                "protocol": format!("{:?}", s.proto),
                "address": s.local.to_string(),
                "reachable_from": reach(s.local),
                "pid": s.pid,
                "process": names.get(&s.pid).map(|n| n.to_string()),
            })).collect::<Vec<_>>(),
            "error": error,
        }))
    }

    #[tool(
        description = "Who uses the network: each program's receive and send rates and totals, \
                       and (after trace_seconds of the network trace) where the traffic goes, by \
                       destination name, with the programs talking to each.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_network_usage(
        &self,
        Parameters(args): Parameters<UsageArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let secs = args.trace_seconds.unwrap_or(5).min(30);
        let (traced, snap) = self.network_traced(secs, &context).await;
        let names = names(&snap);
        let mut programs: Vec<_> = snap
            .processes
            .iter()
            .filter(|p| p.net_rx_rate + p.net_tx_rate > 0.0 || p.net_rx_total + p.net_tx_total > 0)
            .collect();
        programs.sort_by(|a, b| {
            (b.net_rx_rate + b.net_tx_rate).total_cmp(&(a.net_rx_rate + a.net_tx_rate))
        });
        let mut destinations: HashMap<String, (f64, f64, u64, Vec<u32>)> = HashMap::new();
        if let Some(activity) = &snap.system.net_activity {
            for c in &activity.connections {
                let to = remote_name(&snap, c.remote).unwrap_or_else(|| c.remote.to_string());
                let d = destinations.entry(to).or_default();
                d.0 += c.rx_rate;
                d.1 += c.tx_rate;
                d.2 += c.rx_total + c.tx_total;
                if !d.3.contains(&c.pid) {
                    d.3.push(c.pid);
                }
            }
        }
        let mut destinations: Vec<_> = destinations.into_iter().collect();
        destinations.sort_by(|a, b| {
            (b.1.0 + b.1.1)
                .total_cmp(&(a.1.0 + a.1.1))
                .then(b.1.2.cmp(&a.1.2))
        });
        ok(json!({
            "receive_bps": snap.system.net.rx_rate.round(),
            "send_bps": snap.system.net.tx_rate.round(),
            "programs": programs.iter().take(30).map(|p| json!({
                "pid": p.pid, "name": &*p.name,
                "receive_bps": p.net_rx_rate.round(), "send_bps": p.net_tx_rate.round(),
                "received_bytes": p.net_rx_total, "sent_bytes": p.net_tx_total,
            })).collect::<Vec<_>>(),
            "destinations": destinations.iter().take(30).map(|(to, (rx, tx, total, pids))| json!({
                "destination": to,
                "receive_bps": rx.round(), "send_bps": tx.round(), "bytes": total,
                "programs": pids.iter().map(|p| names.get(p).map(|n| n.to_string())).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "traced_seconds": if traced { secs } else { 0 },
            "trace_error": snap.system.net_trace_error,
        }))
    }

    #[tool(
        description = DNS_LOOKUPS,
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_dns_lookups(
        &self,
        Parameters(args): Parameters<LookupArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        if cfg!(target_os = "linux") {
            return fail(
                "Linux keeps no record of the names programs look up: each program asks through \
                 its own resolver library, and systemd-resolved tells no one who asked.",
            );
        }
        let secs = args.trace_seconds.unwrap_or(0).min(30);
        let (_, snap) = self.network_traced(secs, &context).await;
        let names = names(&snap);
        let filter = args.filter.as_deref().unwrap_or("").to_lowercase();
        let limit = args.limit.unwrap_or(100).clamp(1, 1000) as usize;
        let lookups: Vec<Value> = snap
            .system
            .dns_lookups
            .iter()
            .rev()
            .filter(|l| args.pid.is_none_or(|p| p == l.pid))
            .filter(|l| filter.is_empty() || l.name.to_lowercase().contains(&filter))
            .take(limit)
            .map(|l| {
                json!({
                    "at": crate::fmt::clock(l.at, true),
                    "pid": l.pid,
                    "process": names.get(&l.pid).map(|n| n.to_string()),
                    "name": &*l.name,
                    "record_type": l.kind,
                    "status": l.status,
                    "addresses": l.addresses.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
                    "aliases": l.aliases.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        ok(json!({ "lookups": lookups }))
    }
}
