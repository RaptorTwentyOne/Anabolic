// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Activity page: what a program does, step by step, Process Monitor
//! style. Start a program and record it from its first step, record a
//! running one, or record the whole computer and narrow it down afterwards
//! with the filters: every file opened, read, written, renamed or deleted,
//! every registry key and value (Windows), every connection and every
//! program started, in order, and on Linux every system call when asked.
//! From what the kernel reports: nothing is injected or opened.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anabolic_core::activity::{
    ActivityEvent, ActivityKind, ActivityRecorder, ActivitySummary, Group, RecorderState,
    SyscallStat,
};
use anabolic_core::{ProcKey, ProcState, Snapshot};
use crossbeam_channel::Receiver;
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, Sense, TextEdit, Ui};

use crate::proc_table::{UiCmd, item};
use crate::table::{Columns, Spec, TableView};
use crate::{fmt, panes, theme, widgets};

#[cfg(windows)]
const GROUPS: &[(Group, &str)] = &[
    (Group::Process, "Processes"),
    (Group::File, "Files"),
    (Group::Registry, "Registry"),
    (Group::Network, "Network"),
];
/// Linux has no registry.
#[cfg(target_os = "linux")]
const GROUPS: &[(Group, &str)] = &[
    (Group::Process, "Processes"),
    (Group::File, "Files"),
    (Group::Network, "Network"),
    (Group::Syscall, "System calls"),
];

/// Linux reports what was done, not what was refused.
#[cfg(target_os = "linux")]
const FAILURES_TIP: &str = "Steps that did not succeed. Linux reports file steps only when they \
                            succeed: what fails here is system calls that returned an error \
                            (when they are recorded) and programs ending with an error";

/// The option that records system calls too, and what it costs.
#[cfg(target_os = "linux")]
const SYSCALLS_TIP: &str = "Every call the program makes into the kernel, as strace shows it: \
                            the call, its arguments, its result and the time it took, for the \
                            program or process recorded and what it starts, or for every \
                            process. The program is not stopped and its memory is not \
                            read: files are named by the descriptors they are open on, and by \
                            the names the kernel itself copies. Busy programs make hundreds of \
                            thousands a second: a call a thread repeats within a second is one \
                            row with a count, and the summary counts them all. Needs full \
                            access.";

/// What recording sees, for the card that starts it.
#[cfg(windows)]
const RECORD_INTRO: &str = "Start it here and every step it takes is listed from the first: the \
                            files it opens, reads, writes, renames and deletes, the registry keys \
                            and values it reads and changes, the connections it makes and the \
                            programs it starts. It runs as you, not as administrator.";
#[cfg(target_os = "linux")]
const RECORD_INTRO: &str = "Start it here and every step it takes is listed from the first: the \
                            files it opens, reads, writes, renames and deletes, the connections \
                            it makes and the programs it starts. It runs as you, without \
                            Anabolic's rights.";

#[cfg(windows)]
const PROGRAM_HINT: &str = r"C:\Path\To\setup.exe";
#[cfg(target_os = "linux")]
const PROGRAM_HINT: &str = "/path/to/program";

/// The list's columns, by number.
const TITLES: [&str; 6] = ["Time", "Process", "Operation", "Path", "Result", "Detail"];
const COLUMNS: [Spec; 6] = [
    Spec::new(96.0),
    Spec::new(150.0).clip(),
    Spec::new(110.0).clip(),
    Spec::new(520.0).clip(),
    Spec::new(130.0).clip(),
    Spec::new(300.0).clip(),
];

/// Steps kept; older ones roll off. Recording the whole computer makes
/// 3,000 to 13,000 a second here, about 135 bytes each.
const KEEP: usize = 1_000_000;
/// Filtering goes on for at most this long a frame; the rest the next.
const FILTER_BUDGET: Duration = Duration::from_millis(8);
/// More rows than this are not sorted: it would hold the window up.
const SORT_LIMIT: usize = 200_000;

fn group_color(g: Group) -> Color32 {
    match g {
        Group::Process => theme::CPU,
        Group::File => theme::DISK_WRITE,
        Group::Registry => theme::MEMORY,
        Group::Network => theme::NET_RX,
        // The registry's color: Linux has none.
        #[cfg(target_os = "linux")]
        Group::Syscall => theme::MEMORY,
    }
}

fn group_index(g: Group) -> usize {
    match g {
        Group::Process => 0,
        Group::File => 1,
        Group::Registry => 2,
        Group::Network => 3,
        #[cfg(target_os = "linux")]
        Group::Syscall => 4,
    }
}

/// Time inside a system call: "850 ns", "12.4 µs", "3.10 ms", "1.25 s".
fn call_time(d: Duration) -> String {
    let ns = d.as_nanos();
    match ns {
        0..1_000 => format!("{ns} ns"),
        1_000..1_000_000 => format!("{:.1} µs", ns as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.2} ms", ns as f64 / 1e6),
        _ => format!("{:.2} s", d.as_secs_f64()),
    }
}

/// A step's detail as listed and saved: with the time a system call took.
fn detail_text(e: &ActivityEvent) -> Cow<'_, str> {
    if e.took.is_zero() {
        Cow::Borrowed(&e.detail)
    } else {
        Cow::Owned(format!("{}  ·  {}", e.detail, call_time(e.took)))
    }
}

/// Process names and their steps, the most first.
type Busiest = Vec<(Arc<str>, u64)>;

/// Which processes a filter means.
#[derive(Clone, Debug)]
enum Pick {
    /// Every process of this name.
    Name(Arc<str>),
    /// One process.
    Pid(u32),
    /// A process and the ones it started, and they started.
    Tree(u32),
}

impl Pick {
    fn same(&self, other: &Pick) -> bool {
        match (self, other) {
            (Pick::Name(a), Pick::Name(b)) => a.eq_ignore_ascii_case(b),
            (Pick::Pid(a), Pick::Pid(b)) | (Pick::Tree(a), Pick::Tree(b)) => a == b,
            _ => false,
        }
    }
}

/// A process filter asked for from a step's menu or the busiest list.
#[derive(Clone)]
enum Narrow {
    Only(Pick),
    Hide(Pick),
}

/// What the Processes page asks to see live.
pub enum Watch {
    /// One process, and what it starts from now on.
    Process(ProcKey),
    /// A process and the processes under it.
    Tree(ProcKey),
    /// Every process of this name (a group of the Apps view).
    Name(Arc<str>),
}

/// A running process and the running ones under it, the process first.
/// Parents come from `tree::parents`, which minds reused PIDs.
pub(crate) fn tree_pids(snap: &Snapshot, root: u32) -> Vec<u32> {
    let procs = &snap.processes;
    let Some(top) = procs
        .iter()
        .position(|p| p.pid == root && p.state != ProcState::Exited)
    else {
        return vec![root];
    };
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); procs.len()];
    for (i, parent) in crate::tree::parents(procs).into_iter().enumerate() {
        if let Some(parent) = parent {
            children[parent as usize].push(i);
        }
    }
    let mut out = Vec::new();
    let mut next = vec![top];
    while let Some(i) = next.pop() {
        let p = &procs[i];
        if p.state != ProcState::Exited && !p.is_pseudo() {
            out.push(p.pid);
        }
        next.extend(children[i].iter().rev());
        if out.len() >= 10_000 {
            break;
        }
    }
    out
}

/// A new `visible` being made after the filters changed: the rows so far
/// and the next step to look at. The old one is shown until it is done.
struct Rebuild {
    rows: Vec<u64>,
    next: u64,
}

