// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Tools that open a process to look inside: its handles, modules, thread
//! stacks and wait chains, and memory dumps. Each is refused the first time
//! with the risks in words (anti-cheat software above all); the client asks
//! its user and, with their yes, calls again with force=true.

use anabolic_core::{ProcessInfo, Snapshot};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ErrorData, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::handler::{AnabolicMcp, blocking, fail, ok};
use super::json;
use crate::fmt;
use crate::settings::McpKind;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HandleArgs {
    /// The process ID.
    pub pid: u32,
    /// Only handles of this type (File, Key, Event, Process, Thread, Section...).
    #[serde(default)]
    pub kind: Option<String>,
    /// Only handles whose name contains this (any case).
    #[serde(default)]
    pub name: Option<String>,
    /// At most this many (default 200, at most 2000).
    #[serde(default)]
    pub limit: Option<u32>,
    /// True only after the user agreed to the risk this tool described.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InspectArgs {
    /// The process ID.
    pub pid: u32,
    /// True only after the user agreed to the risk this tool described.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StackArgs {
    /// The process ID.
    pub pid: u32,
    /// One thread; left out, its busiest threads.
    #[serde(default)]
    pub tid: Option<u32>,
    /// How many of the busiest threads (default 5, at most 20).
    #[serde(default)]
    pub threads: Option<u32>,
    /// True only after the user agreed to the risk this tool described.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DumpArgs {
    /// The process ID.
    pub pid: u32,
    /// "small" (threads, stacks, modules, handles; a few MB, the default)
    /// or "full" (all of its memory, as big as the process).
    #[serde(default)]
    pub kind: Option<String>,
    /// True only after the user agreed to the risk this tool described.
    #[serde(default)]
    pub force: bool,
}

/// What opening `p` risks, in words for the user.
fn risks(snap: &Snapshot, p: &ProcessInfo, doing: &str) -> String {
    let mut text = format!("This opens {} (PID {}) to {doing}.", p.name, p.pid);
    let draws = snap
        .system
        .frames
        .as_ref()
        .is_some_and(|f| f.processes.iter().any(|g| g.pid == p.pid));
    let in_front =
        anabolic_core::foreground::front().is_some_and(|f| f.pid == p.pid && f.fullscreen);
    if crate::gamemode::untouchable(&p.name) {
        text.push_str(
            " It is one of Windows' core processes or a known anti-cheat program: opening it \
             may fail, and an anti-cheat can take it as tampering (in an online game, a kick or \
             a ban).",
        );
    } else if draws || in_front {
        text.push_str(
            " It looks like a game (it draws frames, or fills the screen in front): if \
             anti-cheat software protects it, opening it can be taken as tampering (a kick or a \
             ban).",
        );
    } else {
        text.push_str(
            " Anti-cheat software protecting a game that runs now can take a program opening \
             other processes badly, even when this one is not the game.",
        );
    }
    text
}

#[tool_router(router = inspect_tools, vis = "pub(super)")]
impl AnabolicMcp {
    /// The live process, or the refusal: not allowed, unknown PID, or not
    /// confirmed yet.
    async fn opened(
        &self,
        kind: McpKind,
        pid: u32,
        force: bool,
        doing: &str,
    ) -> Result<(std::sync::Arc<Snapshot>, ProcessInfo), Result<CallToolResult, ErrorData>> {
        if let Some(refused) = self.refuse(kind) {
            return Err(refused);
        }
        let snap = self.snapshot_with(pid).await;
        let Some(p) = json::find(&snap, pid).cloned() else {
            return Err(fail(json::no_process(pid)));
        };
        if let Some(unconfirmed) = self.confirm(force, &risks(&snap, &p, doing)) {
            return Err(unconfirmed);
        }
        Ok((snap, p))
    }

