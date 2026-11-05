// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Tools about services (Windows' or systemd's) and the programs that
//! start with Windows or the Linux session, read the way the Services and
//! Startup pages read them.

use std::collections::HashMap;

use anabolic_core::services::Service;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ErrorData, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::handler::{AnabolicMcp, blocking, fail, ok};
use crate::settings::McpKind;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ServiceListArgs {
    /// Only services whose name or display name contains this (any case).
    pub filter: Option<String>,
    /// running, stopped or all (default all).
    pub state: Option<String>,
    /// At most this many (default 300, at most 1000).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ServiceName {
    #[cfg_attr(
        windows,
        doc = "The service's name (as in `sc query`), not its display name."
    )]
    #[cfg_attr(
        target_os = "linux",
        doc = "The unit's name (sshd.service; \"(user)\" after it for the user's own), not \
               its description."
    )]
    pub name: String,
}

/// The key that says an entry is the system's own.
const PART_OF_SYSTEM: &str = if cfg!(windows) {
    "part_of_windows"
} else {
    "part_of_the_system"
};

const LIST_SERVICES: &str = if cfg!(windows) {
    "Windows services: name, display name, state, startup type, the process each runs in (and \
     whether that process hosts others). Filter by text and state."
} else {
    "systemd services, the system's and the user's own (marked \"(user)\"): unit name, \
     description, state, startup type (enabled, disabled, masked, static), the process each \
     runs in. Filter by text and state."
};

const LIST_STARTUP: &str = if cfg!(windows) {
    "Programs that start with Windows: Run keys, Startup folders and tasks at sign-in or boot, \
     with their command, publisher, whether each is on, whether its file is missing, and what \
     it costs now if it runs."
} else {
    "Programs that start with the session: autostart entries, systemd user units and Plasma's \
     scripts, with their command, whether each is on, whether its program is missing, and what \
     it costs now if it runs."
};

const BOOTS: &str = "Lists the machine's last boots, newest first, from the system's own \
logs. Each start in its steps: the firmware, the system's loader, Windows to the sign-in screen, \
the sign-in, and the sign-in to the desktop, with Group Policy's time and the slow sign-in steps \
(the time the sign-in screen waited for someone is apart). How the session before it ended: \
clean, a stop error (blue screen) with its code and dump, the power button held, or not shut down \
at all with when it was last seen running; who asked for a shutdown or restart (the program, as \
which user, the reason, its words: Windows Update, an installer, the user), how long shutting down \
took and how long the machine was off. What failed as it started (services that did not start or \
answer, drivers that did not load), and how the session went: how long it ran, its sleeps, the \
services that stopped unexpectedly. Where Windows keeps its own boot measurement it adds that \
and what it found slower than usual; on Linux, this boot's timings and slowest units from \
systemd. Use it when a PC starts slowly, restarted by itself or keeps crashing; \
list_startup_items lists what starts at sign-in. It only reads.";

/// The system's part of a boot, by its name on each system.
const READY_KEY: &str = if cfg!(windows) {
    "to_sign_in_seconds"
} else {
    "to_desktop_seconds"
};

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct BootArgs {
    /// How many boots, newest first (1 to 60, default 15).
    pub limit: Option<u32>,
}

/// Milliseconds as seconds with a tenth.
fn seconds(ms: u64) -> f64 {
    (ms as f64 / 100.0).round() / 10.0
}