pub struct ActivityPage {
    recorder: Option<ActivityRecorder>,
    /// The steps in order; `events[0]` is step number `first` of the
    /// recording (the ones before rolled off).
    events: VecDeque<ActivityEvent>,
    first: u64,
    /// How many steps are kept (`KEEP`).
    keep: usize,
    /// The whole computer, not one program.
    everything: bool,
    /// What is being recorded, in words.
    what: String,
    /// The start cards over a finished recording: "New recording" shows
    /// them, and the recording stays until another starts.
    choosing: bool,
    started: Option<SystemTime>,
    state: Option<RecorderState>,
    summary: ActivitySummary,
    error: Option<String>,
    /// Process names by PID, from the snapshots and the starts recorded.
    names: HashMap<u32, Arc<str>>,
    pub program: String,
    pub args: String,
    picking: Option<Receiver<Option<String>>>,
    exporting: Option<Receiver<Result<std::path::PathBuf, String>>>,
    /// Record system calls too (a program or processes).
    #[cfg(target_os = "linux")]
    pub syscalls: bool,
    view: TableView,
    groups: [bool; 5],
    only_changes: bool,
    only_failures: bool,
    /// Steps the filters hide are let go as they come, not kept.
    keep_shown: bool,
    /// How many were.
    let_go: u64,
    /// Lower-case words a step must contain, and words (typed with a
    /// leading -) it must not.
    words: Vec<String>,
    unwords: Vec<String>,
    /// Processes to show (all when none) and to hide, from menus...
    only: Vec<Pick>,
    hidden: Vec<Pick>,
    /// ...and typed into the filter box ("pid:123", "-tree:456").
    typed_only: Vec<Pick>,
    typed_hidden: Vec<Pick>,
    /// Parent PIDs, from the process list and the starts recorded: for
    /// process trees.
    parents: HashMap<u32, u32>,
    /// The running process picked to record, the text that finds it, and
    /// whether the processes it has started come too.
    pick: Option<u32>,
    pick_find: String,
    pick_tree: bool,
    /// Step numbers that pass the filters, in table order.
    visible: Vec<u64>,
    /// Steps before this one have been looked at for `visible`.
    filtered: u64,
    rebuild: Option<Rebuild>,
    dirty: bool,
    /// Sorting was turned off: too many rows to sort.
    sort_refused: bool,
    /// The processes with the most steps, counted when the list opens.
    busiest: Option<(Instant, Busiest)>,
    selected: Option<u64>,
}

impl Default for ActivityPage {
    fn default() -> Self {
        Self {
            recorder: None,
            events: VecDeque::new(),
            first: 0,
            keep: KEEP,
            choosing: false,
            everything: false,
            what: String::new(),
            started: None,
            state: None,
            summary: ActivitySummary::default(),
            error: None,
            names: HashMap::new(),
            program: String::new(),
            args: String::new(),
            picking: None,
            exporting: None,
            #[cfg(target_os = "linux")]
            syscalls: false,
            view: TableView::default(),
            groups: [true; 5],
            only_changes: false,
            only_failures: false,
            keep_shown: false,
            let_go: 0,
            words: Vec::new(),
            unwords: Vec::new(),
            only: Vec::new(),
            hidden: Vec::new(),
            typed_only: Vec::new(),
            typed_hidden: Vec::new(),
            parents: HashMap::new(),
            pick: None,
            pick_find: String::new(),
            pick_tree: true,
            visible: Vec::new(),
            filtered: 0,
            rebuild: None,
            dirty: false,
            sort_refused: false,
            busiest: None,
            selected: None,
        }
    }
}

