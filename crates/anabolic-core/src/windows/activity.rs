//! What a program does, step by step, the way Process Monitor shows it:
//! the processes it starts, the DLLs it loads, every file it opens,
//! creates, reads, writes, renames or deletes, every registry key and value
//! it touches, and its network connections, with the result of each.
//!
//! All of it comes from one private kernel trace (the system logger): no
//! driver, no hook, and the program is never opened: recording a game
//! touches it no more than watching it in the process list.
//! Facts measured on Windows 11 build 26200 that shape the code:
//!
//! - FileIo, Registry and image-load events carry the issuing process and
//!   thread in their header; TcpIp/UdpIp do not (the header says
//!   0xFFFFFFFF) and carry the pid in the payload instead.
//! - Registry events name a key relative to a key control block (KCB)
//!   address. KCBs created while tracing announce their full name (opcode
//!   22), but the ones Windows had already cached are only named in a
//!   rundown, and a kernel session runs the KCB rundown when it STOPS
//!   (opcode 25, ~6,000 keys here, ~150 ms). So a short helper session is
//!   started and stopped right away to learn them.
//! - File creates name the file (and their FileObject); reads, writes and
//!   info events only carry FileObject and FileKey. Files opened before
//!   recording started are named by the same trick: a helper session whose
//!   stop rundown lists every open file (only the wanted ones are kept).
//! - `del` deletes through FILE_DELETE_ON_CLOSE on the create, renames show
//!   as a Rename info event followed by RenamePath with the new name.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::System::Diagnostics::Etw::{
    EVENT_RECORD, EVENT_TRACE_FLAG_DISK_FILE_IO, EVENT_TRACE_FLAG_FILE_IO,
    EVENT_TRACE_FLAG_FILE_IO_INIT, EVENT_TRACE_FLAG_IMAGE_LOAD, EVENT_TRACE_FLAG_NETWORK_TCPIP,
    EVENT_TRACE_FLAG_PROCESS, EVENT_TRACE_FLAG_REGISTRY, EVENT_TRACE_SYSTEM_LOGGER_MODE,
    FileIoGuid, ImageLoadGuid, ProcessGuid, RegistryGuid, TcpIpGuid, UdpIpGuid,
};

use crate::etw::{self, Buffers, QpcClock, Session, u32_at, u64_at};
use crate::winfile::DeviceMap;

use super::*;

#[path = "activity/launch.rs"]
mod launch;
#[path = "activity/parse.rs"]
mod parse;
use launch::*;
pub use parse::*;

/// Consecutive reads (or writes) of one file by one thread, and packets to
/// one endpoint, within this long are one event.
const MERGE_MS: f64 = 1000.0;
/// A file create waits this long for its result before it is shown anyway.
const RESULT_WAIT_MS: f64 = 2000.0;
/// A key name being looked up in a rundown is waited for this long…
const NAME_WAIT_MS: f64 = 3000.0;
/// …and a file name this long: a file rundown takes a second or two, and
/// may have to wait its turn (`FILE_RUNDOWN_GAP`).
const FILE_NAME_WAIT_MS: f64 = 8000.0;
/// Events are handed over at least this often (trace time).
const FLUSH_MS: f64 = 100.0;
/// File rundowns cost about a second of kernel time and ~0.6 s of ours:
/// at most this often, and only while names are wanted.
const FILE_RUNDOWN_GAP: Duration = Duration::from_secs(5);
/// Key rundowns are cheap (~6,000 keys) but still not every moment.
const KEY_RUNDOWN_GAP: Duration = Duration::from_secs(5);
/// Session names: several recordings (and their name rundowns) may run in
/// one process at once, and a session started under a name in use stops
/// the one before it.
static INSTANCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// A launched program has this long to show up.
const LAUNCH_WAIT: Duration = Duration::from_secs(15);

// FileIo opcodes.
const FILE_NAME: u8 = 0;
const FILE_RUNDOWN: u8 = 36;
const FILE_CREATE: u8 = 64;
const FILE_READ: u8 = 67;
const FILE_WRITE: u8 = 68;
const FILE_SET_INFO: u8 = 69;
const FILE_DELETE: u8 = 70;
const FILE_RENAME: u8 = 71;
const FILE_DIR_ENUM: u8 = 72;
const FILE_OP_END: u8 = 76;
const FILE_DELETE_PATH: u8 = 79;
const FILE_RENAME_PATH: u8 = 80;

// Registry opcodes (Registry_TypeGroup1: InitialTime, Status, Index,
// KeyHandle, KeyName).
const REG_CREATE: u8 = 10;
const REG_OPEN: u8 = 11;
const REG_DELETE: u8 = 12;
const REG_SET_VALUE: u8 = 14;
const REG_DELETE_VALUE: u8 = 15;
const REG_QUERY_VALUE: u8 = 16;
const REG_ENUM_KEY: u8 = 17;
const REG_ENUM_VALUE: u8 = 18;
const REG_QUERY_MULTIPLE: u8 = 19;
const REG_KCB_CREATE: u8 = 22;
const REG_KCB_RUNDOWN: u8 = 25;

// Process and image opcodes.
const PROCESS_START: u8 = 1;
const PROCESS_END: u8 = 2;
const IMAGE_LOAD: u8 = 10;

/// FILE_DELETE_ON_CLOSE in a create's options.
const DELETE_ON_CLOSE: u32 = 0x1000;
/// FILE_DIRECTORY_FILE.
const DIRECTORY_FILE: u32 = 0x1;
/// FileBasicInformation (times, attributes), FileDispositionInformation(Ex).
const INFO_BASIC: u32 = 4;
const INFO_DISPOSITION: u32 = 13;
const INFO_DISPOSITION_EX: u32 = 64;

impl ActivityEvent {
    /// The result in a word, as Process Monitor puts it.
    pub fn result_text(&self) -> String {
        match (self.kind, self.status) {
            (_, None) => String::new(),
            (ActivityKind::ProcessExit, Some(code)) => format!("exit code {code}"),
            (_, Some(status)) => status_text(status),
        }
    }

