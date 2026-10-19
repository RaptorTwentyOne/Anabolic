//! Handle inspection: the handles (Windows) or file descriptors (Linux) a
//! process holds, what they point at, and who holds a given file.
//!
//! Counting reads system-wide tables, so the inspected process is never
//! opened (games with anti-cheat stay untouched). Resolving names is
//! opt-in, and on demand.

use crate::model::ProcKey;
use crate::modules::ModuleInfo;
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg_attr(windows, path = "windows/handles.rs")]
#[cfg_attr(target_os = "linux", path = "linux/handles.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, Debug)]
pub struct HandleEntry {
    pub value: u64,
    pub type_index: u16,
    pub access: u32,
    pub attributes: u32,
    pub object: u64,
}

#[derive(Clone, Debug)]
pub struct TypeCount {
    pub name: Arc<str>,
    pub type_index: u16,
    pub count: u32,
}

#[derive(Debug)]
pub struct HandleReport {
    pub key: ProcKey,
    pub entries: Vec<HandleEntry>,
    /// Largest first.
    pub by_type: Vec<TypeCount>,
    pub type_names: HashMap<u16, Arc<str>>,
    /// Parallel to `entries` (prefix of length `resolved`) when names were requested.
    pub names: Vec<Option<Arc<str>>>,
    pub resolved: usize,
    /// (type, name, count) over the resolved names, largest first.
    pub name_groups: Vec<(u16, Arc<str>, u32)>,
    pub name_error: Option<String>,
    /// Handles open in the whole system at the time.
    pub system_total: usize,
    pub process_targets: Option<ProcessTargets>,
    pub took: Duration,
}

impl HandleReport {
    pub fn type_name(&self, index: u16) -> Arc<str> {
        self.type_names
            .get(&index)
            .cloned()
            .unwrap_or_else(|| Arc::from(format!("Type #{index}")))
    }
}

#[derive(Clone, Debug)]
pub struct ObjectTypeInfo {
    pub name: Arc<str>,
    /// Objects of this type alive in the whole system.
    pub objects: u32,
    pub handles: u32,
}

/// Object type index -> name ("File", "Event", ...).
pub fn object_type_names() -> HashMap<u16, Arc<str>> {
    object_types()
        .into_iter()
        .map(|(i, t)| (i, t.name))
        .collect()
}

/// What a process' Process handles point at.
#[derive(Clone, Debug, Default)]
pub struct ProcessTargets {
    pub handles: usize,
    pub distinct: usize,
    pub running: usize,
    /// Exited processes that stay in the kernel because of these handles.
    pub exited: usize,
    pub handles_to_exited: usize,
    /// (pid, handles) for the running processes with the most handles.
    pub top_running: Vec<(u32, usize)>,
}

#[derive(Clone, Debug)]
pub struct ZombieHolder {
    pub pid: u32,
    /// Distinct exited processes this process keeps alive.
    pub zombies: usize,
    pub handles: usize,
}

/// Who keeps exited processes alive by holding handles to them.
#[derive(Clone, Debug)]
pub struct ZombieReport {
    pub process_objects: u32,
    pub running: usize,
    /// Distinct exited processes referenced by any handle.
    pub zombies: usize,
    pub holders: Vec<ZombieHolder>,
    pub took: Duration,
}