/// The PID a start event names (its detail reads "PID 1234 · command line").
pub(crate) fn started_pid(e: &ActivityEvent) -> Option<u32> {
    e.detail
        .strip_prefix("PID ")?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Whether `hay` contains `needle` (in lower case), ignoring case; no
/// allocation unless the needle is beyond ASCII. Filters run this on
/// millions of steps.
fn has(hay: &str, needle: &str) -> bool {
    if !needle.is_ascii() {
        return hay.to_lowercase().contains(needle);
    }
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    let Some((&head, rest)) = n.split_first() else {
        return true;
    };
    if h.len() < n.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|i| {
        h[i].to_ascii_lowercase() == head
            && h[i + 1..i + n.len()]
                .iter()
                .zip(rest)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Text order ignoring case, without allocating.
fn ci_cmp(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

fn result_of(e: &ActivityEvent) -> Cow<'static, str> {
    match e.result_word() {
        Some(word) => word.into(),
        None if e.status.is_some() => e.result_text().into(),
        None => "".into(),
    }
}

impl ActivityPage {
    pub fn recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// The start cards are shown, not a recording.
    pub fn starting(&self) -> bool {
        !self.recording() && (self.events.is_empty() || self.choosing)
    }

    /// The start cards over a finished recording, or that recording again
    /// (back and forward); nothing while one runs.
    pub fn show_start(&mut self, start: bool) {
        if !self.recording() && !self.events.is_empty() {
            self.choosing = start;
        }
    }

    /// One past the last step's number.
    fn end(&self) -> u64 {
        self.first + self.events.len() as u64
    }

    fn at(&self, i: u64) -> &ActivityEvent {
        &self.events[(i - self.first) as usize]
    }

    fn clear(&mut self) {
        self.events.clear();
        self.first = 0;
        self.visible.clear();
        self.filtered = 0;
        self.rebuild = None;
        self.busiest = None;
        self.selected = None;
        self.let_go = 0;
        self.what.clear();
    }

    fn reset(&mut self, what: String, everything: bool) {
        self.clear();
        self.choosing = false;
        self.error = None;
        self.summary = ActivitySummary::default();
        self.what = what;
        self.everything = everything;
        self.started = Some(SystemTime::now());
        self.only.clear();
        self.hidden.clear();
        self.dirty = true;
    }

    /// Records these running processes and what they start from now on.
    pub fn record_pids(&mut self, pids: &[u32], what: String) {
        self.stop();
        self.reset(what, false);
        #[cfg(target_os = "linux")]
        let recorder = if self.syscalls {
            ActivityRecorder::attach_with_syscalls(pids)
        } else {
            ActivityRecorder::attach_all(pids)
        };
        #[cfg(windows)]
        let recorder = ActivityRecorder::attach_all(pids);
        match recorder {
            Ok(r) => self.recorder = Some(r),
            Err(e) => self.error = Some(e),
        }
    }

    /// A running process (`--record PID`, the picker): with `tree`, the
    /// processes under it too.
    pub fn record_process(&mut self, pid: u32, tree: bool, snap: &Snapshot) {
        self.learn(snap);
        let name = self.name(pid);
        let pids = if tree {
            tree_pids(snap, pid)
        } else {
            vec![pid]
        };
        let what = match pids.len() {
            0 | 1 => format!("{name} (PID {pid}) and what it starts"),
            2 => format!("{name} (PID {pid}), the process under it, and what they start"),
            n => format!(
                "{name} (PID {pid}), the {} processes under it, and what they start",
                n - 1
            ),
        };
        self.record_pids(&pids, what);
    }

    /// The Processes page's "Live activity": while everything is being
    /// recorded, the view narrows to the selection (and shows what it did
    /// before too); otherwise the selection is recorded from now on.
    pub fn watch(&mut self, watch: Watch, snap: &Snapshot) {
        self.learn(snap);
        let pick = match &watch {
            Watch::Process(key) => Pick::Pid(key.pid),
            Watch::Tree(key) => Pick::Tree(key.pid),
            Watch::Name(name) => Pick::Name(name.clone()),
        };
        if self.everything && self.recording() {
            self.only.clear();
            self.hidden.retain(|p| !p.same(&pick));
            self.only.push(pick);
            self.selected = None;
            self.dirty = true;
            return;
        }
        match watch {
            Watch::Process(key) => self.record_process(key.pid, false, snap),
            Watch::Tree(key) => self.record_process(key.pid, true, snap),
            Watch::Name(name) => {
                let pids: Vec<u32> = snap
                    .processes
                    .iter()
                    .filter(|p| {
                        p.state != ProcState::Exited
                            && !p.is_pseudo()
                            && p.name.eq_ignore_ascii_case(&name)
                    })
                    .map(|p| p.pid)
                    .collect();
                let what = format!("every {name} ({} running) and what they start", pids.len());
                self.record_pids(&pids, what);
            }
        }
    }

    /// Names and parents of the running processes.
    fn learn(&mut self, snap: &Snapshot) {
        let procs = &snap.processes;
        for (p, parent) in procs.iter().zip(crate::tree::parents(procs)) {
            if p.state == ProcState::Exited {
                continue;
            }
            self.names.entry(p.pid).or_insert_with(|| p.name.clone());
            if let Some(parent) = parent {
                self.parents.insert(p.pid, procs[parent as usize].pid);
            }
        }
    }

    /// Every process on the computer, but Anabolic; narrowed down after.
    pub fn record_everything(&mut self) {
        self.stop();
        self.reset("everything on this computer".into(), true);
        #[cfg(target_os = "linux")]
        let recorder = if self.syscalls {
            ActivityRecorder::everything_with_syscalls()
        } else {
            ActivityRecorder::everything()
        };
        #[cfg(windows)]
        let recorder = ActivityRecorder::everything();
        match recorder {
            Ok(r) => self.recorder = Some(r),
            Err(e) => self.error = Some(e),
        }
    }

    fn launch(&mut self) {
        let program = self.program.trim().trim_matches('"').to_string();
        if program.is_empty() {
            self.error = Some("Name the program to start, or pick it with Browse.".into());
            return;
        }
        self.stop();
        self.reset(
            format!("{} from its first step", file_name(&program)),
            false,
        );
        let dir = std::path::Path::new(&program)
            .parent()
            .map(|d| d.to_string_lossy().into_owned());
        let (args, dir) = (self.args.trim(), dir.as_deref());
        #[cfg(target_os = "linux")]
        let recorder = if self.syscalls {
            ActivityRecorder::launch_with_syscalls(&program, args, dir)
        } else {
            ActivityRecorder::launch(&program, args, dir)
        };
        #[cfg(windows)]
        let recorder = ActivityRecorder::launch(&program, args, dir);
        match recorder {
            Ok(r) => self.recorder = Some(r),
            Err(e) => self.error = Some(e),
        }
    }

    pub fn stop(&mut self) {
        if let Some(r) = self.recorder.take() {
            self.summary = r.summary();
            let last = r.stop();
            self.add(last);
        }
    }

    fn add(&mut self, mut new: Vec<ActivityEvent>) {
        for e in &new {
            if e.kind == ActivityKind::ProcessStart
                && let Some(pid) = started_pid(e)
            {
                self.names.insert(pid, file_name(&e.path).into());
                self.parents.insert(pid, e.pid);
            }
        }
        if self.keep_shown {
            let before = new.len();
            new.retain(|e| self.passes(e));
            self.let_go += (before - new.len()) as u64;
        }
        self.events.extend(new);
        // Room for the newest: the oldest go, a block at a time.
        if self.events.len() > self.keep + self.keep / 8 {
            let n = self.events.len() - self.keep;
            self.events.drain(..n);
            self.first += n as u64;
            let first = self.first;
            self.visible.retain(|&i| i >= first);
            if let Some(r) = &mut self.rebuild {
                r.rows.retain(|&i| i >= first);
                r.next = r.next.max(first);
            }
            self.filtered = self.filtered.max(first);
        }
    }

    /// Once per refresh, window or not: what the recorder has gathered.
    pub fn poll(&mut self, snap: &Snapshot) {
        if let Some(r) = &mut self.recorder {
            let new = r.take();
            self.summary = r.summary();
            let state = r.state();
            if let RecorderState::Failed(why) = &state {
                self.error = Some(why.clone());
            }
            self.state = Some(state);
            if !new.is_empty() {
                self.learn(snap);
                self.add(new);
            }
        }
        if let Some(rx) = &self.picking
            && let Ok(picked) = rx.try_recv()
        {
            if let Some(path) = picked {
                self.program = path;
            }
            self.picking = None;
        }
    }

    fn name(&self, pid: u32) -> Arc<str> {
        self.names
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| format!("PID {pid}").into())
    }

    /// Types `text` into the filter box (tests).
    pub fn set_filter(&mut self, text: &str) {
        self.view.set_filter(text);
        self.parse_filter();
        self.dirty = true;
    }

    /// The words of the filter box: "-word" hides what contains it.
    /// "pid:123" and "tree:123" pick processes, the same with "-" hides
    /// them.
    fn parse_filter(&mut self) {
        self.words.clear();
        self.unwords.clear();
        self.typed_only.clear();
        self.typed_hidden.clear();
        for word in self.view.filter.split_whitespace() {
            let word = word.to_lowercase();
            let (not, word) = match word.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, word.as_str()),
            };
            if word.is_empty() {
                continue;
            }
            let pick = if let Some(pid) = word.strip_prefix("pid:") {
                pid.parse().ok().map(Pick::Pid)
            } else if let Some(pid) = word.strip_prefix("tree:") {
                pid.parse().ok().map(Pick::Tree)
            } else {
                None
            };
            match (pick, not) {
                (Some(pick), false) => self.typed_only.push(pick),
                (Some(pick), true) => self.typed_hidden.push(pick),
                (None, false) => self.words.push(word.to_string()),
                (None, true) => self.unwords.push(word.to_string()),
            }
        }
    }

    fn narrow(&mut self, how: Narrow) {
        let (pick, only) = match how {
            Narrow::Only(pick) => (pick, true),
            Narrow::Hide(pick) => (pick, false),
        };
        self.only.retain(|p| !p.same(&pick));
        self.hidden.retain(|p| !p.same(&pick));
        if only {
            self.only.push(pick);
        } else {
            self.hidden.push(pick);
        }
        self.dirty = true;
    }

    /// Whether `pid` is `root` or was started by it, directly or not.
    fn descends(&self, mut pid: u32, root: u32) -> bool {
        for _ in 0..64 {
            if pid == root {
                return true;
            }
            match self.parents.get(&pid) {
                Some(&parent) if parent != pid => pid = parent,
                _ => return false,
            }
        }
        false
    }

    fn picks(&self, picks: &[Pick], pid: u32, name: &str) -> bool {
        picks.iter().any(|p| match p {
            Pick::Name(n) => n.eq_ignore_ascii_case(name),
            Pick::Pid(p) => *p == pid,
            Pick::Tree(root) => self.descends(pid, *root),
        })
    }

    fn pick_label(&self, pick: &Pick) -> String {
        match pick {
            Pick::Name(name) => format!("every {name}"),
            Pick::Pid(pid) => format!("{} (PID {pid})", self.name(*pid)),
            Pick::Tree(pid) => format!("{} (PID {pid}) and what it started", self.name(*pid)),
        }
    }

    fn passes(&self, e: &ActivityEvent) -> bool {
        if !self.groups[group_index(e.kind.group())]
            || self.only_changes && !e.kind.changes()
            || self.only_failures && !e.failed()
        {
            return false;
        }
        let name = self.names.get(&e.pid).map_or("", |n| &**n);
        if !self.only.is_empty() && !self.picks(&self.only, e.pid, name)
            || !self.typed_only.is_empty() && !self.picks(&self.typed_only, e.pid, name)
            || self.picks(&self.hidden, e.pid, name)
            || self.picks(&self.typed_hidden, e.pid, name)
        {
            return false;
        }
        if self.words.is_empty() && self.unwords.is_empty() {
            return true;
        }
        let result = result_of(e);
        let texts = [name, e.kind.label(), &e.path, &e.detail, &result];
        self.words.iter().all(|w| texts.iter().any(|t| has(t, w)))
            && !self.unwords.iter().any(|w| texts.iter().any(|t| has(t, w)))
    }

    /// The steps `next..end` that pass, until `deadline`; whether it got to
    /// the end.
    fn scan(&self, rows: &mut Vec<u64>, next: &mut u64, end: u64, deadline: Instant) -> bool {
        while *next < end {
            let stop = (*next + 4096).min(end);
            rows.extend((*next..stop).filter(|&i| self.passes(self.at(i))));
            *next = stop;
            if *next < end && Instant::now() >= deadline {
                return false;
            }
        }
        true
    }

    /// Brings `visible` up to date, a frame's worth at a time: new steps
    /// are added to it; a change of filter makes a new one. Returns whether
    /// there is more to do.
    fn refilter(&mut self) -> bool {
        let deadline = Instant::now() + FILTER_BUDGET;
        if self.dirty {
            self.dirty = false;
            self.rebuild = Some(Rebuild {
                rows: Vec::new(),
                next: self.first,
            });
        }
        let end = self.end();
        if let Some(mut r) = self.rebuild.take() {
            if !self.scan(&mut r.rows, &mut r.next, end, deadline) {
                self.rebuild = Some(r);
                return true;
            }
            self.visible = r.rows;
            self.filtered = end;
            self.order();
            return false;
        }
        if self.filtered < end {
            let (mut fresh, mut next) = (Vec::new(), self.filtered);
            let done = self.scan(&mut fresh, &mut next, end, deadline);
            self.filtered = next;
            self.merge(fresh);
            return !done;
        }
        false
    }

    /// Two rows in the order of the chosen column; equal ones in time
    /// order.
    fn compare(&self, a: u64, b: u64) -> Ordering {
        let Some((column, descending)) = self.view.sort else {
            return a.cmp(&b);
        };
        let (x, y) = (self.at(a), self.at(b));
        let name = |pid: u32| self.names.get(&pid).map_or("", |n| &**n);
        let o = match column {
            0 => Ordering::Equal,
            1 => ci_cmp(name(x.pid), name(y.pid)),
            2 => x.kind.label().cmp(y.kind.label()),
            3 => ci_cmp(&x.path, &y.path),
            4 => result_of(x).cmp(&result_of(y)),
            _ => ci_cmp(&x.detail, &y.detail),
        }
        .then(a.cmp(&b));
        if descending { o.reverse() } else { o }
    }

    /// Too many rows to sort: back to time order.
    fn refuse_sort(&mut self) {
        self.view.sort = None;
        self.sort_refused = true;
        self.visible.sort_unstable();
    }

    /// Puts `visible` in table order (it is in time order, or was sorted
    /// by another column).
    fn order(&mut self) {
        if self.view.sort.is_none() {
            self.visible.sort_unstable();
            return;
        }
        if self.visible.len() > SORT_LIMIT {
            self.refuse_sort();
            return;
        }
        let mut rows = std::mem::take(&mut self.visible);
        rows.sort_unstable_by(|&a, &b| self.compare(a, b));
        self.visible = rows;
    }

    /// New rows into `visible`, where the order puts them.
    fn merge(&mut self, mut fresh: Vec<u64>) {
        if fresh.is_empty() {
            return;
        }
        if self.view.sort.is_none() {
            self.visible.extend(fresh);
            return;
        }
        if self.visible.len() + fresh.len() > SORT_LIMIT {
            self.refuse_sort();
            self.visible.extend(fresh);
            return;
        }
        fresh.sort_unstable_by(|&a, &b| self.compare(a, b));
        let old = std::mem::take(&mut self.visible);
        let mut rows = Vec::with_capacity(old.len() + fresh.len());
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < fresh.len() {
            if self.compare(fresh[j], old[i]) == Ordering::Less {
                rows.push(fresh[j]);
                j += 1;
            } else {
                rows.push(old[i]);
                i += 1;
            }
        }
        rows.extend_from_slice(&old[i..]);
        rows.extend_from_slice(&fresh[j..]);
        self.visible = rows;
    }

    /// The processes with the most steps (all of the recording, filters or
    /// not), counted at most every two seconds while the list is open.
    fn busiest(&mut self) -> &[(Arc<str>, u64)] {
        let stale = self
            .busiest
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(2));
        if stale {
            let mut by_pid: HashMap<u32, u64> = HashMap::new();
            for e in &self.events {
                *by_pid.entry(e.pid).or_default() += 1;
            }
            let mut by_name: HashMap<Arc<str>, u64> = HashMap::new();
            for (pid, n) in by_pid {
                *by_name.entry(self.name(pid)).or_default() += n;
            }
            let mut list: Vec<_> = by_name.into_iter().collect();
            list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| ci_cmp(&a.0, &b.0)));
            list.truncate(20);
            self.busiest = Some((Instant::now(), list));
        }
        self.busiest.as_ref().map_or(&[], |(_, list)| list)
    }
}