    /// `result_text` when it is a fixed phrase: None when there is no
    /// result or it must be put together.
    pub fn result_word(&self) -> Option<&'static str> {
        match (self.kind, self.status) {
            (ActivityKind::ProcessExit, _) | (_, None) => None,
            (_, Some(status)) => status_word(status),
        }
    }

    pub fn failed(&self) -> bool {
        self.kind != ActivityKind::ProcessExit
            && self.status.is_some_and(|s| s & 0xC000_0000 == 0xC000_0000)
    }
}

/// An NTSTATUS in words; the code itself for the rare ones.
pub fn status_text(status: u32) -> String {
    status_word(status)
        .map(str::to_string)
        .unwrap_or_else(|| format!("0x{status:08X}"))
}

/// An NTSTATUS in words, when it has them (filters ask millions of times).
pub fn status_word(status: u32) -> Option<&'static str> {
    Some(match status {
        0 => "success",
        0x0000_0103 => "pending",
        0x0000_0104 => "reparse",
        0x8000_0005 => "buffer overflow",
        0x8000_0006 => "no more files",
        0x8000_001A => "no more entries",
        0xC000_000D => "invalid parameter",
        0xC000_000F => "no such file",
        0xC000_0022 => "access denied",
        0xC000_0023 => "buffer too small",
        0xC000_0033 => "name invalid",
        0xC000_0034 => "name not found",
        0xC000_0035 => "name collision",
        0xC000_003A => "path not found",
        0xC000_0043 => "sharing violation",
        0xC000_0056 => "delete pending",
        0xC000_0061 => "privilege not held",
        0xC000_00BA => "is a directory",
        0xC000_0101 => "directory not empty",
        0xC000_0103 => "not a directory",
        0xC000_0121 => "cannot delete",
        0xC000_0135 => "DLL not found",
        0xC000_0225 => "not found",
        0xC000_0236 => "connection refused",
        0xC000_023A => "connection aborted",
        0xC000_00B5 => "timed out",
        _ => return None,
    })
}

/// Who to record from the start.
enum Target {
    /// Every process but Anabolic itself.
    Everything,
    Attach(Vec<u32>),
    Launch {
        program: String,
        args: String,
        dir: Option<String>,
        how: LaunchHow,
    },
}

enum Control {
    /// Record this process from now on (and what it starts).
    Track(u32),
    /// A program is being launched: track the process with this image
    /// name (the kernel keeps 14 characters of it) started by `parent`
    /// (any parent when None).
    Await(Awaited),
    Kcbs(HashMap<u64, Arc<str>>),
    Files(HashMap<u64, Arc<str>>),
    /// The first names are known: events may flow.
    Ready,
}

/// The program a launch waits for. Image and parent alone are not enough:
/// another copy may start at the same moment (a test's own `cmd.exe`, a
/// second window of the program), so its arguments must be on the command
/// line too.
#[derive(Clone, Debug)]
struct Awaited {
    image: String,
    parent: Option<u32>,
    args: String,
}

impl Awaited {
    fn matches(&self, p: &ProcessFacts) -> bool {
        image_matches(&p.image, &self.image)
            && self.parent.is_none_or(|parent| parent == p.parent)
            && (self.args.is_empty() || p.command_line.contains(&self.args))
    }
}

/// What the UI reads, swapped out on `take`.
#[derive(Default)]
struct Shared {
    out: Vec<ActivityEvent>,
    summary: ActivitySummary,
    tracked: Vec<u32>,
    state: Option<RecorderState>,
}

/// Records what a set of processes does; the session stops on drop.
pub struct ActivityRecorder {
    session: Option<Session>,
    consumer: Option<thread::JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
    control: Sender<Control>,
    /// File rundowns are worth their cost (processes from before).
    rundowns: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

// ---------------------------------------------------------------------------
// Payload parsing (pure; unit-tested from byte arrays).

/// Where an event's path still has to come from.
#[derive(Clone, Debug)]
enum Unnamed {
    /// A key (KCB) and what follows it.
    Key(u64, String),
    /// A file object or key.
    File(u64, u64),
}

struct Slot {
    ev: ActivityEvent,
    /// Still growing (merging, or waiting for its result) until this trace
    /// time; 0 when done.
    open_until: i64,
    unnamed: Option<Unnamed>,
    /// Given up waiting for a name at this trace time.
    name_deadline: i64,
    /// Whole machine: repeats of this step fold into it until this trace
    /// time.
    fold_until: i64,
    /// Went into an earlier row (`Consumer::fold`): not handed over.
    folded: bool,
}

/// Whole machine: steps that are the same but for their time.
fn fold_key(e: &ActivityEvent) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    (e.pid, e.kind, &*e.path, e.detail.as_str(), e.status).hash(&mut h);
    h.finish()
}

fn same_step(a: &ActivityEvent, b: &ActivityEvent) -> bool {
    a.pid == b.pid
        && a.kind == b.kind
        && a.status == b.status
        && a.path == b.path
        && a.detail == b.detail
}

/// A run of reads or writes by one thread.
struct Run {
    seq: u64,
    object: u64,
    write: bool,
}

struct Consumer {
    clock: QpcClock,
    devices: DeviceMap,
    user_sid: String,
    shared: Arc<Mutex<Shared>>,
    control: Receiver<Control>,
    /// Asks the helper to name these files (object or key addresses).
    wanted_files: Arc<Mutex<HashSet<u64>>>,
    wanted_keys: Arc<AtomicBool>,
    tracked: HashSet<u32>,
    awaiting: Option<Awaited>,
    ready: bool,
    kcbs: HashMap<u64, Arc<str>>,
    /// File object or file key -> path.
    files: HashMap<u64, Arc<str>>,
    /// Creates (and renames, deletes) waiting for their result, by IRP.
    pending: HashMap<u64, u64>,
    runs: HashMap<u32, Run>,
    /// (pid, kind, remote, remote port, local port) -> open slot.
    flows: HashMap<(u32, ActivityKind, IpAddr, u16, u16), u64>,
    queue: VecDeque<Slot>,
    /// Seq of `queue[0]`.
    base: u64,
    next_seq: u64,
    last_flush: i64,
    summary: ActivitySummary,
    written: HashSet<Arc<str>>,
    endpoints: HashSet<(IpAddr, u16)>,
    lost: Box<dyn Fn() -> u32 + Send>,
    /// The consumer thread runs above normal priority (set on its first
    /// event): falling behind loses events for good.
    boosted: bool,
    /// Every process (but this one), not only `tracked`.
    everything: bool,
    own: u32,
    /// Whole machine: `fold_key` -> seq of the row repeats fold into.
    folds: HashMap<u64, u64>,
    /// One copy of each path (keys and files repeat a great deal).
    paths: HashSet<Arc<str>>,
}