    #[tool(
        description = "The handles a process holds, by type, with the names of files, registry \
                       keys, events and other objects; for processes held open, which process \
                       each one is. OPENS THE PROCESS (handles are duplicated out of it to read \
                       their names): the first call is refused with the risks; call again with \
                       force=true only if the user agreed.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_handles(
        &self,
        Parameters(args): Parameters<HandleArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, p) = match self
            .opened(
                McpKind::Inspect,
                args.pid,
                args.force,
                "read the names of its handles",
            )
            .await
        {
            Ok(opened) => opened,
            Err(refused) => return refused,
        };
        let key = p.key;
        let report = match blocking(move || anabolic_core::handles::handles_now(key, true)).await {
            Ok(r) => r,
            Err(e) => {
                return fail(format!(
                    "Could not read the handles of PID {}: {e}",
                    args.pid
                ));
            }
        };
        let kind = args.kind.as_deref().map(str::to_lowercase);
        let name = args.name.as_deref().map(str::to_lowercase);
        let limit = args.limit.unwrap_or(200).clamp(1, 2000) as usize;
        let mut matching = 0;
        let mut rows = Vec::new();
        for (i, e) in report.entries.iter().enumerate() {
            let type_name = report.type_name(e.type_index);
            let object = report.names.get(i).cloned().flatten();
            if kind
                .as_ref()
                .is_some_and(|k| type_name.to_lowercase() != *k)
            {
                continue;
            }
            if name.as_ref().is_some_and(|n| {
                !object
                    .as_deref()
                    .is_some_and(|o| o.to_lowercase().contains(n))
            }) {
                continue;
            }
            matching += 1;
            if rows.len() < limit {
                rows.push(json!({
                    "handle": format!("{:#x}", e.value),
                    "type": &*type_name,
                    "name": object.as_deref(),
                    "access": format!("{:#x}", e.access),
                }));
            }
        }
        ok(json!({
            "pid": args.pid,
            "name": &*p.name,
            "handles": report.entries.len(),
            "by_type": report.by_type.iter().map(|t| json!({
                "type": &*report.type_name(t.type_index), "count": t.count,
            })).collect::<Vec<_>>(),
            "matching": matching,
            "shown": rows,
            "names_error": report.name_error,
        }))
    }

    #[tool(
        description = "The DLLs and the program a process has loaded, with their paths, base \
                       addresses and sizes. OPENS THE PROCESS to read its module list: the first \
                       call is refused with the risks; call again with force=true only if the \
                       user agreed.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_modules(
        &self,
        Parameters(args): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, p) = match self
            .opened(
                McpKind::Inspect,
                args.pid,
                args.force,
                "read its list of loaded modules",
            )
            .await
        {
            Ok(opened) => opened,
            Err(refused) => return refused,
        };
        let pid = p.pid;
        let modules = match blocking(move || anabolic_core::modules::process_modules(pid)).await {
            Ok(m) => m,
            Err(e) => return fail(format!("Could not read the modules of PID {pid}: {e}")),
        };
        ok(json!({
            "pid": pid,
            "name": &*p.name,
            "modules": modules.iter().map(|m| json!({
                "name": &*m.name,
                "path": &*m.path,
                "base": format!("{:#x}", m.base),
                "size_bytes": m.size,
            })).collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "Call stacks of a process' threads (one thread, or its busiest ones), with \
                       module!function names where they can be read: where it spends its time or \
                       is stuck. OPENS THE PROCESS and SUSPENDS each thread for a few \
                       milliseconds: the first call is refused with the risks; call again with \
                       force=true only if the user agreed.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_thread_stacks(
        &self,
        Parameters(args): Parameters<StackArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, p) = match self
            .opened(
                McpKind::Inspect,
                args.pid,
                args.force,
                "read its threads' call stacks, suspending each for a few milliseconds",
            )
            .await
        {
            Ok(opened) => opened,
            Err(refused) => return refused,
        };
        let key = p.key;
        let pid = p.pid;
        let wanted = args.threads.unwrap_or(5).clamp(1, 20) as usize;
        let single = args.tid;
        let stacks = blocking(move || {
            let tids: Vec<(u32, i64)> = match single {
                Some(tid) => vec![(tid, 0)],
                None => anabolic_core::threads::by_cpu_time(pid)?
                    .into_iter()
                    .take(wanted)
                    .collect(),
            };
            let mut symbols = anabolic_core::modules::Symbolizer::default();
            Ok::<_, String>(
                tids.into_iter()
                    .map(|(tid, cpu)| {
                        let stack = anabolic_core::threads::call_stack(key, tid, &mut symbols);
                        (tid, cpu, stack)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .await;
        let stacks = match stacks {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        ok(json!({
            "pid": pid,
            "name": &*p.name,
            "threads": stacks.iter().map(|(tid, cpu, stack)| match stack {
                Ok(s) => json!({
                    "tid": tid,
                    "cpu_time_seconds": (*cpu as f64 / 1e7 * 100.0).round() / 100.0,
                    "suspended_ms": s.held.as_secs_f64() * 1000.0,
                    "frames": s.frames.iter().map(|f| match &f.symbol {
                        Some(name) => Value::String(name.clone()),
                        None => Value::String(format!("{:#x}", f.address)),
                    }).collect::<Vec<_>>(),
                }),
                Err(e) => json!({ "tid": tid, "error": e }),
            }).collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "What a process' threads wait for, across processes (Wait Chain \
                       Traversal): locks, other threads, other processes, COM and ALPC calls, \
                       deadlocks first. The answer to \"why is it not responding\". OPENS THE \
                       PROCESS' THREADS: the first call is refused with the risks; call again with \
                       force=true only if the user agreed. Can take seconds.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_wait_chain(
        &self,
        Parameters(args): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (snap, p) = match self
            .opened(
                McpKind::Inspect,
                args.pid,
                args.force,
                "follow what its threads wait for",
            )
            .await
        {
            Ok(opened) => opened,
            Err(refused) => return refused,
        };
        let key = p.key;
        // The thread of a window that is not responding goes first.
        let first = snap
            .system
            .hung
            .iter()
            .find(|h| h.pid == p.pid)
            .map(|h| h.tid);
        let report =
            blocking(move || anabolic_core::waits::wait_chains(key, first, &mut |_, _| true)).await;
        let report = match report {
            Ok(r) => r,
            Err(e) => return fail(format!("Could not follow the waits of PID {}: {e}", p.pid)),
        };
        use anabolic_core::waits::Link;
        ok(json!({
            "pid": p.pid,
            "name": &*p.name,
            "threads_followed": report.threads,
            "threads_idle": report.idle,
            "failed": report.failed,
            "took_ms": report.took.as_millis() as u64,
            "deadlocks": report.chains.iter().filter(|c| c.cycle).count(),
            "chains": report.chains.iter().map(|c| json!({
                "deadlock": c.cycle,
                "links": c.links.iter().map(|l| match l {
                    Link::Thread { pid, tid, status, context_switches } => json!({
                        "thread": tid, "pid": pid, "status": status.label(),
                        "context_switches": context_switches,
                    }),
                    Link::Object { wait, name, status } => json!({
                        "waits_on": wait.label(), "object": name, "status": status.label(),
                    }),
                }).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "Writes a memory dump of a process into Anabolic's dumps folder, for a \
                       debugger: small (threads, stacks, modules, handles) or full (all of its \
                       memory, which includes any passwords and keys it holds). The process is \
                       cloned first, so it pauses only for a moment. Never for lsass.exe. OPENS \
                       THE PROCESS: the first call is refused with the risks; call again with \
                       force=true only if the user agreed.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn write_memory_dump(
        &self,
        Parameters(args): Parameters<DumpArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let kind = match args.kind.as_deref().unwrap_or("small") {
            "small" => anabolic_core::dump::DumpKind::Small,
            "full" => anabolic_core::dump::DumpKind::Full,
            other => return fail(format!("kind must be small or full, not {other}.")),
        };
        let doing = match kind {
            anabolic_core::dump::DumpKind::Full => {
                "copy all of its memory into a file (passwords, keys and documents it holds \
                 included)"
            }
            anabolic_core::dump::DumpKind::Small => {
                "write its threads, stacks, modules and handles into a file"
            }
        };
        let (_, p) = match self
            .opened(McpKind::Dump, args.pid, args.force, doing)
            .await
        {
            Ok(opened) => opened,
            Err(refused) => return refused,
        };
        let Some(dir) = self.shared.data_dir.as_ref().map(|d| d.join("dumps")) else {
            return fail("Anabolic has no data folder here.");
        };
        let (key, name) = (p.key, p.name.to_string());
        let report =
            blocking(move || anabolic_core::dump::write(key, &name, kind, &dir, false)).await;
        match report {
            Ok(r) => ok(json!({
                "pid": p.pid,
                "name": &*p.name,
                "file": r.path.display().to_string(),
                "size_bytes": r.bytes,
                "size": fmt::bytes(r.bytes),
                "took_ms": r.took.as_millis() as u64,
                "from_a_clone": r.cloned,
            })),
            Err(e) => fail(e),
        }
    }
}