/// A file picker for the program to record, on a thread of its own (the
/// dialog runs its own message loop).
#[cfg(windows)]
fn pick_program(owner: Option<isize>) -> Receiver<Option<String>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("anabolic-pick".into())
        .spawn(move || {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree,
            };
            use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
            use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
            use windows::core::w;
            let picked = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                (|| -> windows::core::Result<String> {
                    let dialog: IFileOpenDialog =
                        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                    dialog.SetTitle(w!("Record a program"))?;
                    dialog.SetFileTypes(&[
                        COMDLG_FILTERSPEC {
                            pszName: w!("Programs"),
                            pszSpec: w!("*.exe;*.bat;*.cmd;*.msi;*.ps1"),
                        },
                        COMDLG_FILTERSPEC {
                            pszName: w!("All files"),
                            pszSpec: w!("*.*"),
                        },
                    ])?;
                    dialog.Show(owner.map(|h| HWND(h as *mut _)))?;
                    let item = dialog.GetResult()?;
                    let path = item.GetDisplayName(SIGDN_FILESYSPATH)?;
                    let text = path.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(path.0.cast()));
                    Ok(text)
                })()
            };
            let _ = tx.send(picked.ok());
        });
    rx
}

/// A file picker for the program to record: the desktop's own (KDE's
/// kdialog, else GNOME's zenity), on a thread of its own.
#[cfg(target_os = "linux")]
fn pick_program(_owner: Option<isize>) -> Receiver<Option<String>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("anabolic-pick".into())
        .spawn(move || {
            use std::process::Command;
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            let ask = |program: &str, args: &[&str]| {
                let out = Command::new(program).args(args).output().ok()?;
                let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
                Some((out.status.success() && !path.is_empty()).then_some(path))
            };
            let picked = ask(
                "kdialog",
                &["--title", "Record a program", "--getopenfilename", &home],
            )
            .or_else(|| ask("zenity", &["--file-selection", "--title=Record a program"]))
            .flatten();
            let _ = tx.send(picked);
        });
    rx
}

fn export(
    events: Vec<ActivityEvent>,
    names: HashMap<u32, Arc<str>>,
) -> Receiver<Result<std::path::PathBuf, String>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("anabolic-export".into())
        .spawn(move || {
            let run = || -> Result<std::path::PathBuf, String> {
                use std::io::Write;
                let dir = crate::data_dir()
                    .ok_or("no folder for Anabolic's files")?
                    .join("recordings");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let stamp: String = fmt::clock(SystemTime::now(), false)
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                let path = dir.join(format!("activity-{stamp}.csv"));
                let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
                let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                let mut out = std::io::BufWriter::new(file);
                let mut write = || -> std::io::Result<()> {
                    out.write_all(
                        b"Time,PID,Process,Operation,Path,Result,Detail,Count,Bytes\r\n",
                    )?;
                    for e in &events {
                        let name = names.get(&e.pid).map(|n| n.to_string()).unwrap_or_default();
                        write!(
                            out,
                            "{},{},{},{},{},{},{},{},{}\r\n",
                            fmt::clock(e.at, true),
                            e.pid,
                            quote(&name),
                            quote(e.kind.label()),
                            quote(&e.path),
                            quote(&e.result_text()),
                            quote(&detail_text(e)),
                            e.count,
                            e.bytes
                        )?;
                    }
                    out.flush()
                };
                write().map_err(|e| e.to_string())?;
                Ok(path)
            };
            let _ = tx.send(run());
        });
    rx
}