impl Consumer {
    /// The shared copy of a path.
    fn intern(&mut self, path: String) -> Arc<str> {
        if let Some(p) = self.paths.get(path.as_str()) {
            return p.clone();
        }
        if self.paths.len() >= 500_000 {
            self.paths.clear();
        }
        let p: Arc<str> = path.into();
        self.paths.insert(p.clone());
        p
    }

    /// Whole machine: what a process repeats within a second (the same
    /// operation on the same path, with the same detail and result) is one
    /// row with a count. Explorer alone opens some keys 170 times a second,
    /// and about half of all rows are such repeats. Called once the step
    /// at `seq` is final; returns whether it went into an earlier row.
    fn fold(&mut self, at: i64, seq: u64) -> bool {
        if !self.everything {
            return false;
        }
        let Some(i) = seq.checked_sub(self.base).map(|i| i as usize) else {
            return false;
        };
        let Some(slot) = self.queue.get(i) else {
            return false;
        };
        if slot.unnamed.is_some() || slot.ev.path.is_empty() {
            return false;
        }
        let key = fold_key(&slot.ev);
        if let Some(&into) = self.folds.get(&key)
            && into != seq
            && let Some(j) = into.checked_sub(self.base).map(|j| j as usize)
            && let Some(target) = self.queue.get(j)
            && target.fold_until > at
            && same_step(&target.ev, &slot.ev)
        {
            let (count, bytes) = (slot.ev.count, slot.ev.bytes);
            let target = &mut self.queue[j];
            target.ev.count += count;
            target.ev.bytes += bytes;
            let slot = &mut self.queue[i];
            slot.folded = true;
            slot.open_until = 0;
            return true;
        }
        self.folds.insert(key, seq);
        let until = at + self.ticks(MERGE_MS);
        let slot = &mut self.queue[i];
        slot.fold_until = until;
        slot.open_until = slot.open_until.max(until);
        false
    }

    /// Whether this process's steps are recorded.
    fn wants(&self, pid: u32) -> bool {
        if self.everything {
            pid != self.own
        } else {
            self.tracked.contains(&pid)
        }
    }

    fn ticks(&self, ms: f64) -> i64 {
        self.clock.from_ms(ms)
    }

    fn dos_path(&self, nt: &str) -> String {
        if let Some(rest) = nt.strip_prefix("\\??\\") {
            return rest.to_string();
        }
        if let Some(rest) = nt.strip_prefix("\\Device\\Mup\\") {
            return format!("\\\\{rest}");
        }
        self.devices
            .to_dos_path(nt)
            .unwrap_or_else(|| nt.to_string())
    }

    fn key_path(&self, kcb: u64, rest: &str) -> Option<String> {
        let base = if kcb == 0 {
            String::new()
        } else {
            self.kcbs.get(&kcb)?.to_string()
        };
        let full = match (base.is_empty(), rest.is_empty()) {
            (true, _) => rest.to_string(),
            (false, true) => base,
            (false, false) => format!("{base}\\{rest}"),
        };
        Some(friendly_key(&full, &self.user_sid))
    }

    fn file_path(&self, object: u64, key: u64) -> Option<Arc<str>> {
        self.files
            .get(&object)
            .or_else(|| self.files.get(&key))
            .cloned()
    }