pub fn count_by_type(entries: &[HandleEntry], names: &HashMap<u16, Arc<str>>) -> Vec<TypeCount> {
    let mut counts: HashMap<u16, u32> = HashMap::new();
    for e in entries {
        *counts.entry(e.type_index).or_default() += 1;
    }
    let mut out: Vec<TypeCount> = counts
        .into_iter()
        .map(|(type_index, count)| TypeCount {
            name: names
                .get(&type_index)
                .cloned()
                .unwrap_or_else(|| Arc::from(format!("Type #{type_index}"))),
            type_index,
            count,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    out
}

#[derive(Clone, Debug)]
pub struct FoundHandle {
    pub pid: u32,
    pub process: Arc<str>,
    pub handle: u64,
    pub type_name: Arc<str>,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct FindReport {
    pub query: String,
    pub matches: Vec<FoundHandle>,
    pub processes: usize,
    pub handles: usize,
    /// (name, pid, why)
    pub skipped: Vec<(Arc<str>, u32, String)>,
    pub took: Duration,
}

pub enum InspectRequest {
    Handles {
        key: ProcKey,
        resolve_names: bool,
    },
    Zombies,
    /// Loaded modules, plus names for these code addresses (thread starts).
    Modules {
        key: ProcKey,
        addresses: Vec<u64>,
        /// Threads whose names (SetThreadDescription) to read.
        tids: Vec<u32>,
    },
    /// The call stack of a thread (suspends it for a few milliseconds).
    Stack {
        key: ProcKey,
        tid: u32,
    },
    /// Signature, hash and build facts of a file.
    FileFacts {
        path: Arc<str>,
    },
    /// Processes holding handles to a kernel object, `key`'s left out.
    Holders {
        key: ProcKey,
        object: u64,
    },
    FindHandles {
        query: String,
        exclude: Vec<String>,
    },
    /// What the process' threads wait for; `first` is analysed first (the
    /// thread of a window that is not responding). Runs on a thread of its
    /// own: it can take seconds, and must not hold up the others.
    WaitChain {
        key: ProcKey,
        first: Option<u32>,
    },
    /// Drops the wait chain analysis under way.
    StopWaitChain,
    /// Why the process does not go away (`ending::why_alive`).
    WhyAlive {
        key: ProcKey,
    },
}

pub enum Inspection {
    Handles(Arc<HandleReport>),
    Zombies(Arc<ZombieReport>),
    Modules(Arc<ModuleReport>),
    Found(Arc<FindReport>),
    WaitChain {
        key: ProcKey,
        report: Result<Arc<crate::waits::WaitReport>, String>,
    },
    WhyAlive {
        key: ProcKey,
        report: Result<Arc<crate::ending::EndReport>, String>,
    },
    Stack {
        key: ProcKey,
        tid: u32,
        report: Result<Arc<crate::threads::StackReport>, String>,
    },
    FileFacts {
        path: Arc<str>,
        facts: Result<Arc<crate::filefacts::FileFacts>, String>,
    },
    Holders {
        key: ProcKey,
        object: u64,
        holders: Result<Vec<crate::ending::Holder>, String>,
    },
    /// How far the wait chain analysis of `key` got.
    WaitProgress {
        key: ProcKey,
        done: usize,
        total: usize,
    },
    /// `key` is set when a handle inspection failed.
    Failed {
        key: Option<ProcKey>,
        what: String,
        error: String,
    },
}

#[derive(Clone, Debug)]
pub struct ModuleRow {
    pub info: ModuleInfo,
    pub description: Option<String>,
    pub company: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug)]
pub struct ModuleReport {
    pub key: ProcKey,
    pub modules: Vec<ModuleRow>,
    /// The process' own modules could not be read (kernel ones still resolve).
    pub error: Option<String>,
    pub symbols: HashMap<u64, String>,
    /// Names programs gave their threads.
    pub thread_names: HashMap<u32, String>,
    pub took: Duration,
}

/// Runs on-demand, heavier inspections (the system handle table alone can be
/// tens of megabytes) on its own thread.
pub struct Inspector {
    tx: Sender<InspectRequest>,
    /// `None` stops the analysis under way.
    waits: Sender<Option<(ProcKey, Option<u32>)>>,
}

/// Up to `NAME_LIMIT` names: counted here, not on the UI thread.
fn name_groups(entries: &[HandleEntry], names: &[Option<Arc<str>>]) -> Vec<(u16, Arc<str>, u32)> {
    let unnamed: Arc<str> = Arc::from("<unnamed>");
    let mut counts: HashMap<(u16, Arc<str>), u32> = HashMap::new();
    for (e, name) in entries.iter().zip(names) {
        let name = name.clone().unwrap_or_else(|| unnamed.clone());
        *counts.entry((e.type_index, name)).or_default() += 1;
    }
    let mut groups: Vec<_> = counts.into_iter().map(|((t, n), c)| (t, n, c)).collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.2));
    groups
}