fn summary_line(page: &ActivityPage) -> String {
    let s = &page.summary;
    let mut parts = vec![format!("{} steps", fmt::count(s.kept))];
    if page.first > 0 {
        parts.push(format!(
            "the last {} kept, {} older ones let go",
            fmt::count(page.events.len() as u64),
            fmt::count(page.first)
        ));
    }
    if page.let_go > 0 {
        parts.push(format!(
            "{} hidden by the filters let go",
            fmt::count(page.let_go)
        ));
    }
    let mut add = |n: u32, what: &str| {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    };
    add(s.processes_started, "programs started");
    add(s.files_created, "files created");
    add(s.files_written, "files written");
    add(s.files_deleted, "files deleted");
    add(s.keys_created, "keys created");
    add(s.values_set, "values set");
    add(s.keys_deleted, "keys deleted");
    add(s.endpoints, "addresses contacted");
    if s.sent + s.received > 0 {
        parts.push(format!(
            "{} sent, {} received",
            fmt::bytes(s.sent),
            fmt::bytes(s.received)
        ));
    }
    let (calls, failed) = s
        .syscalls
        .iter()
        .fold((0, 0), |(c, f), stat| (c + stat.calls, f + stat.errors));
    if calls > 0 {
        parts.push(format!(
            "{} system calls, {} failed",
            fmt::count(calls),
            fmt::count(failed)
        ));
    }
    if s.dropped + s.lost as u64 > 0 {
        parts.push(format!(
            "{} not kept",
            fmt::count(s.dropped + s.lost as u64)
        ));
    }
    parts.join("  ·  ")
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn start_card(ui: &mut Ui, page: &mut ActivityPage, snap: &Snapshot) {
    #[cfg(target_os = "linux")]
    {
        ui.horizontal(|ui| {
            ui.checkbox(&mut page.syscalls, "System calls too")
                .on_hover_text(SYSCALLS_TIP);
            ui.label(
                RichText::new(
                    "Every call into the kernel, as strace shows it: for a program, a running \
                     process, or everything (heavy).",
                )
                .size(theme::META)
                .color(theme::TEXT_WEAK),
            )
            .on_hover_text(SYSCALLS_TIP);
        });
        ui.add_space(6.0);
    }
    card(ui, |ui| {
        ui.label(widgets::strong("Record a program", theme::SUBTITLE));
        ui.label(
            RichText::new(RECORD_INTRO)
                .size(theme::META)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Program");
            ui.add(
                TextEdit::singleline(&mut page.program)
                    .hint_text(PROGRAM_HINT)
                    .desired_width(420.0),
            );
            if ui
                .add_enabled(page.picking.is_none(), egui::Button::new("Browse…"))
                .clicked()
            {
                #[cfg(windows)]
                let owner = anabolic_core::instance::own_window().map(|h| h.0 as isize);
                #[cfg(not(windows))]
                let owner = None;
                page.picking = Some(pick_program(owner));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Arguments");
            ui.add(TextEdit::singleline(&mut page.args).desired_width(420.0));
            if ui
                .button(widgets::strong("Start and record", theme::BODY))
                .clicked()
            {
                page.launch();
            }
        });
    });
    ui.add_space(8.0);
    card(ui, |ui| running_card(ui, page, snap));
    ui.add_space(8.0);
    card(ui, |ui| {
        ui.label(widgets::strong("Record everything", theme::SUBTITLE));
        ui.label(
            RichText::new(format!(
                "Every process on this computer from now on, the way Process Monitor does \
                 it; then narrow it down with the filters: a process, a path, an operation, \
                 failures. Catches what a program has others do for it (an installer's \
                 service, a COM server). A step a process repeats within a second is one \
                 row with a count. Keeps the last {} steps (about 150 MB): a minute or \
                 two on a busy computer, more on a quiet one. To record one thing for \
                 longer, filter it and tick \"Keep only what's shown\".",
                fmt::count(KEEP as u64)
            ))
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        );
        #[cfg(target_os = "linux")]
        if page.syscalls {
            ui.label(
                RichText::new(
                    "With system calls: every process' calls, often hundreds of thousands a \
                     second. Anabolic takes a good share of a processor to read them while it \
                     records, and the steps kept fill in a minute or less. Calls a thread was \
                     already waiting in when the recording started, or still is when it stops, \
                     are counted, not listed.",
                )
                .size(theme::META)
                .color(theme::ELEVATED_TEXT),
            );
        }
        ui.add_space(6.0);
        if ui
            .button(widgets::strong("Record everything", theme::BODY))
            .clicked()
        {
            page.record_everything();
        }
    });
}

/// Picks a running process (and the ones under it) to record.
fn running_card(ui: &mut Ui, page: &mut ActivityPage, snap: &Snapshot) {
    ui.label(widgets::strong("Record a running process", theme::SUBTITLE));
    ui.label(
        RichText::new(
            "It, and what it starts from now on. On the Processes page, \"Live activity\" \
             above a selected process does the same (with the processes under it in the tree \
             view, every process of the name in the Apps view).",
        )
        .size(theme::META)
        .color(theme::TEXT_WEAK),
    );
    ui.add_space(6.0);
    let alive = |pid: u32| {
        snap.processes
            .iter()
            .any(|p| p.pid == pid && p.state != ProcState::Exited)
    };
    let mut record = false;
    ui.horizontal(|ui| {
        ui.add(
            TextEdit::singleline(&mut page.pick_find)
                .hint_text("Find by name or PID")
                .desired_width(240.0),
        );
        ui.checkbox(&mut page.pick_tree, "With the processes under it");
        let ready = page.pick.is_some_and(alive);
        if ui
            .add_enabled(
                ready,
                egui::Button::new(widgets::strong("Record", theme::BODY)),
            )
            .on_disabled_hover_text("Pick a process from the list")
            .clicked()
        {
            record = true;
        }
    });
    let find = page.pick_find.trim().to_lowercase();
    let mut rows: Vec<_> = snap
        .processes
        .iter()
        .filter(|p| {
            p.state != ProcState::Exited
                && !p.is_pseudo()
                && p.pid != std::process::id()
                && (find.is_empty() || has(&p.name, &find) || p.pid.to_string().starts_with(&find))
        })
        .collect();
    rows.sort_by(|a, b| ci_cmp(&a.name, &b.name).then(a.pid.cmp(&b.pid)));
    egui::ScrollArea::vertical()
        .id_salt("activity-pick")
        .max_height(170.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for p in rows.iter().take(400) {
                let text = RichText::new(format!("{}   ·   PID {}", p.name, p.pid));
                let r = ui.add(egui::Button::selectable(page.pick == Some(p.pid), text));
                if r.clicked() {
                    page.pick = Some(p.pid);
                }
                if r.double_clicked() {
                    record = true;
                }
            }
        });
    if record && let Some(pid) = page.pick.filter(|&pid| alive(pid)) {
        page.record_process(pid, page.pick_tree, snap);
    }
}