    /// Adds an event at the end of the timeline; returns its seq.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        at: i64,
        pid: u32,
        tid: u32,
        kind: ActivityKind,
        path: Arc<str>,
        detail: String,
        status: Option<u32>,
        bytes: u64,
        open_ms: f64,
        unnamed: Option<Unnamed>,
    ) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push_back(Slot {
            ev: ActivityEvent {
                seq,
                at: self.clock.wall(at),
                pid,
                tid,
                kind,
                path,
                detail,
                status,
                count: 1,
                bytes,
                took: Duration::ZERO,
            },
            open_until: if open_ms > 0.0 {
                at + self.ticks(open_ms)
            } else {
                0
            },
            name_deadline: at
                + self.ticks(match unnamed {
                    Some(Unnamed::File(..)) => FILE_NAME_WAIT_MS,
                    _ => NAME_WAIT_MS,
                }),
            unnamed,
            fold_until: 0,
            folded: false,
        });
        seq
    }

    fn slot(&mut self, seq: u64) -> Option<&mut Slot> {
        let at = seq.checked_sub(self.base)? as usize;
        self.queue.get_mut(at)
    }

    /// A thread's read/write run ends when it does anything else.
    fn end_run(&mut self, tid: u32) {
        if let Some(run) = self.runs.remove(&tid)
            && let Some(slot) = self.slot(run.seq)
        {
            slot.open_until = 0;
        }
    }

    fn control(&mut self) {
        while let Ok(c) = self.control.try_recv() {
            match c {
                Control::Track(pid) => {
                    self.tracked.insert(pid);
                }
                Control::Await(awaited) => self.awaiting = Some(awaited),
                Control::Kcbs(names) => {
                    for (k, v) in names {
                        self.kcbs.entry(k).or_insert(v);
                    }
                }
                Control::Files(names) => {
                    for (k, v) in names {
                        let path: Arc<str> = self.dos_path(&v).into();
                        self.files.entry(k).or_insert(path);
                    }
                }
                Control::Ready => self.ready = true,
            }
        }
    }

    fn on_event(&mut self, record: &EVENT_RECORD) {
        if !self.boosted {
            self.boosted = true;
            crate::hardening::set_current_thread_priority(
                crate::hardening::ThreadPriority::AboveNormal,
            );
        }
        self.summary.seen += 1;
        let h = &record.EventHeader;
        let at = h.TimeStamp;
        let opcode = h.EventDescriptor.Opcode;
        let d = etw::user_data(record);
        let provider = h.ProviderId;
        if provider == FileIoGuid {
            self.on_file(at, h.ProcessId, h.ThreadId, opcode, d);
        } else if provider == RegistryGuid {
            self.on_registry(at, h.ProcessId, h.ThreadId, opcode, d);
        } else if provider == TcpIpGuid || provider == UdpIpGuid {
            self.on_net(at, provider == TcpIpGuid, opcode, d);
        } else if provider == ProcessGuid {
            self.on_process(at, h.ThreadId, opcode, d);
        } else if provider == ImageLoadGuid
            && opcode == IMAGE_LOAD
            && let Some((pid, path)) = parse_image(d)
            && self.wants(pid)
        {
            let path = self.intern(self.dos_path(&path));
            self.summary.images_loaded += 1;
            self.push(
                at,
                pid,
                h.ThreadId,
                ActivityKind::ImageLoad,
                path,
                String::new(),
                None,
                0,
                0.0,
                None,
            );
        }
        if at - self.last_flush >= self.ticks(FLUSH_MS) {
            self.last_flush = at;
            self.control();
            self.flush(at, false);
        }
    }

    fn on_process(&mut self, at: i64, tid: u32, opcode: u8, d: &[u8]) {
        if opcode != PROCESS_START && opcode != PROCESS_END {
            return;
        }
        let Some(p) = parse_process(d) else { return };
        // A launch announced a moment ago must be known before its own
        // start event is judged (flushes read the controls only now and
        // then).
        self.control();
        if opcode == PROCESS_START {
            // A program being launched for recording.
            if self.awaiting.as_ref().is_some_and(|a| a.matches(&p)) {
                self.awaiting = None;
                self.tracked.insert(p.pid);
                if let Ok(mut s) = self.shared.lock() {
                    s.state = Some(RecorderState::Recording);
                }
            }
            if self.wants(p.parent) {
                if !self.everything {
                    self.tracked.insert(p.pid);
                }
                self.summary.processes_started += 1;
                self.push(
                    at,
                    p.parent,
                    tid,
                    ActivityKind::ProcessStart,
                    p.image.clone().into(),
                    format!("PID {}  ·  {}", p.pid, p.command_line),
                    None,
                    0,
                    0.0,
                    None,
                );
            } else if self.tracked.contains(&p.pid) {
                // The launched program itself: its own first step.
                self.push(
                    at,
                    p.pid,
                    tid,
                    ActivityKind::ProcessStart,
                    p.image.clone().into(),
                    format!("PID {}  ·  {}", p.pid, p.command_line),
                    None,
                    0,
                    0.0,
                    None,
                );
            }
        } else if self.wants(p.pid) {
            self.push(
                at,
                p.pid,
                tid,
                ActivityKind::ProcessExit,
                p.image.into(),
                String::new(),
                Some(p.exit),
                0,
                0.0,
                None,
            );
        }
    }

    fn on_file(&mut self, at: i64, pid: u32, tid: u32, opcode: u8, d: &[u8]) {
        match opcode {
            // A result: only IRPs we wait on matter.
            FILE_OP_END => {
                if self.pending.is_empty() {
                    return;
                }
                let (Some(irp), Some(info), Some(status)) =
                    (u64_at(d, 0), u64_at(d, 8), u32_at(d, 16))
                else {
                    return;
                };
                let Some(seq) = self.pending.remove(&irp) else {
                    return;
                };
                let mut created = false;
                if let Some(slot) = self.slot(seq) {
                    slot.ev.status = Some(status);
                    slot.open_until = 0;
                    if matches!(
                        slot.ev.kind,
                        ActivityKind::FileOpen | ActivityKind::FileCreate
                    ) && status == 0
                        && let Some(outcome) = create_outcome(info)
                    {
                        slot.ev.detail = format!("{}  ·  {outcome}", slot.ev.detail);
                        if info == 2 || info == 3 || info == 0 {
                            slot.ev.kind = ActivityKind::FileCreate;
                            created = true;
                        }
                    }
                }
                if created {
                    self.summary.files_created += 1;
                }
                self.fold(at, seq);
                return;
            }
            // Names of files Windows knew before: from the helper only.
            FILE_NAME | FILE_RUNDOWN => return,
            _ => {}
        }
        if !self.wants(pid) {
            return;
        }
        match opcode {
            FILE_CREATE => {
                // IrpPtr, FileObject, TTID, CreateOptions, FileAttributes,
                // ShareAccess, OpenPath.
                let (Some(irp), Some(object), Some(options), Some(path)) =
                    (u64_at(d, 0), u64_at(d, 8), u32_at(d, 20), wide_at(d, 32))
                else {
                    return;
                };
                self.end_run(tid);
                let path = self.intern(self.dos_path(&path));
                self.files.insert(object, path.clone());
                let kind = if options & DELETE_ON_CLOSE != 0 {
                    self.summary.files_deleted += 1;
                    ActivityKind::FileDelete
                } else {
                    ActivityKind::FileOpen
                };
                let seq = self.push(
                    at,
                    pid,
                    tid,
                    kind,
                    path,
                    create_detail(options),
                    None,
                    0,
                    RESULT_WAIT_MS,
                    None,
                );
                self.pending.insert(irp, seq);
            }
            FILE_READ | FILE_WRITE => {
                // Offset, IrpPtr, FileObject, FileKey, TTID, IoSize, IoFlags.
                let (Some(offset), Some(object), Some(key), Some(size)) =
                    (u64_at(d, 0), u64_at(d, 16), u64_at(d, 24), u32_at(d, 36))
                else {
                    return;
                };
                let write = opcode == FILE_WRITE;
                // The same file, the same way, by the same thread: merge.
                if let Some(run) = self.runs.get(&tid)
                    && run.object == object
                    && run.write == write
                {
                    let seq = run.seq;
                    if let Some(slot) = self.slot(seq)
                        && slot.open_until > at
                    {
                        slot.ev.count += 1;
                        slot.ev.bytes += size as u64;
                        return;
                    }
                }
                self.end_run(tid);
                let (path, unnamed) = match self.file_path(object, key) {
                    Some(p) => (p, None),
                    None => {
                        if let Ok(mut w) = self.wanted_files.lock() {
                            w.insert(object);
                            w.insert(key);
                        }
                        (Arc::from(""), Some(Unnamed::File(object, key)))
                    }
                };
                if write && !path.is_empty() && self.written.insert(path.clone()) {
                    self.summary.files_written += 1;
                }
                let seq = self.push(
                    at,
                    pid,
                    tid,
                    if write {
                        ActivityKind::FileWrite
                    } else {
                        ActivityKind::FileRead
                    },
                    path,
                    format!("from offset {offset}"),
                    None,
                    size as u64,
                    MERGE_MS,
                    unnamed,
                );
                self.runs.insert(tid, Run { seq, object, write });
            }
            FILE_SET_INFO | FILE_DELETE | FILE_RENAME | FILE_DELETE_PATH | FILE_RENAME_PATH => {
                // IrpPtr, FileObject, FileKey, ExtraInfo, TTID, InfoClass,
                // and a path for the *Path ones.
                let (Some(irp), Some(object), Some(key), Some(class)) =
                    (u64_at(d, 0), u64_at(d, 8), u64_at(d, 16), u32_at(d, 36))
                else {
                    return;
                };
                self.end_run(tid);
                let named = |c: &Self| c.file_path(object, key);
                match opcode {
                    // The new name follows in RenamePath.
                    FILE_RENAME => {}
                    FILE_RENAME_PATH => {
                        let to = wide_at(d, 40)
                            .map(|p| self.dos_path(&p))
                            .unwrap_or_default();
                        let from = named(self).unwrap_or_else(|| Arc::from(""));
                        let seq = self.push(
                            at,
                            pid,
                            tid,
                            ActivityKind::FileRename,
                            from,
                            format!("to {to}"),
                            None,
                            0,
                            RESULT_WAIT_MS,
                            None,
                        );
                        self.pending.insert(irp, seq);
                        if !to.is_empty() {
                            self.files.insert(object, to.into());
                        }
                    }
                    FILE_DELETE_PATH => {
                        // The delete before it said which file; a path
                        // here is better than a guess.
                        if let Some(&seq) = self.pending.get(&irp)
                            && let Some(path) = wide_at(d, 40)
                        {
                            let path = self.intern(self.dos_path(&path));
                            if let Some(slot) = self.slot(seq) {
                                slot.ev.path = path;
                                slot.unnamed = None;
                            }
                        }
                    }
                    _ => {
                        let delete = opcode == FILE_DELETE
                            || class == INFO_DISPOSITION
                            || class == INFO_DISPOSITION_EX;
                        if !delete && class != INFO_BASIC {
                            // End of file, allocation...: noise of writes.
                            return;
                        }
                        let (path, unnamed) = match named(self) {
                            Some(p) => (p, None),
                            None => (Arc::from(""), Some(Unnamed::File(object, key))),
                        };
                        if delete {
                            self.summary.files_deleted += 1;
                        }
                        let seq = self.push(
                            at,
                            pid,
                            tid,
                            if delete {
                                ActivityKind::FileDelete
                            } else {
                                ActivityKind::FileSetInfo
                            },
                            path,
                            if delete {
                                "marked for deletion".into()
                            } else {
                                "times or attributes".into()
                            },
                            None,
                            0,
                            RESULT_WAIT_MS,
                            unnamed,
                        );
                        self.pending.insert(irp, seq);
                    }
                }
            }
            FILE_DIR_ENUM => {
                // IrpPtr, FileObject, FileKey, TTID, Length, InfoClass,
                // FileIndex, FileName (the pattern).
                let (Some(object), Some(key)) = (u64_at(d, 8), u64_at(d, 16)) else {
                    return;
                };
                let pattern = wide_at(d, 40).unwrap_or_default();
                // One listing per folder and thread per second.
                if let Some(run) = self.runs.get(&tid)
                    && run.object == object
                    && !run.write
                {
                    let seq = run.seq;
                    if let Some(slot) = self.slot(seq)
                        && slot.ev.kind == ActivityKind::FileListDir
                        && slot.open_until > at
                    {
                        slot.ev.count += 1;
                        return;
                    }
                }
                self.end_run(tid);
                let (path, unnamed) = match self.file_path(object, key) {
                    Some(p) => (p, None),
                    None => (Arc::from(""), Some(Unnamed::File(object, key))),
                };
                let seq = self.push(
                    at,
                    pid,
                    tid,
                    ActivityKind::FileListDir,
                    path,
                    if pattern.is_empty() {
                        String::new()
                    } else {
                        format!("matching {pattern}")
                    },
                    None,
                    0,
                    MERGE_MS,
                    unnamed,
                );
                self.runs.insert(
                    tid,
                    Run {
                        seq,
                        object,
                        write: false,
                    },
                );
            }
            _ => {}
        }
    }

    fn on_registry(&mut self, at: i64, pid: u32, tid: u32, opcode: u8, d: &[u8]) {
        let Some((status, index, kcb, name)) = parse_registry(d) else {
            return;
        };
        // Names for keys created while recording, whoever creates them.
        if opcode == REG_KCB_CREATE || opcode == REG_KCB_RUNDOWN {
            if !name.is_empty() {
                self.kcbs.insert(kcb, name.into());
            }
            return;
        }
        if !self.wants(pid) {
            return;
        }
        let (kind, rest, detail) = match opcode {
            REG_OPEN => (ActivityKind::RegOpenKey, name, String::new()),
            REG_CREATE => (ActivityKind::RegCreateKey, name, String::new()),
            REG_DELETE => (ActivityKind::RegDeleteKey, name, String::new()),
            REG_QUERY_VALUE | REG_QUERY_MULTIPLE => {
                (ActivityKind::RegQueryValue, String::new(), name)
            }
            REG_SET_VALUE => (ActivityKind::RegSetValue, String::new(), name),
            REG_DELETE_VALUE => (ActivityKind::RegDeleteValue, String::new(), name),
            REG_ENUM_KEY => (
                ActivityKind::RegEnumKey,
                String::new(),
                format!("index {index}"),
            ),
            REG_ENUM_VALUE => (
                ActivityKind::RegEnumValue,
                String::new(),
                format!("index {index}"),
            ),
            _ => return,
        };
        let detail = match kind {
            ActivityKind::RegQueryValue
            | ActivityKind::RegSetValue
            | ActivityKind::RegDeleteValue
                if detail.is_empty() =>
            {
                "(Default)".to_string()
            }
            _ => detail,
        };
        if status == 0 {
            match kind {
                ActivityKind::RegSetValue => self.summary.values_set += 1,
                ActivityKind::RegCreateKey => self.summary.keys_created += 1,
                ActivityKind::RegDeleteKey => self.summary.keys_deleted += 1,
                _ => {}
            }
        }
        self.end_run(tid);
        let (path, unnamed) = match self.key_path(kcb, &rest) {
            Some(p) => (self.intern(p), None),
            None => {
                self.wanted_keys.store(true, Ordering::Relaxed);
                (Arc::from(""), Some(Unnamed::Key(kcb, rest)))
            }
        };
        let seq = self.push(
            at,
            pid,
            tid,
            kind,
            path,
            detail,
            Some(status),
            0,
            0.0,
            unnamed,
        );
        // A repeat: gone again before anyone saw it.
        if self.fold(at, seq) {
            self.queue.pop_back();
            self.next_seq -= 1;
        }
    }

    fn on_net(&mut self, at: i64, tcp: bool, opcode: u8, d: &[u8]) {
        // TCP: send 10, receive 11, connect 12, disconnect 13, accept 15
        // (IPv6: +16). UDP: send 10, receive 11 (IPv6: +16).
        let v6 = opcode >= 26;
        let base = if v6 { opcode - 16 } else { opcode };
        let kind = match (tcp, base) {
            (_, 10) => ActivityKind::NetSend,
            (_, 11) => ActivityKind::NetReceive,
            (true, 12) => ActivityKind::NetConnect,
            (true, 13) => ActivityKind::NetDisconnect,
            (true, 15) => ActivityKind::NetAccept,
            _ => return,
        };
        let Some((pid, size, remote, remote_port, local_port)) = parse_net(d, v6) else {
            return;
        };
        if !self.wants(pid) {
            return;
        }
        if self.endpoints.insert((remote, remote_port)) {
            self.summary.endpoints += 1;
        }
        match kind {
            ActivityKind::NetSend => self.summary.sent += size as u64,
            ActivityKind::NetReceive => self.summary.received += size as u64,
            _ => {}
        }
        let endpoint = match remote {
            IpAddr::V6(v6) => format!("[{v6}]:{remote_port}"),
            v4 => format!("{v4}:{remote_port}"),
        };
        let proto = if tcp { "TCP" } else { "UDP" };
        let flow = (pid, kind, remote, remote_port, local_port);
        if matches!(kind, ActivityKind::NetSend | ActivityKind::NetReceive) {
            if let Some(&seq) = self.flows.get(&flow)
                && let Some(slot) = self.slot(seq)
                && slot.open_until > at
            {
                slot.ev.count += 1;
                slot.ev.bytes += size as u64;
                return;
            }
            let seq = self.push(
                at,
                pid,
                0,
                kind,
                endpoint.into(),
                format!("{proto}, local port {local_port}"),
                None,
                size as u64,
                MERGE_MS,
                None,
            );
            self.flows.insert(flow, seq);
        } else {
            self.push(
                at,
                pid,
                0,
                kind,
                endpoint.into(),
                format!("{proto}, local port {local_port}"),
                None,
                0,
                0.0,
                None,
            );
        }
    }

    /// Hands finished events over, in order. `all`: the recording ended,
    /// so nothing is waited for any more.
    fn flush(&mut self, now: i64, all: bool) {
        if !self.ready && !all {
            return;
        }
        let mut out = Vec::new();
        while let Some(front) = self.queue.front() {
            let open = front.open_until != 0 && front.open_until > now;
            if open && !all {
                break;
            }
            // A missing name: look again, wait a little, then give up.
            if let Some(unnamed) = front.unnamed.clone() {
                let found = match &unnamed {
                    Unnamed::Key(kcb, rest) => self.key_path(*kcb, rest).map(Arc::from),
                    Unnamed::File(object, key) => self.file_path(*object, *key),
                };
                match found {
                    Some(path) => {
                        let front = self.queue.front_mut().expect("front");
                        front.ev.path = path;
                        front.unnamed = None;
                    }
                    None if !all && front.name_deadline > now => break,
                    None => {
                        let front = self.queue.front_mut().expect("front");
                        front.ev.path = match unnamed {
                            Unnamed::Key(..) => "(a key opened before recording)".into(),
                            Unnamed::File(..) => "(a file opened before recording)".into(),
                        };
                        front.unnamed = None;
                    }
                }
            }
            let slot = self.queue.pop_front().expect("front");
            self.base += 1;
            if slot.folded {
                continue;
            }
            // The whole machine: the reader keeps what it has room for.
            if self.everything || self.summary.kept < MAX_EVENTS {
                self.summary.kept += 1;
                out.push(slot.ev);
            } else {
                self.summary.dropped += 1;
            }
        }
        // Waiting results whose events are gone.
        if self.pending.len() > 10_000 {
            let base = self.base;
            self.pending.retain(|_, seq| *seq >= base);
        }
        let base = self.base;
        self.flows.retain(|_, seq| *seq >= base);
        self.runs.retain(|_, run| run.seq >= base);
        self.folds.retain(|_, seq| *seq >= base);
        self.summary.lost = (self.lost)();
        if let Ok(mut s) = self.shared.lock() {
            s.out.append(&mut out);
            s.summary = self.summary.clone();
            let mut tracked: Vec<u32> = self.tracked.iter().copied().collect();
            tracked.sort_unstable();
            s.tracked = tracked;
        }
    }
}

