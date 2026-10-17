//! Tools about Windows services and the programs that start with Windows,
//! read the way the Services and Startup pages read them.

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
    /// The service's name (as in `sc query`), not its display name.
    pub name: String,
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
        description = "Windows services: name, display name, state, startup type, the process \
                       each runs in (and whether that process hosts others). Filter by text and \
                       state.",
        annotations(read_only_hint = true, open_world_hint = false)
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
        annotations(read_only_hint = true, open_world_hint = false)
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
        description = "Programs that start with Windows: Run keys, Startup folders and tasks at \
                       sign-in or boot, with their command, publisher, whether each is on, \
                       whether its file is missing, and what it costs now if it runs.",
        annotations(read_only_hint = true, open_world_hint = false)
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
                json!({
                    "name": i.name,
                    "where": i.source.label(),
                    "location": i.location,
                    "command": i.command,
                    "program": i.program,
                    "enabled": i.enabled,
                    "publisher": i.publisher,
                    "description": i.description,
                    "part_of_windows": i.windows,
                    "file_missing": i.missing,
                    "running_now": running.len(),
                    "cpu_percent_now": super::json::pct(running.iter().map(|p| p.cpu).sum()),
                    "private_bytes_now": running.iter().map(|p| p.private_bytes).sum::<u64>(),
                })
            })
            .collect();
        ok(json!({ "items": items, "errors": list.errors }))
    }
}