/// The system calls recorded, by call, as `strace -c` lists them: the most
/// time first. Returns a call to filter the list by, when one is clicked.
fn syscall_summary(ui: &mut Ui, stats: &[SyscallStat]) -> Option<&'static str> {
    let mut picked = None;
    ui.menu_button("System calls", |ui| {
        ui.set_min_width(420.0);
        let total: Duration = stats.iter().map(|s| s.time).sum();
        ui.label(
            RichText::new(
                "Every call recorded, merged rows and rows let go too; time is spent inside \
                 the call. Click a call to list only it.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        );
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .show(ui, |ui| {
                egui::Grid::new("activity-syscalls")
                    .num_columns(6)
                    .spacing([14.0, 2.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for title in ["Call", "Calls", "Errors", "Time", "% time", "Average"] {
                            ui.label(
                                RichText::new(title)
                                    .size(theme::META)
                                    .color(theme::TEXT_WEAK),
                            );
                        }
                        ui.end_row();
                        for s in stats {
                            if ui
                                .add(egui::Button::new(RichText::new(s.name).monospace()).small())
                                .clicked()
                            {
                                picked = Some(s.name);
                                ui.close();
                            }
                            ui.label(fmt::count(s.calls));
                            let errors = RichText::new(fmt::count(s.errors));
                            ui.label(if s.errors > 0 {
                                errors.color(theme::BAD)
                            } else {
                                errors.color(theme::TEXT_DIM)
                            });
                            ui.label(call_time(s.time));
                            let share = if total.is_zero() {
                                0.0
                            } else {
                                s.time.as_secs_f64() / total.as_secs_f64() * 100.0
                            };
                            ui.label(format!("{share:.1}"));
                            let average = s.time.as_nanos() / u128::from(s.calls.max(1));
                            ui.label(call_time(Duration::from_nanos(average as u64)));
                            ui.end_row();
                        }
                    });
            });
    })
    .response
    .on_hover_text("The calls recorded, by call: how many, how many failed, and the time in them");
    picked
}

/// Shown and hidden processes, each with a cross that drops it.
fn process_filters(ui: &mut Ui, page: &mut ActivityPage) -> bool {
    if page.only.is_empty() && page.hidden.is_empty() {
        return false;
    }
    let label =
        |list: &[Pick]| -> Vec<String> { list.iter().map(|p| page.pick_label(p)).collect() };
    let (only, hidden) = (label(&page.only), label(&page.hidden));
    let mut gone = None;
    ui.horizontal_wrapped(|ui| {
        for (is_only, labels) in [(true, &only), (false, &hidden)] {
            for (i, text) in labels.iter().enumerate() {
                let (word, color) = if is_only {
                    ("Only", theme::TEXT)
                } else {
                    ("Hiding", theme::TEXT_DIM)
                };
                ui.label(RichText::new(word).color(theme::TEXT_WEAK));
                ui.label(RichText::new(text).color(color));
                if ui
                    .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                    .on_hover_text("Drop this filter")
                    .clicked()
                {
                    gone = Some((is_only, i));
                }
                ui.add_space(8.0);
            }
        }
    });
    match gone {
        Some((true, i)) => {
            page.only.remove(i);
            true
        }
        Some((false, i)) => {
            page.hidden.remove(i);
            true
        }
        None => false,
    }
}

pub fn show(
    ui: &mut Ui,
    page: &mut ActivityPage,
    snap: &Snapshot,
    elevated: bool,
    relaunching: bool,
    out: &mut Vec<UiCmd>,
) {
    #[cfg(windows)]
    if !elevated {
        ui.horizontal(|ui| {
            ui.label(widgets::icon(theme::icon::LOCK, 13.0).color(theme::ELEVATED_TEXT));
            ui.label(
                RichText::new(
                    "Recording uses kernel tracing, which needs Anabolic running as administrator.",
                )
                .color(theme::ELEVATED_TEXT),
            );
            if ui
                .add_enabled(!relaunching, egui::Button::new("Restart as admin"))
                .clicked()
            {
                out.push(UiCmd::RelaunchElevated);
            }
        });
        return;
    }
    // Processes and connections are recorded by anyone; files need the
    // right to watch every file system, system calls the kernel's tracing.
    #[cfg(target_os = "linux")]
    if !elevated {
        ui.horizontal(|ui| {
            ui.label(widgets::icon(theme::icon::LOCK, 13.0).color(theme::ELEVATED_TEXT));
            ui.label(
                RichText::new(
                    "Without full access only programs started and connections are recorded: \
                     watching files needs cap_sys_admin, and system calls cap_perfmon.",
                )
                .color(theme::ELEVATED_TEXT),
            );
            if ui
                .add_enabled(!relaunching, egui::Button::new("Grant full access"))
                .clicked()
            {
                out.push(UiCmd::RelaunchElevated);
            }
        });
    }
    if let Some(rx) = &page.exporting
        && let Ok(done) = rx.try_recv()
    {
        page.exporting = None;
        match done {
            Ok(path) => out.push(UiCmd::OpenLocation(path.to_string_lossy().into_owned())),
            Err(e) => page.error = Some(format!("Could not save: {e}")),
        }
    }

    // What is recorded, and the controls.
    ui.horizontal(|ui| {
        if page.recording() {
            ui.label(widgets::icon(theme::icon::RECORD, 13.0).color(theme::BAD));
            let state = match &page.state {
                Some(RecorderState::Starting) | None => "Getting ready…",
                Some(RecorderState::Recording) => "Recording",
                Some(RecorderState::Failed(_)) => "Recording (with a problem)",
            };
            ui.label(widgets::strong(
                format!("{state}: {}", page.what),
                theme::BODY,
            ));
            if let Some(at) = page.started {
                let secs = SystemTime::now().duration_since(at).unwrap_or_default();
                ui.label(
                    RichText::new(format!(
                        "for {}",
                        fmt::span(Duration::from_secs(secs.as_secs()))
                    ))
                    .color(theme::TEXT_WEAK),
                );
            }
            if ui.button("Stop").clicked() {
                page.stop();
            }
        } else if page.choosing {
            ui.label(
                RichText::new(format!("Last recording: {}", page.what)).color(theme::TEXT_WEAK),
            );
            if ui
                .button("Back to it")
                .on_hover_text("It stays until another recording starts (also Alt+←)")
                .clicked()
            {
                page.choosing = false;
            }
        } else if !page.events.is_empty() {
            ui.label(widgets::strong(
                format!("Recorded: {}", page.what),
                theme::BODY,
            ));
            if ui
                .button("New recording")
                .on_hover_text(
                    "The ways to start one; this recording stays until another starts \
                     (Alt+← comes back to it)",
                )
                .clicked()
            {
                page.choosing = true;
            }
        }
        if !page.visible.is_empty()
            && !page.choosing
            && ui
                .add_enabled(page.exporting.is_none(), egui::Button::new("Save as CSV"))
                .on_hover_text(
                    "The steps shown (after the filters), into Anabolic's recordings folder, \
                     which opens when done",
                )
                .clicked()
        {
            let rows = page.visible.iter().map(|&i| page.at(i).clone()).collect();
            page.exporting = Some(export(rows, page.names.clone()));
        }
    });
    if let Some(e) = &page.error {
        ui.label(RichText::new(e).color(theme::BAD));
    }
    if page.starting() {
        start_card(ui, page, snap);
        return;
    }
    ui.label(
        RichText::new(summary_line(page))
            .size(theme::META)
            .color(theme::TEXT_WEAK),
    );
    ui.add_space(4.0);

    // Filters.
    let mut changed = false;
    let mut narrow = None;
    ui.horizontal(|ui| {
        let total = page.events.len();
        let shown = page.visible.len();
        if page
            .view
            .filter_box(ui, "Filter: words to find, -word to hide", shown, total)
        {
            page.parse_filter();
            changed = true;
        }
        ui.menu_button("Busiest", |ui| {
            ui.set_min_width(300.0);
            ui.label(
                RichText::new("Processes with the most steps")
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
            );
            for (name, n) in page.busiest().to_vec() {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [150.0, 18.0],
                        Label::new(RichText::new(&*name)).truncate(),
                    );
                    ui.label(RichText::new(fmt::count(n)).color(theme::TEXT_WEAK));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Hide").clicked() {
                            narrow = Some(Narrow::Hide(Pick::Name(name.clone())));
                        }
                        if ui.small_button("Only").clicked() {
                            narrow = Some(Narrow::Only(Pick::Name(name.clone())));
                        }
                    });
                });
            }
        })
        .response
        .on_hover_text("The processes with the most steps; show only one, or hide it");
        if !page.summary.syscalls.is_empty()
            && let Some(call) = syscall_summary(ui, &page.summary.syscalls)
        {
            page.view.set_filter(call);
            page.parse_filter();
            changed = true;
        }
        ui.add_space(6.0);
        for &(g, label) in GROUPS {
            let i = group_index(g);
            let text = RichText::new(label).color(if page.groups[i] {
                group_color(g)
            } else {
                theme::TEXT_DIM
            });
            if ui
                .add(egui::Button::selectable(page.groups[i], text))
                .clicked()
            {
                page.groups[i] = !page.groups[i];
                changed = true;
            }
        }
        ui.add_space(6.0);
        changed |= ui
            .checkbox(&mut page.only_changes, "Only changes")
            .on_hover_text("Creating, writing, renaming and deleting; setting and deleting keys and values; starting programs")
            .changed();
        let failures = ui.checkbox(&mut page.only_failures, "Only failures");
        #[cfg(target_os = "linux")]
        let failures = failures.on_hover_text(FAILURES_TIP);
        changed |= failures.changed();
        if page.recording() {
            ui.add_space(6.0);
            ui.checkbox(&mut page.keep_shown, "Keep only what's shown")
                .on_hover_text(
                    "Steps the filters hide are let go as they come instead of kept, so a                      long recording of one thing fits. What was let go does not come back                      when the filters change.",
                );
        }
    });
    changed |= process_filters(ui, page);
    if let Some(how) = narrow.take() {
        page.narrow(how);
    }
    if changed {
        page.dirty = true;
    }
    if page.rebuild.is_some() || page.sort_refused {
        ui.horizontal(|ui| {
            if let Some(r) = &page.rebuild {
                let done =
                    r.next.saturating_sub(page.first) as f32 / page.events.len().max(1) as f32;
                ui.spinner();
                ui.label(
                    RichText::new(format!("Filtering… {:.0}%", done * 100.0))
                        .size(theme::META)
                        .color(theme::TEXT_WEAK),
                );
            }
            if page.sort_refused {
                ui.label(
                    RichText::new(format!(
                        "Sorting takes {} rows or fewer: narrow the filter, then click a \
                         column title again.",
                        fmt::count(SORT_LIMIT as u64)
                    ))
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
                );
            }
        });
    }
    let sort_before = page.view.sort;
    if page.refilter() || page.dirty {
        ui.ctx().request_repaint();
    }

    // The selected step, beside the list.
    let selected = page.selected;
    let found = selected
        .and_then(|seq| page.events.binary_search_by_key(&seq, |e| e.seq).ok())
        .map(|i| page.events[i].clone());
    if let Some(e) = found {
        let name = page.name(e.pid);
        let mut narrowed = None;
        panes::side(ui, "activity-pane", |ui| {
            narrowed = step_pane(ui, &e, &name, snap, out);
        });
        if let Some(how) = narrowed {
            page.narrow(how);
        }
    }

    let mut clicked = None;
    let events = &page.events;
    let first = page.first;
    let visible = &page.visible;
    let names = &page.names;
    let view = &mut page.view;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        Columns::load(ui.ctx(), "activity", &COLUMNS).show(ui, |table, columns| {
            table
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .sense(Sense::click())
                .stick_to_bottom(view.sort.is_none())
                .cell_layout(Layout::left_to_right(Align::Center))
                .header(20.0, |mut h| {
                    columns.titles(&mut h, |ui, c| {
                        view.header(ui, c, TITLES[c], false);
                    });
                })
                .body(|body| {
                    columns.keep(body.widths());
                    body.rows(18.0, visible.len(), |mut row| {
                        let e = &events[(visible[row.index()] - first) as usize];
                        row.set_selected(selected == Some(e.seq));
                        let failed = e.failed();
                        for &c in columns.order() {
                            row.col(|ui| match c {
                                0 => {
                                    ui.label(
                                        RichText::new(fmt::clock(e.at, true))
                                            .color(theme::TEXT_WEAK),
                                    );
                                }
                                1 => {
                                    let name = names
                                        .get(&e.pid)
                                        .map(|n| n.to_string())
                                        .unwrap_or_else(|| format!("PID {}", e.pid));
                                    ui.add(Label::new(name).truncate());
                                }
                                2 => {
                                    ui.label(
                                        RichText::new(e.kind.label())
                                            .color(group_color(e.kind.group())),
                                    );
                                }
                                3 => {
                                    let mut text = RichText::new(&*e.path);
                                    if e.kind.changes() {
                                        text = text.strong();
                                    }
                                    ui.add(Label::new(text).truncate());
                                }
                                4 => {
                                    let color = if failed { theme::BAD } else { theme::TEXT_WEAK };
                                    ui.label(RichText::new(e.result_text()).color(color));
                                }
                                _ => {
                                    let mut detail = detail_text(e).into_owned();
                                    if e.count > 1 {
                                        detail = format!("{}×  {detail}", e.count);
                                    }
                                    if e.bytes > 0 {
                                        detail = format!("{}  {detail}", fmt::bytes(e.bytes));
                                    }
                                    ui.add(
                                        Label::new(RichText::new(detail).color(theme::TEXT_DIM))
                                            .truncate(),
                                    );
                                }
                            });
                        }
                        let response = row.response();
                        if response.clicked() {
                            clicked = Some(e.seq);
                        }
                        widgets::row_menu(&response, "activity", e.seq, |ui| {
                            let name = names.get(&e.pid).cloned();
                            if let Some(how) = step_menu(ui, e, name, snap, out) {
                                narrow = Some(how);
                            }
                        });
                    });
                })
        });
    });
    if let Some(how) = narrow {
        page.narrow(how);
        ui.ctx().request_repaint();
    }
    if page.view.sort != sort_before {
        page.sort_refused = false;
        page.order();
    }
    if let Some(seq) = clicked {
        page.selected = if selected == Some(seq) {
            None
        } else {
            Some(seq)
        };
    }
}