impl Drop for Consumer {
    /// The session stopped: what is left goes out, finished or not.
    fn drop(&mut self) {
        self.control();
        self.ready = true;
        self.flush(i64::MAX, true);
    }
}

// ---------------------------------------------------------------------------
// Helper sessions: names Windows knew before recording.

/// Starts a short kernel session and stops it at once: its stop rundown
/// lists every key control block (registry) or open file (file names).
fn rundown(
    flags: u32,
    keep: impl Fn(u8, &[u8], &mut HashMap<u64, Arc<str>>) + Send + 'static,
) -> Result<HashMap<u64, Arc<str>>, String> {
    // The file rundown delivers ~530,000 names at once.
    let session = Session::start_instance(
        "Anabolic-ActivityNames",
        INSTANCE.fetch_add(1, Ordering::Relaxed),
        EVENT_TRACE_SYSTEM_LOGGER_MODE,
        flags,
        Buffers {
            kb: 512,
            min: 16,
            max: 64,
        },
    )?;
    let names = Arc::new(Mutex::new(HashMap::new()));
    let sink = names.clone();
    let consumer = session.consume(move |record| {
        let h = &record.EventHeader;
        if let Ok(mut n) = sink.lock() {
            keep(h.EventDescriptor.Opcode, etw::user_data(record), &mut n);
        }
    })?;
    drop(session);
    let _ = consumer.join();
    let names = std::mem::take(&mut *names.lock().map_err(|e| e.to_string())?);
    Ok(names)
}