fn boot_row(b: &anabolic_core::boot::Boot) -> Value {
    use anabolic_core::boot::{BootKind, Shutdown};
    let mut row = json!({
        "started": anabolic_core::sysinfo::local_time(b.started),
        "kind": match b.kind {
            BootKind::Cold => "full",
            BootKind::FastStartup => "fast_startup",
            BootKind::Hibernation => "from_hibernation",
            BootKind::Unknown => "unknown",
        },
        "firmware_seconds": b.firmware_ms.map(seconds),
        "shutdown_before": match b.before {
            Shutdown::Clean => "clean",
            Shutdown::Crashed(_) => "stop_error",
            Shutdown::PowerButton => "power_button_held",
            Shutdown::Unexpected => "not_shut_down",
            Shutdown::Unknown => "unknown",
        },
    });
    row[READY_KEY] = json!(b.ready_ms.map(seconds));
    if let Shutdown::Crashed(code) = b.before {
        row["stop_code"] = json!(format!("0x{code:X}"));
    }
    if let Some(m) = b.measured {
        row["windows_measure"] = json!({
            "whole_seconds": seconds(m.total_ms),
            "to_desktop_seconds": seconds(m.main_path_ms),
            "after_desktop_seconds": seconds(m.after_ms),
        });
    }
    let steps = [
        ("loader_seconds", b.loader_ms),
        ("to_sign_in_screen_seconds", b.to_screen_ms),
        ("sign_in_to_desktop_seconds", b.to_desktop_ms),
        ("group_policy_at_start_seconds", b.policy_start_ms),
        ("group_policy_at_sign_in_seconds", b.policy_sign_in_ms),
    ];
    for (key, ms) in steps {
        if let Some(ms) = ms {
            row[key] = json!(seconds(ms));
        }
    }
    if !b.sign_in_steps.is_empty() {
        row["slow_sign_in_steps"] = json!(
            b.sign_in_steps
                .iter()
                .map(|(step, ms)| json!({ "step": step, "seconds": seconds(*ms) }))
                .collect::<Vec<_>>()
        );
    }
    let e = &b.ended;
    let mut ended = serde_json::Map::new();
    let texts = [
        ("asked_by_program", &e.by),
        ("as_user", &e.user),
        ("action", &e.action),
        ("reason", &e.reason),
        ("comment", &e.comment),
        ("stop_error", &e.crash),
        ("dump", &e.dump),
    ];
    for (key, value) in texts {
        if let Some(v) = value {
            ended.insert(key.into(), json!(v));
        }
    }
    if let Some(t) = e.asked_at {
        ended.insert(
            "asked_at".into(),
            json!(anabolic_core::sysinfo::local_time(t)),
        );
    }
    if let Some(ms) = e.shutdown_ms {
        ended.insert("shutting_down_seconds".into(), json!(seconds(ms)));
    }
    if let Some(t) = e.last_alive {
        ended.insert(
            "last_seen_running".into(),
            json!(anabolic_core::sysinfo::local_time(t)),
        );
    }
    if let Some(ms) = e.off_ms {
        ended.insert("off_seconds".into(), json!(seconds(ms)));
    }
    if !ended.is_empty() {
        row["session_before"] = Value::Object(ended);
    }
    let problem = |p: &anabolic_core::boot::Problem| json!({ "after_seconds": seconds(p.after_ms), "what": p.what });
    if !b.problems.is_empty() {
        row["problems_at_start"] = json!(b.problems.iter().map(problem).collect::<Vec<_>>());
    }
    row["session"] = json!({
        "lasted_seconds": b.session.lasted_ms.map(seconds),
        "sleeps": b.session.sleeps,
        "services_stopped_unexpectedly": b.session.crashed.iter().map(problem).collect::<Vec<_>>(),
    });
    if !b.slowed.is_empty() {
        row["slowest"] = json!(
            b.slowed
                .iter()
                .map(|s| json!({
                    "kind": s.kind.label(),
                    "name": s.name,
                    "took_seconds": seconds(s.took_ms),
                    "more_than_usual_seconds": seconds(s.over_ms),
                }))
                .collect::<Vec<_>>()
        );
    }
    row
}

fn service_row(s: &Service, process: Option<&str>) -> Value {
    json!({
        "name": &*s.name,
        "display_name": &*s.display,
        "state": s.state.label(),
        "pid": (s.pid != 0).then_some(s.pid),
        "process": process,
        "shares_its_process": s.shared,
        "startup": s.config.as_ref().map(|c| c.start.label()),
        "can_stop": s.accepts_stop,
        "can_pause": s.accepts_pause,
    })
}