impl Inspector {
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> (Self, Receiver<Inspection>) {
        let (tx, rx) = unbounded::<InspectRequest>();
        let (waits, wait_rx) = unbounded();
        let (out, results) = unbounded();
        let notify = Arc::new(notify);
        {
            let (out, notify) = (out.clone(), notify.clone());
            thread::Builder::new()
                .name("anabolic-waits".into())
                .spawn(move || wait_chains(wait_rx, out, &*notify))
                .expect("failed to spawn the wait chain thread");
        }
        thread::Builder::new()
            .name("anabolic-inspector".into())
            .spawn(move || {
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::BelowNormal,
                );
                let mut bench = imp::Bench::new();
                while let Ok(request) = rx.recv() {
                    // Of each kind, only the most recent request matters.
                    let mut queue = vec![request];
                    while let Ok(newer) = rx.try_recv() {
                        let kind = std::mem::discriminant(&newer);
                        match queue.iter().position(|r| std::mem::discriminant(r) == kind) {
                            Some(i) => queue[i] = newer,
                            None => queue.push(newer),
                        }
                    }
                    for request in queue {
                        let message = match request {
                            InspectRequest::Handles { key, resolve_names } => {
                                match bench.handles(key, resolve_names) {
                                    Ok(report) => Inspection::Handles(Arc::new(report)),
                                    Err(error) => Inspection::Failed {
                                        key: Some(key),
                                        what: format!("handles of PID {}", key.pid),
                                        error,
                                    },
                                }
                            }
                            InspectRequest::Modules {
                                key,
                                addresses,
                                tids,
                            } => {
                                Inspection::Modules(Arc::new(bench.modules(key, &addresses, &tids)))
                            }
                            InspectRequest::Stack { key, tid } => Inspection::Stack {
                                key,
                                tid,
                                report: bench.stack(key, tid).map(Arc::new),
                            },
                            InspectRequest::FileFacts { path } => Inspection::FileFacts {
                                facts: crate::filefacts::file_facts(&path).map(Arc::new),
                                path,
                            },
                            InspectRequest::Holders { key, object } => Inspection::Holders {
                                key,
                                object,
                                holders: crate::ending::object_holders(
                                    object,
                                    &[key.pid, std::process::id()],
                                ),
                            },
                            InspectRequest::FindHandles { query, exclude } => {
                                match bench.find(&query, &exclude) {
                                    Ok(report) => Inspection::Found(Arc::new(report)),
                                    Err(error) => Inspection::Failed {
                                        key: None,
                                        what: format!("search for \"{query}\""),
                                        error,
                                    },
                                }
                            }
                            InspectRequest::WhyAlive { key } => Inspection::WhyAlive {
                                key,
                                report: crate::ending::why_alive(key).map(Arc::new),
                            },
                            // Sent to their own thread by `request`.
                            InspectRequest::WaitChain { .. } | InspectRequest::StopWaitChain => {
                                continue;
                            }
                            InspectRequest::Zombies => match zombie_report() {
                                Ok(report) => Inspection::Zombies(Arc::new(report)),
                                Err(error) => Inspection::Failed {
                                    key: None,
                                    what: "zombie scan".into(),
                                    error,
                                },
                            },
                        };
                        if out.send(message).is_err() {
                            return;
                        }
                        notify();
                    }
                }
            })
            .expect("failed to spawn the inspector thread");
        (Self { tx, waits }, results)
    }

    pub fn request(&self, request: InspectRequest) {
        // A closed channel means the app is closing: nothing to do.
        match request {
            InspectRequest::WaitChain { key, first } => {
                let _ = self.waits.send(Some((key, first)));
            }
            InspectRequest::StopWaitChain => {
                let _ = self.waits.send(None);
            }
            other => {
                let _ = self.tx.send(other);
            }
        }
    }
}

/// The wait chain thread: one analysis at a time; a newer request (or a
/// stop) ends the one under way, which then answers nothing.
fn wait_chains(
    requests: Receiver<Option<(ProcKey, Option<u32>)>>,
    out: Sender<Inspection>,
    notify: &dyn Fn(),
) {
    const PROGRESS: Duration = Duration::from_millis(250);
    let mut next = None;
    loop {
        let request = match next.take() {
            Some(request) => request,
            None => match requests.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };
        let Some((key, first)) = request else {
            continue;
        };
        let mut told = Instant::now();
        let report = crate::waits::wait_chains(key, first, &mut |done, total| {
            if let Ok(newer) = requests.try_recv() {
                next = Some(newer);
                return false;
            }
            if told.elapsed() >= PROGRESS {
                told = Instant::now();
                let _ = out.send(Inspection::WaitProgress { key, done, total });
                notify();
            }
            true
        });
        if next.is_some() {
            continue;
        }
        if out
            .send(Inspection::WaitChain {
                key,
                report: report.map(Arc::new),
            })
            .is_err()
        {
            return;
        }
        notify();
    }
}
