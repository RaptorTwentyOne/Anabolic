// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Where a process spends its processor time, as the Profile tab samples
//! it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anabolic_core::profile::Profile;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::handler::{AnabolicMcp, fail, ok, progress};
use super::json;
use crate::settings::McpKind;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ProfileArgs {
    /// The process to sample.
    pub pid: u32,
    /// How long to sample, in seconds (1 to 30, default 5).
    pub seconds: Option<u32>,
    /// How many functions and modules to list, the most time first (1 to
    /// 100, default 25).
    pub top: Option<u32>,
}

const DESCRIPTION: &str = "Samples where a process spends its processor time, for a few \
seconds (1 to 30, default 5): the system's own profiler records what each processor runs a \
thousand times a second, with the call stack, and the samples of this process are summed. \
Returns the functions taking the most time (their own time, and with the functions they call, \
as shares of the samples), the modules, the threads, and the hottest call path from the \
outermost call in. Use it when a process uses much CPU and the question is what in it does; \
get_thread_stacks gives one moment's stacks instead. Windows only, as administrator. The \
process is not opened: the kernel walks its stacks, and names come from the symbols on this PC \
(the modules' PDB files) or their exports; code without either is named by its module. It \
runs a kernel trace for the time asked, which costs some CPU while it runs.";

fn pct(p: &Profile, n: u64) -> f64 {
    (f64::from(p.share(n)) * 10.0).round() / 10.0
}

/// The call path that holds the most samples, from the outermost call in.
fn hot_path(p: &Profile) -> Vec<Value> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(&c) = p.tree[at].children.first() {
        let node = &p.tree[c as usize];
        if node.total * 20 < p.samples || out.len() >= 40 {
            break;
        }
        out.push(json!({
            "call": p.frames[node.frame as usize].label(),
            "with_calls_percent": pct(p, node.total),
            "own_percent": pct(p, node.own),
        }));
        at = c as usize;
    }
    out
}

#[tool_router(router = profile_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = DESCRIPTION,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn profile_process(
        &self,
        Parameters(args): Parameters<ProfileArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Trace) {
            return refused;
        }
        let snap = self.snapshot_with(args.pid).await;
        let Some(process) = json::find(&snap, args.pid) else {
            return fail(json::no_process(args.pid));
        };
        let name = process.name.to_string();
        let secs = args.seconds.unwrap_or(5).clamp(1, 30);
        let top = args.top.unwrap_or(25).clamp(1, 100) as usize;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let pid = args.pid;
        let work = tokio::task::spawn_blocking(move || {
            anabolic_core::profile::profile(pid, secs as f32, &|| flag.load(Ordering::Relaxed))
        });
        for second in 1..=secs {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if context.ct.is_cancelled() {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            progress(
                &context,
                f64::from(second),
                Some(f64::from(secs)),
                format!("sampling {name}: {second} of {secs} s"),
            )
            .await;
        }
        let p = match work.await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return fail(format!("Could not sample it: {e}")),
            Err(e) => return fail(format!("The sampling stopped: {e}")),
        };
        let functions: Vec<Value> = p
            .functions
            .iter()
            .take(top)
            .map(|h| {
                let f = &p.frames[h.frame as usize];
                json!({
                    "function": if f.function.is_empty() { None } else { Some(&*f.function) },
                    "module": &*f.module,
                    "kernel": f.kernel,
                    "own_percent": pct(&p, h.own),
                    "with_calls_percent": pct(&p, h.total),
                })
            })
            .collect();
        let modules: Vec<Value> = p
            .modules
            .iter()
            .take(top)
            .map(|m| {
                json!({
                    "module": &*m.module,
                    "own_percent": pct(&p, m.own),
                    "with_calls_percent": pct(&p, m.total),
                })
            })
            .collect();
        let threads: Vec<Value> = p
            .threads
            .iter()
            .take(20)
            .map(|&(tid, n)| json!({ "tid": tid, "percent": pct(&p, n) }))
            .collect();
        ok(json!({
            "pid": pid,
            "name": name,
            "seconds": (p.took.as_secs_f64() * 10.0).round() / 10.0,
            "samples": p.samples,
            "functions": functions,
            "modules": modules,
            "threads": threads,
            "hot_path": hot_path(&p),
            "notes": p.notes,
        }))
    }
}