#[tool_router(router = service_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = LIST_SERVICES,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_services(
        &self,
        Parameters(args): Parameters<ServiceListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let names: HashMap<u32, &str> = snap.processes.iter().map(|p| (p.pid, &*p.name)).collect();
        let list = blocking(anabolic_core::services::query).await;
        let filter = args.filter.as_deref().unwrap_or("").to_lowercase();
        let state = args.state.as_deref().unwrap_or("all").to_lowercase();
        let limit = args.limit.unwrap_or(300).clamp(1, 1000) as usize;
        let rows: Vec<Value> = list
            .services
            .iter()
            .filter(|s| {
                filter.is_empty()
                    || s.name.to_lowercase().contains(&filter)
                    || s.display.to_lowercase().contains(&filter)
            })
            .filter(|s| match state.as_str() {
                "running" => s.pid != 0,
                "stopped" => s.pid == 0,
                _ => true,
            })
            .take(limit)
            .map(|s| service_row(s, names.get(&s.pid).copied()))
            .collect();
        ok(json!({ "services": rows, "error": list.error }))
    }

    #[tool(
        description = "One service in full: state, process, startup type (and whether a trigger \
                       starts it), its program and arguments, the account it runs as, its \
                       description, what it depends on and which services depend on it.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_service(
        &self,
        Parameters(ServiceName { name }): Parameters<ServiceName>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let list = blocking(anabolic_core::services::query).await;
        let Some(s) = list
            .services
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&name))
        else {
            return fail(format!(
                "No service is named {name} (list_services finds names by display name too)."
            ));
        };
        let process = snap.processes.iter().find(|p| p.pid == s.pid && s.pid != 0);
        let dependents: Vec<&str> = list
            .services
            .iter()
            .filter(|d| {
                d.config.as_ref().is_some_and(|c| {
                    c.dependencies
                        .iter()
                        .any(|n| n.eq_ignore_ascii_case(&s.name))
                })
            })
            .map(|d| &*d.name)
            .collect();
        let mut row = service_row(s, process.map(|p| &*p.name));
        if let Some(c) = &s.config {
            row["started_by_trigger"] = json!(c.triggered);
            row["program"] = json!(c.binary);
            row["account"] = json!(c.account);
            row["description"] = json!(c.description);
            row["depends_on"] = json!(c.dependencies);
        }
        row["depended_on_by"] = json!(dependents);
        row["exit_code"] = json!(s.exit_code);
        if let Some(p) = process {
            row["process_cpu_percent"] = json!(super::json::pct(p.cpu));
            row["process_private_bytes"] = json!(p.private_bytes);
        }
        ok(row)
    }

    #[tool(
        description = LIST_STARTUP,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_startup_items(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let snap = self.snapshot().await;
        let list = blocking(anabolic_core::startup::list).await;
        let items: Vec<Value> = list
            .items
            .iter()
            .map(|i| {
                let file = i
                    .program
                    .as_deref()
                    .and_then(|p| p.rsplit(['\\', '/']).next())
                    .map(str::to_lowercase);
                let running: Vec<_> = snap
                    .processes
                    .iter()
                    .filter(|p| file.as_deref() == Some(&*p.name.to_lowercase()))
                    .collect();
                let mut row = json!({
                    "name": i.name,
                    "where": i.source.label(),
                    "location": i.location,
                    "command": i.command,
                    "program": i.program,
                    "enabled": i.enabled,
                    "publisher": i.publisher,
                    "description": i.description,
                    "file_missing": i.missing,
                    "running_now": running.len(),
                    "cpu_percent_now": super::json::pct(running.iter().map(|p| p.cpu).sum()),
                    "private_bytes_now": running.iter().map(|p| p.private_bytes).sum::<u64>(),
                });
                row[PART_OF_SYSTEM] = json!(i.system);
                row
            })
            .collect();
        ok(json!({ "items": items, "errors": list.errors }))
    }

    #[tool(
        description = BOOTS,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_boot_history(
        &self,
        Parameters(args): Parameters<BootArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let limit = args.limit.unwrap_or(15).clamp(1, 60) as usize;
        let read = match blocking(anabolic_core::boot::read).await {
            Ok(read) => read,
            Err(e) => return fail(format!("The boots could not be read: {e}")),
        };
        let shown = &read.boots[..read.boots.len().min(limit)];
        let mut out = json!({
            "boots": shown.iter().map(boot_row).collect::<Vec<_>>(),
            "not_shut_down_cleanly_before": shown.iter().filter(|b| b.before.bad()).count(),
        });
        if let Some(note) = &read.note {
            out["note"] = json!(note);
        }
        ok(out)
    }
}
