//! Recording what processes do, Process Monitor style, for an MCP client:
//! every file, registry key, connection and program a process (or the
//! whole machine) touches for some seconds, from a kernel trace. Nothing is
//! opened. One recording at a time: kernel sessions are few.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anabolic_core::activity::{ActivityEvent, ActivityRecorder, Group, RecorderState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::handler::{AnabolicMcp, fail, ok, progress};
use super::json;
use crate::settings::McpKind;

/// Steps kept per recording at most; the counts go on past it.
const KEPT: usize = 200_000;

/// Whether a recording for an MCP client runs.
#[derive(Default)]
pub struct Recordings {
    busy: bool,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RecordArgs {
    /// Record this process (and, with with_children, the ones under it and
    /// what they start).
    pub pid: Option<u32>,
    /// With pid: the processes under it too (default true).
    pub with_children: Option<bool>,
    /// Record every running process of this name, e.g. "chrome.exe".
    pub name: Option<String>,
    /// Record the whole machine (what a process repeats within a second is
    /// one step with a count; at most 30 seconds).
    pub everything: Option<bool>,
    /// Seconds to record: 1 to 120 (default 10).
    pub seconds: Option<u32>,
    /// Kinds of steps to keep: process, file, registry, network (default all).
    pub kinds: Option<Vec<String>>,
    /// Words a step's process, path or detail must all contain; "-word"
    /// leaves out the steps that contain it.
    pub filter: Option<String>,
    /// Only the steps that failed (access denied, not found...).
    pub only_failures: Option<bool>,
    /// Only the steps that change something (writes, deletes, renames, set
    /// values, starts).
    pub only_changes: Option<bool>,
    /// Steps returned at most, the latest kept (default 300, at most 3000).
    pub limit: Option<u32>,
}

/// Steps kept from a recording: the filters, as the Activity page reads
/// them.
struct Keep {
    groups: Vec<Group>,
    words: Vec<String>,
    unwords: Vec<String>,
    failures: bool,
    changes: bool,
}

impl Keep {
    fn new(args: &RecordArgs) -> Result<Keep, String> {
        let mut groups = Vec::new();
        for kind in args.kinds.iter().flatten() {
            groups.push(match kind.to_lowercase().as_str() {
                "process" | "processes" => Group::Process,
                "file" | "files" => Group::File,
                "registry" => Group::Registry,
                "network" => Group::Network,
                other => {
                    return Err(format!(
                        "Unknown kind {other}: process, file, registry or network."
                    ));
                }
            });
        }
        let (mut words, mut unwords) = (Vec::new(), Vec::new());
        for word in args.filter.as_deref().unwrap_or("").split_whitespace() {
            let word = word.to_lowercase();
            match word.strip_prefix('-') {
                Some("") => {}
                Some(not) => unwords.push(not.to_string()),
                None => words.push(word),
            }
        }
        Ok(Keep {
            groups,
            words,
            unwords,
            failures: args.only_failures.unwrap_or(false),
            changes: args.only_changes.unwrap_or(false),
        })
    }

    fn keeps(&self, e: &ActivityEvent, process: &str) -> bool {
        if !self.groups.is_empty() && !self.groups.contains(&e.kind.group()) {
            return false;
        }
        if (self.failures && !e.failed()) || (self.changes && !e.kind.changes()) {
            return false;
        }
        if self.words.is_empty() && self.unwords.is_empty() {
            return true;
        }
        let text = format!("{process} {} {}", e.path, e.detail).to_lowercase();
        self.words.iter().all(|w| text.contains(w))
            && !self.unwords.iter().any(|w| text.contains(w))
    }
}

/// Clears the busy mark however the recording ends.
struct Busy<'a>(&'a std::sync::Mutex<Recordings>);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        if let Ok(mut r) = self.0.lock() {
            r.busy = false;
        }
    }
}