fn key_rundown() -> Result<HashMap<u64, Arc<str>>, String> {
    rundown(EVENT_TRACE_FLAG_REGISTRY.0, |opcode, d, names| {
        if (opcode == REG_KCB_RUNDOWN || opcode == REG_KCB_CREATE)
            && let Some((_, _, kcb, name)) = parse_registry(d)
            && !name.is_empty()
        {
            names.insert(kcb, name.into());
        }
    })
}

fn file_rundown(wanted: HashSet<u64>) -> Result<HashMap<u64, Arc<str>>, String> {
    rundown(EVENT_TRACE_FLAG_DISK_FILE_IO.0, move |opcode, d, names| {
        // FileIo_Name: FileObject (or key), FileName.
        if matches!(opcode, FILE_NAME | 32 | FILE_RUNDOWN)
            && let Some(object) = u64_at(d, 0)
            && wanted.contains(&object)
            && let Some(name) = wide_at(d, 8)
            && !name.is_empty()
        {
            names.insert(object, name.into());
        }
    })
}

/// The helper thread: names first, then the launch, then file names on
/// request.
#[allow(clippy::too_many_arguments)]
fn helper(
    target: Target,
    control: Sender<Control>,
    shared: Arc<Mutex<Shared>>,
    wanted_files: Arc<Mutex<HashSet<u64>>>,
    wanted_keys: Arc<AtomicBool>,
    rundowns: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let fail = |why: String| {
        if let Ok(mut s) = shared.lock() {
            s.state = Some(RecorderState::Failed(why));
        }
    };
    // Keys Windows already had open, so registry steps have names.
    match key_rundown() {
        Ok(names) => {
            let _ = control.send(Control::Kcbs(names));
        }
        Err(e) => fail(format!("Could not read the registry's key names: {e}")),
    }
    let _ = control.send(Control::Ready);
    match target {
        Target::Everything => {
            rundowns.store(true, Ordering::Relaxed);
            if let Ok(mut s) = shared.lock()
                && s.state.is_none()
            {
                s.state = Some(RecorderState::Recording);
            }
        }
        Target::Attach(pids) => {
            // Running processes have files open from before: name them.
            rundowns.store(true, Ordering::Relaxed);
            for pid in pids {
                let _ = control.send(Control::Track(pid));
            }
            if let Ok(mut s) = shared.lock()
                && s.state.is_none()
            {
                s.state = Some(RecorderState::Recording);
            }
        }
        Target::Launch {
            program,
            args,
            dir,
            how,
        } => {
            let image = expected_image(&program);
            let elevated = crate::hardening::is_elevated();
            let through_shell = matches!(how, LaunchHow::Normal { .. }) && elevated;
            let _ = control.send(Control::Await(Awaited {
                image: image.clone(),
                parent: (!through_shell).then(std::process::id),
                args: args.trim().to_string(),
            }));
            let launched = match how {
                LaunchHow::Normal { show } if elevated => {
                    shell_launch(&program, &args, dir.as_deref(), show)
                }
                _ => {
                    use std::os::windows::process::CommandExt;
                    let mut command = std::process::Command::new(&program);
                    command.raw_arg(&args);
                    if let Some(dir) = &dir {
                        command.current_dir(dir);
                    }
                    if how == LaunchHow::Hidden {
                        command
                            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());
                    }
                    // Its pid is known here: tracked even if the await
                    // were to miss it.
                    command
                        .spawn()
                        .map(|child| {
                            let _ = control.send(Control::Track(child.id()));
                            if let Ok(mut s) = shared.lock() {
                                s.state = Some(RecorderState::Recording);
                            }
                        })
                        .map_err(|e| e.to_string())
                }
            };
            if let Err(e) = launched {
                fail(format!("Could not start {program}: {e}"));
            } else {
                let started = Instant::now();
                loop {
                    thread::sleep(Duration::from_millis(100));
                    let state = shared.lock().ok().and_then(|s| s.state.clone());
                    if state.is_some() || stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if started.elapsed() > LAUNCH_WAIT {
                        fail(format!(
                            "{program} was started but no process named {image} appeared within {} s \
                             (it may have handed over to a copy already running)",
                            LAUNCH_WAIT.as_secs()
                        ));
                        break;
                    }
                }
            }
        }
    }
    // Names for files and keys that were open before recording.
    let mut last_files: Option<Instant> = None;
    let mut last_keys = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(250));
        // A file rundown delivers every open file of the machine (~530k
        // events here, ~0.6 s of our CPU): only for processes that were
        // running before recording. A launched program opens its files
        // while recorded; what it inherits (console, pipes) stays unnamed.
        let due = rundowns.load(Ordering::Relaxed)
            && last_files.is_none_or(|t| t.elapsed() >= FILE_RUNDOWN_GAP);
        if due {
            let wanted = wanted_files
                .lock()
                .map(|mut w| std::mem::take(&mut *w))
                .unwrap_or_default();
            if !wanted.is_empty() {
                last_files = Some(Instant::now());
                if let Ok(names) = file_rundown(wanted) {
                    let _ = control.send(Control::Files(names));
                }
            }
        }
        if last_keys.elapsed() >= KEY_RUNDOWN_GAP && wanted_keys.swap(false, Ordering::Relaxed) {
            last_keys = Instant::now();
            if let Ok(names) = key_rundown() {
                let _ = control.send(Control::Kcbs(names));
            }
        }
    }
}