fn is_file(e: &ActivityEvent) -> bool {
    matches!(e.kind.group(), Group::File | Group::Process) || e.kind == ActivityKind::ImageLoad
}

/// The step's menu; a process filter when one was asked for.
fn step_menu(
    ui: &mut Ui,
    e: &ActivityEvent,
    name: Option<Arc<str>>,
    snap: &Snapshot,
    out: &mut Vec<UiCmd>,
) -> Option<Narrow> {
    let mut narrow = None;
    // A start names the process started: it can be picked too.
    let started = (e.kind == ActivityKind::ProcessStart)
        .then(|| started_pid(e))
        .flatten();
    for (title, only) in [("Show only", true), ("Hide", false)] {
        ui.menu_button(title, |ui| {
            let mut picks = Vec::new();
            if let Some(name) = &name {
                picks.push((format!("Every {name}"), Pick::Name(name.clone())));
            }
            picks.push((format!("PID {}", e.pid), Pick::Pid(e.pid)));
            picks.push((
                format!("PID {} and what it started", e.pid),
                Pick::Tree(e.pid),
            ));
            if let Some(child) = started {
                picks.push((
                    format!("The process started, PID {child}, and what it started"),
                    Pick::Tree(child),
                ));
            }
            for (label, pick) in picks {
                if ui.button(label).clicked() {
                    narrow = Some(if only {
                        Narrow::Only(pick)
                    } else {
                        Narrow::Hide(pick)
                    });
                    ui.close();
                }
            }
        });
    }
    ui.separator();
    if let Some(p) = snap
        .processes
        .iter()
        .find(|p| p.pid == e.pid && p.state != ProcState::Exited)
    {
        item(ui, out, "Go to process", "", UiCmd::GoTo(p.key));
        ui.separator();
    }
    if is_file(e) && e.path.contains(":\\") {
        item(
            ui,
            out,
            "Open file location",
            "",
            UiCmd::OpenLocation(e.path.to_string()),
        );
    }
    item(ui, out, "Copy path", "", UiCmd::Copy(e.path.to_string()));
    item(
        ui,
        out,
        "Search online",
        "",
        UiCmd::SearchOnline(file_name(&e.path).to_string()),
    );
    narrow
}