#[tool_router(router = activity_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = "Records what a process (with the processes under it), every process of a \
                       name, or the whole machine does for some seconds, step by step like \
                       Process Monitor: programs started, DLLs loaded, files opened, read, \
                       written, renamed and deleted, registry keys and values, connections, each \
                       with its result. From a kernel trace: nothing is opened or injected. \
                       Reports progress every second; returns the counts and the steps kept \
                       (filter them with kinds, filter, only_failures, only_changes).",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn record_activity(
        &self,
        Parameters(args): Parameters<RecordArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Record) {
            return refused;
        }
        let keep = match Keep::new(&args) {
            Ok(k) => k,
            Err(e) => return fail(e),
        };
        let everything = args.everything.unwrap_or(false);
        let targets = [args.pid.is_some(), args.name.is_some(), everything];
        if targets.iter().filter(|t| **t).count() != 1 {
            return fail("Give exactly one of pid, name or everything=true.");
        }
        let snap = match args.pid {
            Some(pid) => self.snapshot_with(pid).await,
            None => self.snapshot().await,
        };
        let mut names: HashMap<u32, Arc<str>> = snap
            .processes
            .iter()
            .map(|p| (p.pid, p.name.clone()))
            .collect();
        let pids: Vec<u32> = if let Some(pid) = args.pid {
            if json::find(&snap, pid).is_none() {
                return fail(json::no_process(pid));
            }
            if args.with_children.unwrap_or(true) {
                crate::activity_page::tree_pids(&snap, pid)
            } else {
                vec![pid]
            }
        } else if let Some(name) = &args.name {
            let pids: Vec<u32> = snap
                .processes
                .iter()
                .filter(|p| p.state != anabolic_core::model::ProcState::Exited)
                .filter(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| p.pid)
                .collect();
            if pids.is_empty() {
                return fail(format!("No process named {name} runs now."));
            }
            pids
        } else {
            Vec::new()
        };
        let secs = args
            .seconds
            .unwrap_or(10)
            .clamp(1, if everything { 30 } else { 120 });
        let limit = args.limit.unwrap_or(300).clamp(1, 3000) as usize;
        {
            let mut r = self
                .shared
                .recordings
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if r.busy {
                return fail("A recording for an MCP client is running already: one at a time.");
            }
            r.busy = true;
        }
        let _busy = Busy(&self.shared.recordings);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<(u32, usize)>(4);
        let target_pids = pids.clone();
        let work = tokio::task::spawn_blocking(move || {
            let recorder = if everything {
                ActivityRecorder::everything()
            } else {
                ActivityRecorder::attach_all(&target_pids)
            };
            let mut recorder = recorder?;
            let mut events: Vec<ActivityEvent> = Vec::new();
            let mut total = 0usize;
            let mut counts: HashMap<&'static str, usize> = HashMap::new();
            let mut take = |batch: Vec<ActivityEvent>, events: &mut Vec<ActivityEvent>| {
                for e in batch {
                    total += 1;
                    *counts.entry(e.kind.label()).or_default() += 1;
                    if events.len() < KEPT {
                        events.push(e);
                    }
                }
            };
            for second in 1..=secs {
                std::thread::sleep(Duration::from_secs(1));
                if let RecorderState::Failed(e) = recorder.state() {
                    return Err(e);
                }
                take(recorder.take(), &mut events);
                let _ = tx.blocking_send((second, events.len()));
            }
            take(recorder.stop(), &mut events);
            Ok((events, total, counts))
        });
        while let Some((second, steps)) = rx.recv().await {
            progress(
                &context,
                second as f64,
                Some(secs as f64),
                format!("recording: {second} of {secs} s, {steps} steps"),
            )
            .await;
        }
        let (events, total, counts) = match work.await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return fail(format!("Could not record: {e}")),
            Err(e) => return fail(format!("The recording stopped: {e}")),
        };
        // Programs started during the recording get their names.
        for e in &events {
            if let Some(pid) = crate::activity_page::started_pid(e) {
                names.insert(pid, crate::activity_page::file_name(&e.path).into());
            }
        }
        let name_of = |pid: u32| names.get(&pid).cloned().unwrap_or_else(|| "?".into());
        let kept: Vec<&ActivityEvent> = events
            .iter()
            .filter(|e| keep.keeps(e, &name_of(e.pid)))
            .collect();
        let skipped = kept.len().saturating_sub(limit);
        let mut paths: HashMap<&str, usize> = HashMap::new();
        let mut failures = 0;
        for e in &kept {
            *paths.entry(&e.path).or_default() += e.count as usize;
            failures += usize::from(e.failed());
        }
        let mut top: Vec<_> = paths.into_iter().filter(|(p, _)| !p.is_empty()).collect();
        top.sort_by_key(|t| std::cmp::Reverse(t.1));
        let mut counts: Vec<_> = counts.into_iter().collect();
        counts.sort_by_key(|c| std::cmp::Reverse(c.1));
        ok(json!({
            "recorded_seconds": secs,
            "processes": if everything { Value::String("every process".into()) } else {
                json!(pids.iter().map(|p| json!({ "pid": p, "name": &*name_of(*p) })).collect::<Vec<_>>())
            },
            "steps_recorded": total,
            "steps_by_operation": counts.iter().map(|(k, n)| json!({ "operation": k, "steps": n })).collect::<Vec<_>>(),
            "steps_matching": kept.len(),
            "failures_matching": failures,
            "most_touched_paths": top.iter().take(15).map(|(p, n)| json!({ "path": p, "times": n })).collect::<Vec<_>>(),
            "earlier_steps_left_out": skipped,
            "steps": kept[skipped..].iter().map(|e| json!({
                "at": crate::fmt::clock(e.at, true),
                "pid": e.pid,
                "process": &*name_of(e.pid),
                "operation": e.kind.label(),
                "path": &*e.path,
                "detail": e.detail,
                "result": e.result_text(),
                "times": e.count,
                "bytes": e.bytes,
            })).collect::<Vec<_>>(),
        }))
    }
}