impl ActivityRecorder {
    /// Records a running process, and what it starts from now on.
    pub fn attach(pid: u32) -> Result<Self, String> {
        Self::start(Target::Attach(vec![pid]))
    }

    /// Records every process of the machine but Anabolic itself, the way
    /// Process Monitor does: what is wanted is found by filtering after.
    pub fn everything() -> Result<Self, String> {
        Self::start(Target::Everything)
    }

    /// Records several running processes (a program and its children).
    pub fn attach_all(pids: &[u32]) -> Result<Self, String> {
        Self::start(Target::Attach(pids.to_vec()))
    }

    /// Starts a program and records it from its first step. When Anabolic
    /// runs as administrator the program is started as the signed-in user
    /// (through Explorer), not elevated. Returns at once: `state` says when
    /// the program showed up, or why not.
    pub fn launch(program: &str, args: &str, dir: Option<&str>) -> Result<Self, String> {
        Self::launch_with(program, args, dir, LaunchHow::Normal { show: 1 })
    }

    pub fn launch_with(
        program: &str,
        args: &str,
        dir: Option<&str>,
        how: LaunchHow,
    ) -> Result<Self, String> {
        Self::start(Target::Launch {
            program: program.to_string(),
            args: args.to_string(),
            dir: dir.map(str::to_string),
            how,
        })
    }

