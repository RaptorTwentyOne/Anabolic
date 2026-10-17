//! Tools that change something: end, suspend and resume processes, set
//! their priority, cores and efficiency mode; control services; close
//! connections and block programs; turn startup programs on and off. Each
//! needs its kind allowed on the MCP page. Windows' critical processes and
//! Anabolic itself are never touched; a game or anti-cheat program is acted
//! on only with force=true after the client told its user the risk.

use std::collections::HashMap;
use std::sync::Arc;

use anabolic_core::actions::{Action, Outcome, Priority, Target};
use anabolic_core::services::{ServiceOp, StartType};
use anabolic_core::{ProcessInfo, Snapshot};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ErrorData, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::json;

use super::handler::{AnabolicMcp, blocking, fail, ok};
use super::{McpEvent, json};
use crate::settings::McpKind;

/// Ending or suspending these takes Windows down or signs the user out.
const CRITICAL: [&str; 11] = [
    "system",
    "registry",
    "secure system",
    "memory compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
];

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EndArgs {
    /// The process ID.
    pub pid: u32,
    /// Also every process under it (they end first).
    #[serde(default)]
    pub tree: bool,
    /// True only after the user agreed to the risk this tool described.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PidForce {
    /// The process ID.
    pub pid: u32,
    /// True only after the user agreed to the risk this tool described.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PriorityArgs {
    pub pid: u32,
    /// idle, below_normal, normal, above_normal, high or realtime.
    pub priority: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AffinityArgs {
    pub pid: u32,
    /// The logical processors it may run on, from 0 (get_cpu lists them).
    pub cpus: Vec<u32>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EfficiencyArgs {
    pub pid: u32,
    /// On: Windows runs it at low priority on efficient cores and clocks.
    pub on: bool,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ServiceControl {
    /// The service's name (as in `sc query`).
    pub name: String,
    /// start, stop, restart, pause or continue.
    pub action: String,
    /// For stop and restart: also stop the running services that depend on
    /// it (otherwise Windows refuses while any runs).
    #[serde(default)]
    pub with_dependents: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ServiceStartup {
    pub name: String,
    /// automatic, automatic_delayed, manual or disabled.
    pub startup: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnectionArgs {
    /// The process that owns the connection.
    pub pid: u32,
    /// The other end's IPv4 address.
    pub remote: String,
    pub remote_port: u16,
    pub local_port: u16,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProgramPath {
    /// The program's full path (get_process gives it).
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StartupToggle {
    /// The item's name, as list_startup_items gives it.
    pub name: String,
    /// Where it is, when two items share the name (list_startup_items' location).
    #[serde(default)]
    pub location: Option<String>,
    pub enabled: bool,
}

fn target(p: &ProcessInfo) -> Target {
    Target {
        key: p.key,
        name: p.name.clone(),
    }
}

/// What the action thread's answer means for the client.
fn outcome(result: Option<Outcome>) -> Result<CallToolResult, ErrorData> {
    match result {
        None => ok(json!({ "done": true })),
        Some(Outcome::Info(message)) => ok(json!({ "done": true, "result": message })),
        Some(Outcome::Error(message)) => fail(message),
        Some(Outcome::Refused { message, .. }) => fail(message),
        Some(Outcome::Affinity {
            process_mask,
            system_mask,
            ..
        }) => ok(json!({
            "done": true,
            "cpus": (0..64).filter(|i| process_mask & (1u64 << i) != 0).collect::<Vec<_>>(),
            "system_cpus": system_mask.count_ones(),
        })),
        Some(Outcome::Firewall { path, blocked }) => ok(json!({
            "done": true, "program": path, "blocked": blocked,
        })),
        Some(other) => ok(json!({ "done": true, "result": format!("{other:?}") })),
    }
}

/// Every process under `index`, the deepest first, so children end before
/// their parents.
fn under(snap: &Snapshot, root: usize) -> Vec<usize> {
    let parents = crate::tree::parents(&snap.processes);
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children.entry(*parent as usize).or_default().push(i);
        }
    }
    fn walk(i: usize, children: &HashMap<usize, Vec<usize>>, out: &mut Vec<usize>) {
        for &kid in children.get(&i).into_iter().flatten() {
            walk(kid, children, out);
            out.push(kid);
        }
    }
    let mut out = Vec::new();
    walk(root, &children, &mut out);
    out
}

impl AnabolicMcp {
    /// The process to act on, or the refusal: not allowed, unknown, one
    /// that must never be touched, or a game or anti-cheat program without
    /// force.
    async fn actionable(
        &self,
        kind: McpKind,
        pid: u32,
        force: bool,
        doing: &str,
    ) -> Result<(Arc<Snapshot>, ProcessInfo), Result<CallToolResult, ErrorData>> {
        if let Some(refused) = self.refuse(kind) {
            return Err(refused);
        }
        let snap = self.snapshot_with(pid).await;
        let Some(p) = json::find(&snap, pid).cloned() else {
            return Err(fail(json::no_process(pid)));
        };
        let lower = p.name.to_lowercase();
        if CRITICAL.contains(&lower.as_str()) || pid <= 4 {
            return Err(fail(format!(
                "{} is critical to Windows: doing this would crash Windows or sign the user \
                 out. Anabolic refuses it.",
                p.name
            )));
        }
        if pid == snap.own_pid {
            return Err(fail(
                "That is Anabolic itself: it does not do this to itself.",
            ));
        }
        let in_front =
            anabolic_core::foreground::front().is_some_and(|f| f.pid == pid && f.fullscreen);
        let risky = if crate::gamemode::untouchable(&p.name) {
            Some(
                "It is one of Windows' own processes or a known anti-cheat program: the game it \
                 protects may close, and an online game can take it as tampering (a kick or a \
                 ban).",
            )
        } else if in_front {
            Some("It is the full-screen program in front, most likely a game being played.")
        } else {
            None
        };
        if let Some(why) = risky
            && let Some(unconfirmed) = self.confirm(
                force,
                &format!("This would {doing} {} (PID {pid}). {why}", p.name),
            )
        {
            return Err(unconfirmed);
        }
        Ok((snap, p))
    }

    async fn act(&self, action: Action) -> Result<CallToolResult, ErrorData> {
        outcome(blocking(move || anabolic_core::actions::perform(action)).await)
    }
}

#[tool_router(router = action_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = "Ends a process, or with tree=true it and every process under it (those \
                       first). Unsaved work in it is lost. Windows' critical processes and \
                       Anabolic are refused; a game or anti-cheat program needs force=true after \
                       the user agreed.",
        annotations(
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn end_process(
        &self,
        Parameters(args): Parameters<EndArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (snap, p) = match self
            .actionable(McpKind::End, args.pid, args.force, "end")
            .await
        {
            Ok(found) => found,
            Err(refused) => return refused,
        };
        let mut targets = Vec::new();
        if args.tree
            && let Some(root) = snap.processes.iter().position(|x| x.key == p.key)
        {
            for i in under(&snap, root) {
                let child = &snap.processes[i];
                // Exited or ending already: nothing left to end.
                let live = child.state != anabolic_core::model::ProcState::Exited && !child.ending;
                if live
                    && !CRITICAL.contains(&child.name.to_lowercase().as_str())
                    && child.pid != snap.own_pid
                {
                    targets.push(target(child));
                }
            }
        }
        targets.push(target(&p));
        let result =
            blocking(move || anabolic_core::actions::perform(Action::Terminate(targets))).await;
        // Short-lived children (a ping, a shell) can end on their own while
        // the tree is ended; Windows then reports them as ending already.
        if let Some(Outcome::Refused { keys, message }) = &result {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let now = self.shared.snapshot.load_full();
            let gone = keys.iter().all(|k| {
                now.processes
                    .iter()
                    .find(|x| x.key == *k)
                    .is_none_or(|x| x.state == anabolic_core::model::ProcState::Exited)
            });
            if gone {
                return ok(json!({
                    "done": true,
                    "note": format!(
                        "{} of them ended on their own meanwhile ({message}); none is left.",
                        keys.len()
                    ),
                }));
            }
        }
        outcome(result)
    }

    #[tool(
        description = "Suspends every thread of a process: it stops using the CPU until resumed \
                       (its windows stop responding meanwhile). Critical processes and Anabolic \
                       are refused; a game or anti-cheat program needs force=true.",
        annotations(
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn suspend_process(
        &self,
        Parameters(args): Parameters<PidForce>,
    ) -> Result<CallToolResult, ErrorData> {
        match self
            .actionable(McpKind::Control, args.pid, args.force, "suspend")
            .await
        {
            Ok((_, p)) => self.act(Action::Suspend(target(&p))).await,
            Err(refused) => refused,
        }
    }

    #[tool(
        description = "Resumes a suspended process.",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn resume_process(
        &self,
        Parameters(args): Parameters<PidForce>,
    ) -> Result<CallToolResult, ErrorData> {
        match self
            .actionable(McpKind::Control, args.pid, args.force, "resume")
            .await
        {
            Ok((_, p)) => self.act(Action::Resume(target(&p))).await,
            Err(refused) => refused,
        }
    }

    #[tool(
        description = "Sets a process' priority class: idle, below_normal, normal, \
                       above_normal, high or realtime (realtime can starve the rest of the \
                       system).",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_priority(
        &self,
        Parameters(args): Parameters<PriorityArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let priority = match args
            .priority
            .to_lowercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "idle" | "low" => Priority::Idle,
            "below_normal" => Priority::BelowNormal,
            "normal" => Priority::Normal,
            "above_normal" => Priority::AboveNormal,
            "high" => Priority::High,
            "realtime" | "real_time" => Priority::Realtime,
            other => {
                return fail(format!(
                    "Unknown priority {other}: idle, below_normal, normal, above_normal, high or \
                     realtime."
                ));
            }
        };
        match self
            .actionable(McpKind::Control, args.pid, args.force, "reprioritise")
            .await
        {
            Ok((_, p)) => self.act(Action::SetPriority(target(&p), priority)).await,
            Err(refused) => refused,
        }
    }

    #[tool(
        description = "Sets which logical processors a process may run on (its affinity).",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_affinity(
        &self,
        Parameters(args): Parameters<AffinityArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.cpus.is_empty() || args.cpus.iter().any(|c| *c >= 64) {
            return fail("cpus must list one or more logical processors from 0 to 63.");
        }
        let mask = args.cpus.iter().fold(0u64, |m, c| m | (1u64 << c));
        match self
            .actionable(
                McpKind::Control,
                args.pid,
                args.force,
                "change the cores of",
            )
            .await
        {
            Ok((_, p)) => self.act(Action::SetAffinity(target(&p), mask)).await,
            Err(refused) => refused,
        }
    }

    #[tool(
        description = "Turns Windows' efficiency mode on or off for a process (low priority, \
                       efficient cores and clocks: EcoQoS).",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_efficiency_mode(
        &self,
        Parameters(args): Parameters<EfficiencyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self
            .actionable(
                McpKind::Control,
                args.pid,
                args.force,
                "change the efficiency mode of",
            )
            .await
        {
            Ok((_, p)) => {
                self.act(Action::SetEfficiencyMode(target(&p), args.on))
                    .await
            }
            Err(refused) => refused,
        }
    }

    #[tool(
        description = "Starts, stops, restarts, pauses or continues a Windows service. Stopping \
                       one that others depend on fails unless with_dependents=true.",
        annotations(
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn control_service(
        &self,
        Parameters(args): Parameters<ServiceControl>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Services) {
            return refused;
        }
        let dependents = args.with_dependents;
        let op = match args.action.to_lowercase().as_str() {
            "start" => ServiceOp::Start,
            "stop" => ServiceOp::Stop { dependents },
            "restart" => ServiceOp::Restart { dependents },
            "pause" => ServiceOp::Pause,
            "continue" | "resume" => ServiceOp::Continue,
            other => {
                return fail(format!(
                    "Unknown action {other}: start, stop, restart, pause or continue."
                ));
            }
        };
        self.service_op(args.name, op).await
    }

    #[tool(
        description = "Sets how a Windows service starts: automatic, automatic_delayed, manual \
                       or disabled.",
        annotations(
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_service_startup(
        &self,
        Parameters(args): Parameters<ServiceStartup>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Services) {
            return refused;
        }
        let start = match args
            .startup
            .to_lowercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "automatic" | "auto" => StartType::Automatic,
            "automatic_delayed" | "delayed" => StartType::AutomaticDelayed,
            "manual" => StartType::Manual,
            "disabled" => StartType::Disabled,
            other => {
                return fail(format!(
                    "Unknown startup type {other}: automatic, automatic_delayed, manual or \
                     disabled."
                ));
            }
        };
        self.service_op(args.name, ServiceOp::SetStart(start)).await
    }

    #[tool(
        description = "Closes one TCP connection (IPv4) of a process, as TCPView does: the \
                       process is not opened; the program usually reconnects if it wants to.",
        annotations(
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn close_connection(
        &self,
        Parameters(args): Parameters<ConnectionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Network) {
            return refused;
        }
        let Ok(remote) = args.remote.parse::<std::net::Ipv4Addr>() else {
            return fail("remote must be an IPv4 address (IPv6 connections cannot be closed).");
        };
        let snap = self.snapshot_with(args.pid).await;
        let Some(p) = json::find(&snap, args.pid) else {
            return fail(json::no_process(args.pid));
        };
        self.act(Action::CloseConnection {
            pid: args.pid,
            process: p.name.clone(),
            remote,
            remote_port: args.remote_port,
            local_port: args.local_port,
        })
        .await
    }

    #[tool(
        description = "Blocks a program from the network with two Windows Firewall rules \
                       (outbound and inbound), until unblock_program. Windows' own files cannot \
                       be blocked. The Network page lists what is blocked.",
        annotations(
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn block_program(
        &self,
        Parameters(ProgramPath { path }): Parameters<ProgramPath>,
    ) -> Result<CallToolResult, ErrorData> {
        self.firewall(path, true).await
    }

    #[tool(
        description = "Removes the firewall rules block_program (or the Network page) made for \
                       a program.",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn unblock_program(
        &self,
        Parameters(ProgramPath { path }): Parameters<ProgramPath>,
    ) -> Result<CallToolResult, ErrorData> {
        self.firewall(path, false).await
    }

    #[tool(
        description = "Turns a startup program on or off the way Task Manager does (nothing is \
                       deleted; tasks through Task Scheduler).",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_startup_item(
        &self,
        Parameters(args): Parameters<StartupToggle>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Startup) {
            return refused;
        }
        let list = blocking(anabolic_core::startup::list).await;
        let found: Vec<_> = list
            .items
            .iter()
            .filter(|i| i.name.eq_ignore_ascii_case(&args.name))
            .filter(|i| {
                args.location
                    .as_deref()
                    .is_none_or(|l| i.location.eq_ignore_ascii_case(l))
            })
            .cloned()
            .collect();
        let item = match found.as_slice() {
            [one] => one.clone(),
            [] => return fail(format!("No startup item is named {}.", args.name)),
            _ => {
                return fail(format!(
                    "{} items are named {}: give location too (list_startup_items has it).",
                    found.len(),
                    args.name
                ));
            }
        };
        let on = args.enabled;
        match blocking(move || anabolic_core::startup::set_enabled(&item, on)).await {
            Ok(message) => ok(json!({ "done": true, "result": message })),
            Err(e) => fail(e),
        }
    }
}

impl AnabolicMcp {
    async fn service_op(&self, name: String, op: ServiceOp) -> Result<CallToolResult, ErrorData> {
        let list = blocking(anabolic_core::services::query).await;
        let Some(s) = list
            .services
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&name))
        else {
            return fail(format!("No service is named {name}."));
        };
        let (name, display) = (s.name.to_string(), s.display.to_string());
        let (result, dependents) =
            blocking(move || anabolic_core::services::control_now(&name, &display, op)).await;
        match result {
            Ok(message) => ok(json!({ "done": true, "result": message })),
            Err(e) if !dependents.is_empty() => fail(format!(
                "{e} Running services depend on it: {}. Call again with with_dependents=true \
                 to stop them too.",
                dependents.join(", ")
            )),
            Err(e) => fail(e),
        }
    }

    async fn firewall(&self, path: String, block: bool) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Network) {
            return refused;
        }
        let result = blocking({
            let path = path.clone();
            move || anabolic_core::actions::perform(Action::Firewall { path, block })
        })
        .await;
        if let Some(Outcome::Firewall { path, blocked }) = &result {
            self.shared.event(McpEvent::Blocked {
                path: path.clone(),
                blocked: *blocked,
            });
        }
        outcome(result)
    }
}