fn step_pane(
    ui: &mut Ui,
    e: &ActivityEvent,
    name: &Arc<str>,
    snap: &Snapshot,
    out: &mut Vec<UiCmd>,
) -> Option<Narrow> {
    panes::title(ui, e.kind.label().to_string(), Some(&e.path));
    panes::facts(ui, ("activity-step", e.seq), |ui| {
        panes::fact(ui, "When", fmt::clock(e.at, true));
        panes::fact(
            ui,
            "Process",
            format!("{name} (PID {})  ·  thread {}", e.pid, e.tid),
        );
        let result = e.result_text();
        if !result.is_empty() {
            let color = if e.failed() { theme::BAD } else { theme::TEXT };
            panes::fact(ui, "Result", RichText::new(result).color(color));
        }
        if e.count > 1 {
            panes::fact(ui, "Times", format!("{}, taken together", e.count));
        }
        if e.bytes > 0 {
            panes::fact(ui, "Bytes", fmt::bytes(e.bytes));
        }
        if !e.took.is_zero() {
            let took = if e.count > 1 {
                format!(
                    "{} in all, {} on average",
                    call_time(e.took),
                    call_time(e.took / e.count)
                )
            } else {
                call_time(e.took)
            };
            panes::fact(ui, "In the call", took);
        }
        if !e.detail.is_empty() {
            panes::fact(ui, "Detail", e.detail.as_str());
        }
    });
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| step_menu(ui, e, Some(name.clone()), snap, out))
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_words_ignoring_case() {
        assert!(has(r"C:\Windows\System32\KERNEL32.dll", "kernel32"));
        assert!(has("Open key", "open"));
        assert!(has("anything", ""));
        assert!(!has("ab", "abc"));
        assert!(!has("explorer.exe", "svchost"));
        assert!(has("ÜNLÜ.txt", "ünlü"));
    }

    fn step_of(pid: u32) -> ActivityEvent {
        ActivityEvent {
            seq: 0,
            at: SystemTime::now(),
            pid,
            tid: 1,
            kind: ActivityKind::FileRead,
            path: "C:\\x".into(),
            detail: String::new(),
            status: None,
            count: 1,
            bytes: 0,
            took: Duration::ZERO,
        }
    }

    #[test]
    fn shows_the_time_in_a_call() {
        assert_eq!(call_time(Duration::from_nanos(850)), "850 ns");
        assert_eq!(call_time(Duration::from_nanos(12_400)), "12.4 µs");
        assert_eq!(call_time(Duration::from_micros(3_100)), "3.10 ms");
        assert_eq!(call_time(Duration::from_millis(1_250)), "1.25 s");
        let mut step = step_of(1);
        step.detail = "read(5, NULL, 64) = 64".into();
        assert_eq!(detail_text(&step), "read(5, NULL, 64) = 64");
        step.took = Duration::from_micros(2);
        assert_eq!(detail_text(&step), "read(5, NULL, 64) = 64  ·  2.0 µs");
    }

    #[test]
    fn picks_a_pid_or_a_tree() {
        let mut page = ActivityPage::default();
        // 10 started 11, which started 12; 20 is someone else's.
        page.parents.insert(11, 10);
        page.parents.insert(12, 11);
        page.parents.insert(20, 1);
        let passes = |page: &ActivityPage, pid| page.passes(&step_of(pid));
        page.set_filter("tree:10");
        assert!([10, 11, 12].iter().all(|&pid| passes(&page, pid)));
        assert!(!passes(&page, 20));
        page.set_filter("tree:10 -pid:11");
        assert!(passes(&page, 12) && !passes(&page, 11));
        // One started while recording joins its parent's tree.
        page.add(vec![ActivityEvent {
            kind: ActivityKind::ProcessStart,
            path: "child.exe".into(),
            detail: "PID 13  ·  child.exe /x".into(),
            ..step_of(12)
        }]);
        assert!(passes(&page, 13));
        assert_eq!(&*page.name(13), "child.exe");

        page.set_filter("");
        page.narrow(Narrow::Only(Pick::Pid(20)));
        assert!(passes(&page, 20) && !passes(&page, 10));
        // Hiding what was shown moves it over.
        page.narrow(Narrow::Hide(Pick::Pid(20)));
        assert!(page.only.is_empty());
        assert!(!passes(&page, 20) && passes(&page, 10));
    }

    #[test]
    fn minus_words_hide() {
        let mut page = ActivityPage::default();
        page.view.set_filter("hklm -policymanager");
        page.parse_filter();
        assert_eq!(page.words, ["hklm"]);
        assert_eq!(page.unwords, ["policymanager"]);
        let step = |path: &str| ActivityEvent {
            seq: 0,
            at: SystemTime::now(),
            pid: 4,
            tid: 1,
            kind: ActivityKind::RegOpenKey,
            path: path.into(),
            detail: String::new(),
            status: Some(0),
            count: 1,
            bytes: 0,
            took: Duration::ZERO,
        };
        assert!(page.passes(&step(r"HKLM\Software\Microsoft")));
        assert!(!page.passes(&step(r"HKLM\Software\Microsoft\PolicyManager\default")));
        assert!(!page.passes(&step(r"HKCU\Software")));
    }

    #[test]
    fn old_steps_roll_off_and_rows_follow() {
        let mut page = ActivityPage {
            keep: 1000,
            ..Default::default()
        };
        let step = |seq: u64, pid: u32| ActivityEvent {
            seq,
            at: SystemTime::now(),
            pid,
            tid: 1,
            kind: ActivityKind::FileRead,
            path: "C:\\x".into(),
            detail: String::new(),
            status: None,
            count: 1,
            bytes: 0,
            took: Duration::ZERO,
        };
        page.names.insert(1, "a.exe".into());
        page.names.insert(2, "b.exe".into());
        page.narrow(Narrow::Hide(Pick::Name("B.EXE".into())));
        let n = 1125;
        page.add((0..n).map(|i| step(i, 1 + (i % 2) as u32)).collect());
        while page.refilter() {}
        assert_eq!(page.visible.len() as u64, n.div_ceil(2));
        page.add(vec![step(n, 1)]);
        assert_eq!(page.events.len(), 1000);
        assert_eq!(page.first, n + 1 - 1000);
        assert!(page.visible.iter().all(|&i| i >= page.first));
        while page.refilter() {}
        assert_eq!(page.at(*page.visible.last().unwrap()).seq, n);
        assert_eq!(
            page.events
                .binary_search_by_key(&n, |e| e.seq)
                .map(|i| page.events[i].pid),
            Ok(1)
        );
    }

    #[test]
    fn sorted_rows_take_new_ones_in_order() {
        let mut page = ActivityPage::default();
        let step = |seq: u64, path: &str| ActivityEvent {
            seq,
            at: SystemTime::now(),
            pid: 1,
            tid: 1,
            kind: ActivityKind::FileOpen,
            path: path.into(),
            detail: String::new(),
            status: Some(0),
            count: 1,
            bytes: 0,
            took: Duration::ZERO,
        };
        page.add(vec![step(0, "c"), step(1, "A"), step(2, "e")]);
        while page.refilter() {}
        page.view.sort = Some((3, false));
        page.order();
        page.add(vec![step(3, "B"), step(4, "d")]);
        while page.refilter() {}
        let paths: Vec<&str> = page.visible.iter().map(|&i| &*page.at(i).path).collect();
        assert_eq!(paths, ["A", "B", "c", "d", "e"]);
    }
}