    fn start(target: Target) -> Result<Self, String> {
        if !crate::hardening::is_elevated() {
            return Err(
                "recording needs Anabolic running as administrator (a kernel trace)".into(),
            );
        }
        let flags = EVENT_TRACE_FLAG_PROCESS.0
            | EVENT_TRACE_FLAG_IMAGE_LOAD.0
            | EVENT_TRACE_FLAG_FILE_IO.0
            | EVENT_TRACE_FLAG_FILE_IO_INIT.0
            | EVENT_TRACE_FLAG_REGISTRY.0
            | EVENT_TRACE_FLAG_NETWORK_TCPIP.0;
        // Room for bursts (100,000+ events a second on a busy machine): a
        // program's first second is the one that matters most.
        let session = Session::start_instance(
            "Anabolic-Activity",
            INSTANCE.fetch_add(1, Ordering::Relaxed),
            EVENT_TRACE_SYSTEM_LOGGER_MODE,
            flags,
            Buffers {
                kb: 512,
                min: 32,
                max: 128,
            },
        )?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (control_tx, control_rx) = unbounded();
        let wanted_files = Arc::new(Mutex::new(HashSet::new()));
        let wanted_keys = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let rundowns = Arc::new(AtomicBool::new(false));
        // The consumer reads the session's loss count through its name.
        let lost_name = session_lost_reader(&session);
        let everything = matches!(target, Target::Everything);
        let mut consumer = Consumer {
            clock: QpcClock::new(),
            devices: DeviceMap::build(),
            user_sid: user_sid(),
            shared: shared.clone(),
            control: control_rx,
            wanted_files: wanted_files.clone(),
            wanted_keys: wanted_keys.clone(),
            tracked: HashSet::new(),
            awaiting: None,
            ready: false,
            kcbs: HashMap::new(),
            files: HashMap::new(),
            pending: HashMap::new(),
            runs: HashMap::new(),
            flows: HashMap::new(),
            queue: VecDeque::new(),
            base: 0,
            next_seq: 0,
            last_flush: 0,
            summary: ActivitySummary::default(),
            written: HashSet::new(),
            endpoints: HashSet::new(),
            lost: lost_name,
            boosted: false,
            everything,
            own: std::process::id(),
            folds: HashMap::new(),
            paths: HashSet::new(),
        };
        let handle = session.consume(move |record| consumer.on_event(record))?;
        {
            let (control, shared, wanted_files, wanted_keys, rundowns, stop) = (
                control_tx.clone(),
                shared.clone(),
                wanted_files,
                wanted_keys,
                rundowns.clone(),
                stop.clone(),
            );
            thread::Builder::new()
                .name("anabolic-activity".into())
                .spawn(move || {
                    helper(
                        target,
                        control,
                        shared,
                        wanted_files,
                        wanted_keys,
                        rundowns,
                        stop,
                    )
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            session: Some(session),
            consumer: Some(handle),
            shared,
            control: control_tx,
            rundowns,
            stop,
        })
    }

    /// The events since the last call, in order.
    pub fn take(&mut self) -> Vec<ActivityEvent> {
        self.shared
            .lock()
            .map(|mut s| std::mem::take(&mut s.out))
            .unwrap_or_default()
    }

    pub fn summary(&self) -> ActivitySummary {
        self.shared
            .lock()
            .map(|s| s.summary.clone())
            .unwrap_or_default()
    }

    /// The processes being recorded.
    pub fn tracked(&self) -> Vec<u32> {
        self.shared
            .lock()
            .map(|s| s.tracked.clone())
            .unwrap_or_default()
    }

    pub fn state(&self) -> RecorderState {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.state.clone())
            .unwrap_or(RecorderState::Starting)
    }

    /// Records this process too (one the program started before).
    pub fn track(&self, pid: u32) {
        self.rundowns.store(true, Ordering::Relaxed);
        let _ = self.control.send(Control::Track(pid));
    }

    /// Stops recording; the last events can still be taken.
    pub fn stop(mut self) -> Vec<ActivityEvent> {
        self.halt();
        self.take()
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Stopping the session ends the consumer, which hands over the rest.
        drop(self.session.take());
        if let Some(handle) = self.consumer.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ActivityRecorder {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Events the session lost, readable from the consumer thread (which does
/// not own the session).
fn session_lost_reader(session: &Session) -> Box<dyn Fn() -> u32 + Send> {
    let name = session.name();
    Box::new(move || etw::lost_by_name(&name).0)
}

#[cfg(test)]
#[path = "activity/tests.rs"]
mod tests;
