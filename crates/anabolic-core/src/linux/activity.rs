//! Recording what processes do on Linux, Process Monitor style, from what
//! the kernel reports to any program allowed to listen:
//!
//! - files: fanotify, marked on every mounted file system. Each event names
//!   the process, the folder and the file. Watching needs cap_sys_admin,
//!   and naming the folders cap_dac_read_search. Linux reports what was
//!   done, not what was refused: an open that failed is not seen.
//! - processes: the kernel's process events (fork, exec, exit), else a look
//!   at the process list ten times a second.
//! - connections: the socket table four times a second: connections made,
//!   accepted and closed, and the bytes each carried (TCP).
//! - system calls, when asked (a program or processes, not the whole
//!   machine): the kernel's syscall tracepoints, see `syscalls`.
//!
//! The registry kinds do not occur on Linux.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CString;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::netlink::{self, DiagSocket, ProcEvent, ProcEvents};
use crate::nettrace::Proto;
use crate::procfs;
use crate::syscalls::{Call, Names, Object, Tracer};

use super::*;

impl ActivityEvent {
    /// The result in a word, as Process Monitor puts it.
    pub fn result_text(&self) -> String {
        match (self.kind, self.status) {
            (_, None) => String::new(),
            (ActivityKind::ProcessExit, Some(code)) => format!("exit code {code}"),
            // A failed system call: its errno's symbol, and what it means.
            (ActivityKind::Syscall(_), Some(errno)) if errno != 0 => {
                match (crate::syscalls::errno_name(errno), status_word(errno)) {
                    (Some(name), Some(word)) => format!("{name} ({word})"),
                    (Some(name), None) => name.to_string(),
                    _ => status_text(errno),
                }
            }
            (_, Some(status)) => status_text(status),
        }
    }

    /// `result_text` when it is a fixed phrase (a failed system call: its
    /// errno's symbol).
    pub fn result_word(&self) -> Option<&'static str> {
        match (self.kind, self.status) {
            (ActivityKind::ProcessExit, _) | (_, None) => None,
            (ActivityKind::Syscall(_), Some(errno)) if errno != 0 => {
                crate::syscalls::errno_name(errno)
            }
            (_, Some(status)) => status_word(status),
        }
    }

    /// An errno other than 0.
    pub fn failed(&self) -> bool {
        self.kind != ActivityKind::ProcessExit && self.status.is_some_and(|s| s != 0)
    }
}

/// An errno in words; the number itself for the rare ones.
pub fn status_text(status: u32) -> String {
    status_word(status)
        .map(str::to_string)
        .unwrap_or_else(|| format!("error {status}"))
}

/// An errno in words, when it has them.
pub fn status_word(status: u32) -> Option<&'static str> {
    Some(match status as i32 {
        0 => "success",
        libc::ENOENT => "no such file",
        libc::EACCES => "access denied",
        libc::EPERM => "not permitted",
        libc::EEXIST => "already exists",
        libc::EISDIR => "is a directory",
        libc::ENOTDIR => "not a directory",
        libc::ENOTEMPTY => "directory not empty",
        libc::EBUSY => "busy",
        libc::EAGAIN => "try again",
        libc::EINVAL => "invalid argument",
        libc::ECONNREFUSED => "connection refused",
        libc::ECONNRESET => "connection reset",
        libc::ETIMEDOUT => "timed out",
        libc::EINPROGRESS => "in progress",
        libc::EINTR => "interrupted",
        libc::EBADF => "bad descriptor",
        libc::ENOTTY => "not a terminal",
        libc::ESPIPE => "not seekable",
        libc::EPIPE => "broken pipe",
        libc::ENOSYS => "not implemented",
        libc::ENODATA => "no data",
        libc::EOPNOTSUPP => "not supported",
        libc::ENOTCONN => "not connected",
        libc::ENOTSOCK => "not a socket",
        libc::ESRCH => "no such process",
        libc::ECHILD => "no child process",
        libc::ENOMEM => "out of memory",
        libc::EFAULT => "bad address",
        libc::EMFILE => "too many open files",
        libc::ENOSPC => "no space left",
        libc::EROFS => "read-only file system",
        libc::ELOOP => "too many links",
        libc::ENAMETOOLONG => "name too long",
        libc::EXDEV => "across file systems",
        libc::ENXIO | libc::ENODEV => "no such device",
        libc::EIO => "input/output error",
        libc::ERANGE => "out of range",
        libc::EADDRINUSE => "address in use",
        libc::EALREADY => "already in progress",
        libc::EHOSTUNREACH => "host unreachable",
        libc::ENETUNREACH => "network unreachable",
        _ => return None,
    })
}

/// Reads and writes of one file by one process, sends and receives on one
/// connection, merge into one row while they keep coming within this.
const MERGE: Duration = Duration::from_millis(1000);
/// How often the socket table is read.
const NET_EVERY: Duration = Duration::from_millis(250);
/// How often the process list is read, without process events.
const PROCS_EVERY: Duration = Duration::from_millis(100);
const FLUSH_EVERY: Duration = Duration::from_millis(100);

/// Who to record from the start.
enum Target {
    /// Every process but Anabolic itself.
    Everything,
    Attach(Vec<u32>),
    Launch {
        program: String,
        args: String,
        dir: Option<String>,
    },
}

/// What the UI reads, swapped out on `take`.
#[derive(Default)]
struct Shared {
    out: Vec<ActivityEvent>,
    summary: ActivitySummary,
    tracked: Vec<u32>,
    state: Option<RecorderState>,
}

/// Records what a set of processes does; recording stops on drop.
pub struct ActivityRecorder {
    worker: Option<thread::JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
    control: Sender<u32>,
    stop: Arc<AtomicBool>,
}

impl ActivityRecorder {
    /// Records a running process, and what it starts from now on.
    pub fn attach(pid: u32) -> Result<Self, String> {
        Self::start(Target::Attach(vec![pid]), false)
    }

    /// Records every process of the machine but Anabolic itself: what is
    /// wanted is found by filtering after.
    pub fn everything() -> Result<Self, String> {
        Self::start(Target::Everything, false)
    }

    /// Records several running processes (a program and its children).
    pub fn attach_all(pids: &[u32]) -> Result<Self, String> {
        Self::start(Target::Attach(pids.to_vec()), false)
    }

    /// As `attach_all`, with every system call they make too.
    pub fn attach_with_syscalls(pids: &[u32]) -> Result<Self, String> {
        Self::start(Target::Attach(pids.to_vec()), true)
    }

    /// As `launch`, with every system call it makes too, from its first.
    pub fn launch_with_syscalls(
        program: &str,
        args: &str,
        dir: Option<&str>,
    ) -> Result<Self, String> {
        Self::start(
            Target::Launch {
                program: program.to_string(),
                args: args.to_string(),
                dir: dir.map(str::to_string),
            },
            true,
        )
    }

    /// Starts a program and records it from its first step. It runs as the
    /// user, with none of Anabolic's rights. Returns at once: `state` says
    /// when the program started, or why not.
    pub fn launch(program: &str, args: &str, dir: Option<&str>) -> Result<Self, String> {
        Self::launch_with(program, args, dir, LaunchHow::Normal { show: 1 })
    }

    /// As `launch`; on Linux every program starts without a terminal, so
    /// `how` changes nothing.
    pub fn launch_with(
        program: &str,
        args: &str,
        dir: Option<&str>,
        _how: LaunchHow,
    ) -> Result<Self, String> {
        Self::start(
            Target::Launch {
                program: program.to_string(),
                args: args.to_string(),
                dir: dir.map(str::to_string),
            },
            false,
        )
    }

    fn start(target: Target, syscalls: bool) -> Result<Self, String> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (control, control_rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (shared, stop) = (shared.clone(), stop.clone());
            thread::Builder::new()
                .name("anabolic-activity".into())
                .spawn(move || {
                    let mut recorder = Recorder::new(target, shared, control_rx, stop);
                    recorder.syscalls = syscalls;
                    recorder.run()
                })
                .map_err(|e| e.to_string())?
        };
        Ok(Self {
            worker: Some(worker),
            shared,
            control,
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
        let _ = self.control.send(pid);
    }

    /// Stops recording; the last events can still be taken.
    pub fn stop(mut self) -> Vec<ActivityEvent> {
        self.halt();
        self.take()
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ActivityRecorder {
    fn drop(&mut self) {
        self.halt();
    }
}

// ---------------------------------------------------------------------------
// Files: fanotify.

const FAN_ACCESS: u64 = 0x1;
const FAN_MODIFY: u64 = 0x2;
const FAN_CLOSE_WRITE: u64 = 0x8;
const FAN_OPEN: u64 = 0x20;
const FAN_CREATE: u64 = 0x100;
const FAN_DELETE: u64 = 0x200;
const FAN_Q_OVERFLOW: u64 = 0x4000;
const FAN_RENAME: u64 = 0x1000_0000;
const FAN_ONDIR: u64 = 0x4000_0000;
const FAN_CLOEXEC: u32 = 0x1;
const FAN_NONBLOCK: u32 = 0x2;
const FAN_REPORT_DIR_FID: u32 = 0x400;
const FAN_REPORT_NAME: u32 = 0x800;
const FAN_MARK_ADD: u32 = 0x1;
const FAN_MARK_FILESYSTEM: u32 = 0x100;
const INFO_DFID_NAME: u8 = 2;
const INFO_DFID: u8 = 3;
const INFO_OLD_DFID_NAME: u8 = 10;
const INFO_NEW_DFID_NAME: u8 = 12;

/// File systems with nothing a program keeps (the kernel's own views).
const PSEUDO: [&str; 22] = [
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devpts",
    "devtmpfs",
    "securityfs",
    "debugfs",
    "tracefs",
    "bpf",
    "pstore",
    "mqueue",
    "hugetlbfs",
    "configfs",
    "fusectl",
    "efivarfs",
    "autofs",
    "binfmt_misc",
    "nsfs",
    "rpc_pipefs",
    "selinuxfs",
    "ramfs",
];

/// One file event, named.
struct FileEvent {
    pid: u32,
    mask: u64,
    path: Arc<str>,
    /// A rename's new name.
    to: Option<Arc<str>>,
}

/// A fanotify group marked on every file system.
struct Files {
    fd: OwnedFd,
    /// A folder of each file system (by fsid), to open its handles from.
    mounts: HashMap<[i32; 2], OwnedFd>,
    /// Folder handles already named.
    dirs: HashMap<Vec<u8>, Arc<str>>,
    overflows: u32,
}

impl Files {
    fn open() -> Result<Self, String> {
        let flags = FAN_CLOEXEC | FAN_NONBLOCK | FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
        let fd = unsafe { libc::fanotify_init(flags, (libc::O_RDONLY | libc::O_LARGEFILE) as u32) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::EPERM) => "watching files needs cap_sys_admin".into(),
                _ => format!("fanotify: {}", crate::util::error_text(&e)),
            });
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mask = FAN_OPEN
            | FAN_ACCESS
            | FAN_MODIFY
            | FAN_CLOSE_WRITE
            | FAN_CREATE
            | FAN_DELETE
            | FAN_RENAME
            | FAN_ONDIR;
        let mut mounts = HashMap::new();
        let table = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        for line in table.lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            let Some(dash) = fields.iter().position(|f| *f == "-") else {
                continue;
            };
            let (Some(mount), Some(kind)) = (fields.get(4), fields.get(dash + 1)) else {
                continue;
            };
            if PSEUDO.contains(kind) || kind.starts_with("fuse") {
                continue;
            }
            let mount = unescape(mount);
            let Ok(path) = CString::new(mount.as_str()) else {
                continue;
            };
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statfs(path.as_ptr(), &mut st) } != 0 {
                continue;
            }
            // SAFETY: fsid_t is two ints.
            let fsid: [i32; 2] = unsafe { std::mem::transmute(st.f_fsid) };
            if mounts.contains_key(&fsid) {
                continue;
            }
            let marked = unsafe {
                libc::fanotify_mark(
                    fd.as_raw_fd(),
                    FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                    mask,
                    libc::AT_FDCWD,
                    path.as_ptr(),
                )
            };
            if marked != 0 {
                continue;
            }
            let dir = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if dir >= 0 {
                mounts.insert(fsid, unsafe { OwnedFd::from_raw_fd(dir) });
            }
        }
        if mounts.is_empty() {
            return Err("no file system could be watched".into());
        }
        Ok(Self {
            fd,
            mounts,
            dirs: HashMap::new(),
            overflows: 0,
        })
    }

    /// The path of a folder handle: opened by handle (cap_dac_read_search)
    /// and named by the kernel.
    fn folder(&mut self, fsid: [i32; 2], handle: &[u8]) -> Arc<str> {
        let mut key = Vec::with_capacity(8 + handle.len());
        key.extend_from_slice(&fsid[0].to_ne_bytes());
        key.extend_from_slice(&fsid[1].to_ne_bytes());
        key.extend_from_slice(handle);
        if let Some(path) = self.dirs.get(&key) {
            return path.clone();
        }
        let name: Arc<str> = match self.mounts.get(&fsid) {
            Some(mount) => {
                let mut buf = handle.to_vec();
                let fd = unsafe {
                    libc::syscall(
                        libc::SYS_open_by_handle_at,
                        mount.as_raw_fd(),
                        buf.as_mut_ptr(),
                        libc::O_PATH | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    match std::io::Error::last_os_error().raw_os_error() {
                        Some(libc::ESTALE) => "(a folder since deleted)".into(),
                        _ => "(a folder Anabolic may not name)".into(),
                    }
                } else {
                    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
                    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                        .map(|p| Arc::from(p.to_string_lossy().as_ref()))
                        .unwrap_or_else(|_| "(a folder)".into())
                }
            }
            None => "(a folder)".into(),
        };
        if self.dirs.len() > 100_000 {
            self.dirs.clear();
        }
        self.dirs.insert(key, name.clone());
        name
    }

    /// Every event waiting now. Returns how many the kernel delivered.
    fn read(&mut self, out: &mut Vec<FileEvent>) -> u64 {
        let mut buf = vec![0u8; 64 * 1024];
        let mut seen = 0;
        loop {
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            let mut at = 0;
            let n = n as usize;
            while at + 24 <= n {
                let len = u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
                if len < 24 || at + len > n {
                    break;
                }
                seen += 1;
                let event = &buf[at..at + len];
                at += len;
                let meta_len = u16::from_ne_bytes([event[6], event[7]]) as usize;
                let mask = u64::from_ne_bytes(event[8..16].try_into().unwrap());
                let pid = i32::from_ne_bytes(event[20..24].try_into().unwrap()) as u32;
                if mask & FAN_Q_OVERFLOW != 0 {
                    self.overflows += 1;
                    continue;
                }
                let (mut path, mut to) = (None, None);
                let mut info = meta_len;
                while info + 4 <= event.len() {
                    let kind = event[info];
                    let info_len = u16::from_ne_bytes([event[info + 2], event[info + 3]]) as usize;
                    if info_len < 4 || info + info_len > event.len() {
                        break;
                    }
                    let record = &event[info..info + info_len];
                    info += info_len;
                    if ![
                        INFO_DFID_NAME,
                        INFO_DFID,
                        INFO_OLD_DFID_NAME,
                        INFO_NEW_DFID_NAME,
                    ]
                    .contains(&kind)
                        || record.len() < 20
                    {
                        continue;
                    }
                    let fsid = [
                        i32::from_ne_bytes(record[4..8].try_into().unwrap()),
                        i32::from_ne_bytes(record[8..12].try_into().unwrap()),
                    ];
                    let handle_bytes =
                        u32::from_ne_bytes(record[12..16].try_into().unwrap()) as usize;
                    let Some(handle) = record.get(12..20 + handle_bytes) else {
                        continue;
                    };
                    let name = record[20 + handle_bytes..]
                        .split(|&b| b == 0)
                        .next()
                        .map(String::from_utf8_lossy)
                        .unwrap_or_default();
                    let folder = self.folder(fsid, handle);
                    let full: Arc<str> = if name.is_empty() || name == "." {
                        folder
                    } else if folder.ends_with('/') {
                        format!("{folder}{name}").into()
                    } else {
                        format!("{folder}/{name}").into()
                    };
                    if kind == INFO_NEW_DFID_NAME {
                        to = Some(full);
                    } else {
                        path = Some(full);
                    }
                }
                if let Some(path) = path {
                    out.push(FileEvent {
                        pid,
                        mask,
                        path,
                        to,
                    });
                }
            }
        }
        seen
    }
}

/// The mount table's escapes: "\040" for a space.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(code) = text
                .get(i + 1..i + 4)
                .and_then(|o| u8::from_str_radix(o, 8).ok())
        {
            out.push(code);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A shared library the loader opens: what Windows calls loading an image.
fn shared_object(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.ends_with(".so") || file.contains(".so.")
}

// ---------------------------------------------------------------------------
// The recorder.

struct Slot {
    ev: ActivityEvent,
    /// Still growing (merging) until then.
    open_until: Option<Instant>,
}

/// A connection being watched.
#[derive(Clone, Copy)]
struct Conn {
    pid: u32,
    remote: (IpAddr, u16),
    local_port: u16,
    sent: u64,
    received: u64,
}

struct Recorder {
    shared: Arc<Mutex<Shared>>,
    control: Receiver<u32>,
    stop: Arc<AtomicBool>,
    target: Option<Target>,
    everything: bool,
    own: u32,
    tracked: HashSet<u32>,
    files: Option<Files>,
    events: Option<ProcEvents>,
    /// Without process events: the processes at the last look.
    procs: HashSet<u32>,
    /// What each process runs, for its exit.
    images: HashMap<u32, Arc<str>>,
    queue: VecDeque<Slot>,
    /// Seq of `queue[0]`.
    base: u64,
    next_seq: u64,
    /// Open merging rows: (pid, kind, path) -> seq.
    runs: HashMap<(u32, ActivityKind, Arc<str>), u64>,
    conns: HashMap<u64, Conn>,
    conns_primed: bool,
    summary: ActivitySummary,
    written: HashSet<Arc<str>>,
    endpoints: HashSet<(IpAddr, u16)>,
    paths: HashSet<Arc<str>>,
    lost_events: u32,
    /// System calls were asked for.
    syscalls: bool,
    tracer: Option<Tracer>,
    /// Open rows of system calls: (thread, call, what it is about, result)
    /// -> seq.
    call_runs: HashMap<(u32, u32, Object, Option<u32>), u64>,
    /// Totals by call number.
    call_stats: HashMap<u32, SyscallStat>,
}

impl Recorder {
    fn new(
        target: Target,
        shared: Arc<Mutex<Shared>>,
        control: Receiver<u32>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            shared,
            control,
            stop,
            everything: matches!(target, Target::Everything),
            target: Some(target),
            own: std::process::id(),
            tracked: HashSet::new(),
            files: None,
            events: None,
            procs: HashSet::new(),
            images: HashMap::new(),
            queue: VecDeque::new(),
            base: 0,
            next_seq: 0,
            runs: HashMap::new(),
            conns: HashMap::new(),
            conns_primed: false,
            summary: ActivitySummary::default(),
            written: HashSet::new(),
            endpoints: HashSet::new(),
            paths: HashSet::new(),
            lost_events: 0,
            syscalls: false,
            tracer: None,
            call_runs: HashMap::new(),
            call_stats: HashMap::new(),
        }
    }

    fn set_state(&self, state: RecorderState) {
        if let Ok(mut s) = self.shared.lock() {
            s.state = Some(state);
        }
    }

    /// Whether this process' steps are recorded.
    fn wants(&self, pid: u32) -> bool {
        if self.everything {
            pid != self.own && pid != 0
        } else {
            self.tracked.contains(&pid)
        }
    }

    /// The shared copy of a path.
    fn intern(&mut self, path: &str) -> Arc<str> {
        if let Some(p) = self.paths.get(path) {
            return p.clone();
        }
        if self.paths.len() >= 500_000 {
            self.paths.clear();
        }
        let p: Arc<str> = path.into();
        self.paths.insert(p.clone());
        p
    }

    /// Adds a step at the end of the timeline. Reads, writes, sends and
    /// receives go into the open row of the same kind on the same thing,
    /// while there is one; for the whole machine, so do repeats of any
    /// other step.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        pid: u32,
        kind: ActivityKind,
        path: Arc<str>,
        detail: String,
        status: Option<u32>,
        bytes: u64,
    ) {
        let now = Instant::now();
        let merges = matches!(
            kind,
            ActivityKind::FileRead
                | ActivityKind::FileWrite
                | ActivityKind::NetSend
                | ActivityKind::NetReceive
        ) || self.everything;
        let key = (pid, kind, path.clone());
        if merges
            && let Some(&seq) = self.runs.get(&key)
            && let Some(slot) = seq
                .checked_sub(self.base)
                .and_then(|i| self.queue.get_mut(i as usize))
            && slot.open_until.is_some_and(|until| until > now)
            && slot.ev.detail == detail
            && slot.ev.status == status
        {
            slot.ev.count += 1;
            slot.ev.bytes += bytes;
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push_back(Slot {
            ev: ActivityEvent {
                seq,
                at: SystemTime::now(),
                pid,
                tid: 0,
                kind,
                path,
                detail,
                status,
                count: 1,
                bytes,
                took: Duration::ZERO,
            },
            open_until: merges.then_some(now + MERGE),
        });
        if merges {
            self.runs.insert(key, seq);
        }
    }

    /// A system call, at the end of the timeline. Calls a thread repeats on
    /// the same thing with the same result go into one row while it is
    /// open, so that a loop of poll or futex stays readable.
    fn on_call(&mut self, c: &Call, names: &mut Names) {
        let Ok(nr) = u16::try_from(c.nr) else {
            return;
        };
        let took = c.took().unwrap_or_default();
        let status = c.status();
        let stat = self.call_stats.entry(c.nr).or_insert_with(|| SyscallStat {
            name: ActivityKind::Syscall(nr).label(),
            ..Default::default()
        });
        stat.calls += 1;
        stat.errors += u64::from(c.errno().is_some());
        stat.time += took;
        let object = c.object();
        let bytes = c.bytes();
        let now = Instant::now();
        let key = (c.tid, c.nr, object, status);
        if let Some(&seq) = self.call_runs.get(&key)
            && let Some(slot) = seq
                .checked_sub(self.base)
                .and_then(|i| self.queue.get_mut(i as usize))
            && slot.open_until.is_some_and(|until| until > now)
        {
            slot.ev.count += 1;
            slot.ev.bytes += bytes;
            slot.ev.took += took;
            return;
        }
        let path = names.path(c, &key.2);
        let mut detail = c.text();
        match c.outcome {
            _ if c.args.is_none() => detail.push_str("  (began before it was seen)"),
            crate::syscalls::Outcome::Waiting => detail.push_str(
                if std::path::Path::new(&format!("/proc/{}/task/{}", c.pid, c.tid)).exists() {
                    "  (still in it when the recording stopped)"
                } else {
                    "  (the thread ended in it)"
                },
            ),
            crate::syscalls::Outcome::Unknown => detail.push_str("  (its end was lost)"),
            _ => {}
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push_back(Slot {
            ev: ActivityEvent {
                seq,
                at: crate::perf::wall(c.time),
                pid: c.pid,
                tid: c.tid,
                kind: ActivityKind::Syscall(nr),
                path,
                detail,
                status,
                count: 1,
                bytes,
                took,
            },
            open_until: Some(now + MERGE),
        });
        self.call_runs.insert(key, seq);
    }

    /// Every system call finished by now. `all`: the recording ends.
    fn read_calls(&mut self, all: bool) {
        let Some(mut tracer) = self.tracer.take() else {
            return;
        };
        self.summary.seen += tracer.read(all, |c, names| self.on_call(c, names));
        if all {
            tracer.unfinished(|c, names| self.on_call(c, names));
        }
        self.tracer = Some(tracer);
    }

    /// Hands finished steps over, in order. `all`: the recording ended.
    fn flush(&mut self, all: bool) {
        let now = Instant::now();
        let mut out = Vec::new();
        while let Some(front) = self.queue.front() {
            if !all && front.open_until.is_some_and(|until| until > now) {
                break;
            }
            let slot = self.queue.pop_front().expect("front");
            self.base += 1;
            // The whole machine: the reader keeps what it has room for.
            if self.everything || self.summary.kept < MAX_EVENTS {
                self.summary.kept += 1;
                out.push(slot.ev);
            } else {
                self.summary.dropped += 1;
            }
        }
        let base = self.base;
        self.runs.retain(|_, seq| *seq >= base);
        self.call_runs.retain(|_, seq| *seq >= base);
        let calls_lost = self.tracer.as_ref().map_or(0, |t| t.lost);
        self.summary.lost = (u64::from(self.lost_events)
            + u64::from(self.files.as_ref().map_or(0, |f| f.overflows))
            + calls_lost)
            .min(u64::from(u32::MAX)) as u32;
        if !self.call_stats.is_empty() {
            let mut stats: Vec<SyscallStat> = self.call_stats.values().cloned().collect();
            stats.sort_by(|a, b| {
                b.time
                    .cmp(&a.time)
                    .then(b.calls.cmp(&a.calls))
                    .then(a.name.cmp(b.name))
            });
            self.summary.syscalls = stats;
        }
        if let Ok(mut s) = self.shared.lock() {
            s.out.append(&mut out);
            s.summary = self.summary.clone();
            let mut tracked: Vec<u32> = self.tracked.iter().copied().collect();
            tracked.sort_unstable();
            s.tracked = tracked;
        }
    }

    // Processes.

    /// A process started a program: the parent's step when the parent is
    /// recorded (and the child is recorded from now on), else the process'
    /// own first step when it is recorded itself (a launched program).
    fn started(&mut self, pid: u32) {
        let Some(stat) = procfs::stat(pid) else {
            return;
        };
        let image: Arc<str> = procfs::exe(pid).unwrap_or(stat.comm).into();
        self.images.insert(pid, image.clone());
        let line = procfs::cmdline(pid).unwrap_or_default();
        let detail = format!("PID {pid}  \u{b7}  {line}");
        if self.wants(stat.ppid) {
            if !self.everything {
                self.tracked.insert(pid);
            }
            self.summary.processes_started += 1;
            self.push(
                stat.ppid,
                ActivityKind::ProcessStart,
                image,
                detail,
                None,
                0,
            );
        } else if self.tracked.contains(&pid) {
            self.push(pid, ActivityKind::ProcessStart, image, detail, None, 0);
        }
    }

    /// `status` as wait(2) gives it, when known: the exit code, or 128 and
    /// the signal that ended it, as a shell says.
    fn ended(&mut self, pid: u32, status: Option<u32>) {
        if let Some(t) = &mut self.tracer {
            t.names.forget(pid);
        }
        if !self.wants(pid) {
            self.images.remove(&pid);
            return;
        }
        let image = self
            .images
            .remove(&pid)
            .unwrap_or_else(|| format!("PID {pid}").into());
        let code = status.map(|s| match s & 0x7f {
            0 => (s >> 8) & 0xff,
            signal => 128 + signal,
        });
        self.push(
            pid,
            ActivityKind::ProcessExit,
            image,
            String::new(),
            code,
            0,
        );
    }

    fn on_process_event(&mut self, event: ProcEvent) {
        match event {
            // A child that has not started a program of its own yet: its
            // steps are its parent's program's.
            ProcEvent::Fork { parent, pid, tid } if pid == tid => {
                if self.wants(parent) && !self.everything {
                    self.tracked.insert(pid);
                }
                if let Some(image) = self.images.get(&parent).cloned() {
                    self.images.insert(pid, image);
                }
            }
            ProcEvent::Exec { pid } => self.started(pid),
            ProcEvent::Exit { pid, tid, status } if pid == tid => self.ended(pid, Some(status)),
            _ => {}
        }
    }

    /// Without process events: what started and ended since the last look.
    fn poll_processes(&mut self) {
        let now: HashSet<u32> = procfs::pids().into_iter().collect();
        let mut new: Vec<u32> = now.difference(&self.procs).copied().collect();
        new.sort_unstable();
        for pid in new {
            self.started(pid);
        }
        let gone: Vec<u32> = self.procs.difference(&now).copied().collect();
        for pid in gone {
            self.ended(pid, None);
        }
        self.procs = now;
    }

    // Files.

    fn on_file(&mut self, e: FileEvent) {
        if !self.wants(e.pid) {
            return;
        }
        let dir = e.mask & FAN_ONDIR != 0;
        let path = self.intern(&e.path);
        let ok = Some(0);
        if e.mask & FAN_CREATE != 0 {
            self.summary.files_created += 1;
            let detail = if dir { "folder" } else { "" };
            self.push(
                e.pid,
                ActivityKind::FileCreate,
                path.clone(),
                detail.into(),
                ok,
                0,
            );
        }
        if e.mask & FAN_RENAME != 0 {
            let to = e.to.as_deref().unwrap_or("");
            self.push(
                e.pid,
                ActivityKind::FileRename,
                path.clone(),
                format!("to {to}"),
                ok,
                0,
            );
        }
        if e.mask & FAN_DELETE != 0 {
            self.summary.files_deleted += 1;
            self.push(
                e.pid,
                ActivityKind::FileDelete,
                path.clone(),
                String::new(),
                ok,
                0,
            );
        }
        if e.mask & FAN_OPEN != 0 && !dir {
            if shared_object(&path) {
                self.summary.images_loaded += 1;
                self.push(
                    e.pid,
                    ActivityKind::ImageLoad,
                    path.clone(),
                    String::new(),
                    None,
                    0,
                );
            } else {
                self.push(
                    e.pid,
                    ActivityKind::FileOpen,
                    path.clone(),
                    String::new(),
                    ok,
                    0,
                );
            }
        }
        if e.mask & FAN_ACCESS != 0 {
            let kind = if dir {
                ActivityKind::FileListDir
            } else {
                ActivityKind::FileRead
            };
            self.push(e.pid, kind, path.clone(), String::new(), ok, 0);
        }
        if e.mask & FAN_MODIFY != 0 {
            if self.written.insert(path.clone()) {
                self.summary.files_written += 1;
            }
            self.push(
                e.pid,
                ActivityKind::FileWrite,
                path.clone(),
                String::new(),
                ok,
                0,
            );
        }
        // Closed after writing: the run of writes is over.
        if e.mask & FAN_CLOSE_WRITE != 0
            && let Some(&seq) = self.runs.get(&(e.pid, ActivityKind::FileWrite, path))
            && let Some(slot) = seq
                .checked_sub(self.base)
                .and_then(|i| self.queue.get_mut(i as usize))
        {
            slot.open_until = None;
        }
    }

    // Connections.

    /// The sockets each recorded process holds, by inode.
    fn socket_owners(&self, sockets: &[DiagSocket]) -> HashMap<u64, u32> {
        if self.everything {
            let inodes: Vec<u64> = sockets.iter().map(|s| s.inode).collect();
            return crate::sockets::owners(&inodes, Duration::from_secs(2));
        }
        let mut out = HashMap::new();
        for &pid in &self.tracked {
            let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for e in dir.flatten() {
                if let Some(inode) = std::fs::read_link(e.path()).ok().and_then(|t| {
                    t.to_str()?
                        .strip_prefix("socket:[")?
                        .strip_suffix(']')?
                        .parse::<u64>()
                        .ok()
                }) {
                    out.insert(inode, pid);
                }
            }
        }
        out
    }

    fn poll_connections(&mut self) {
        const ESTABLISHED: u8 = 1;
        const SYN_SENT: u8 = 2;
        const SYN_RECV: u8 = 3;
        const LISTEN: u8 = 10;
        let sockets = netlink::dump(Proto::Tcp).unwrap_or_default();
        let listening: HashSet<u16> = sockets
            .iter()
            .filter(|s| s.state == LISTEN)
            .map(|s| s.local.1)
            .collect();
        let live: Vec<&DiagSocket> = sockets
            .iter()
            .filter(|s| s.inode != 0 && s.state != LISTEN)
            .collect();
        let owners = self.socket_owners(&sockets);
        let mut seen = HashSet::with_capacity(live.len());
        for s in live {
            let Some(&pid) = owners.get(&s.inode) else {
                continue;
            };
            if !self.wants(pid) {
                continue;
            }
            seen.insert(s.cookie);
            let (sent, received) = (s.sent.unwrap_or(0), s.received.unwrap_or(0));
            let endpoint: Arc<str> = match s.remote.0 {
                IpAddr::V6(v6) => format!("[{v6}]:{}", s.remote.1),
                v4 => format!("{v4}:{}", s.remote.1),
            }
            .into();
            let detail = format!("TCP, local port {}", s.local.1);
            let before = match self.conns.get(&s.cookie) {
                Some(c) => *c,
                None => {
                    let new = Conn {
                        pid,
                        remote: s.remote,
                        local_port: s.local.1,
                        sent: 0,
                        received: 0,
                    };
                    if !self.conns_primed {
                        // Open before recording began: only its traffic
                        // from now on counts.
                        self.conns.insert(
                            s.cookie,
                            Conn {
                                sent,
                                received,
                                ..new
                            },
                        );
                        continue;
                    }
                    if matches!(s.state, ESTABLISHED | SYN_SENT | SYN_RECV) {
                        let kind = if listening.contains(&s.local.1) {
                            ActivityKind::NetAccept
                        } else {
                            ActivityKind::NetConnect
                        };
                        if self.endpoints.insert(s.remote) {
                            self.summary.endpoints += 1;
                        }
                        self.push(pid, kind, endpoint.clone(), detail.clone(), None, 0);
                    }
                    new
                }
            };
            let (tx, rx) = (
                sent.saturating_sub(before.sent),
                received.saturating_sub(before.received),
            );
            if tx > 0 {
                self.summary.sent += tx;
                self.push(
                    pid,
                    ActivityKind::NetSend,
                    endpoint.clone(),
                    detail.clone(),
                    None,
                    tx,
                );
            }
            if rx > 0 {
                self.summary.received += rx;
                self.push(pid, ActivityKind::NetReceive, endpoint, detail, None, rx);
            }
            self.conns.insert(
                s.cookie,
                Conn {
                    sent,
                    received,
                    ..before
                },
            );
        }
        if self.conns_primed {
            let gone: Vec<u64> = self
                .conns
                .keys()
                .filter(|c| !seen.contains(c))
                .copied()
                .collect();
            for cookie in gone {
                let c = self.conns.remove(&cookie).expect("listed");
                let endpoint: Arc<str> = match c.remote.0 {
                    IpAddr::V6(v6) => format!("[{v6}]:{}", c.remote.1),
                    v4 => format!("{v4}:{}", c.remote.1),
                }
                .into();
                self.push(
                    c.pid,
                    ActivityKind::NetDisconnect,
                    endpoint,
                    format!("TCP, local port {}", c.local_port),
                    None,
                    0,
                );
            }
        } else {
            self.conns.retain(|c, _| seen.contains(c));
        }
        self.conns_primed = true;
    }

    // The thread.

    /// Starts the program to record: as the user (Anabolic's capabilities
    /// do not pass to a program it starts), with no terminal. It is
    /// recorded from the moment it exists; the steps it takes meanwhile
    /// wait in the kernel's queues and are read right after. Its system
    /// calls, when asked, from its first. Says what is not recorded.
    fn launch(
        &mut self,
        program: &str,
        args: &str,
        dir: Option<&str>,
    ) -> Result<Option<String>, String> {
        let mut command = std::process::Command::new(program);
        command
            .args(split_args(args))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(dir) = dir.filter(|d| !d.is_empty()) {
            command.current_dir(dir);
        }
        let (child, problem) = match &mut self.tracer {
            Some(tracer) => tracer.launch(&mut command),
            None => (command.spawn(), None),
        };
        let mut child = child.map_err(|e| format!("{program} could not start: {e}"))?;
        self.tracked.insert(child.id());
        // Collected when it ends, so it does not linger as a zombie.
        let _ = thread::Builder::new()
            .name("anabolic-launched".into())
            .spawn(move || child.wait());
        Ok(problem)
    }

    fn run(mut self) {
        let mut problems = Vec::new();
        match Files::open() {
            Ok(files) => self.files = Some(files),
            Err(e) => problems.push(format!("file activity is not recorded: {e}")),
        }
        // On this thread: it reads the trace, and its buffers live with it.
        if self.syscalls && !self.everything {
            match Tracer::new() {
                Ok(tracer) => self.tracer = Some(tracer),
                Err(e) => problems.push(format!("system calls are not recorded: {e}")),
            }
        }
        self.events = ProcEvents::open().ok();
        if self.events.is_none() {
            self.procs = procfs::pids().into_iter().collect();
        }
        let target = self.target.take();
        if let Some(Target::Attach(pids)) = &target {
            self.tracked.extend(pids);
            for &pid in pids {
                if let Some(image) = procfs::exe(pid) {
                    self.images.insert(pid, image.into());
                }
            }
            if let Some(why) = self.tracer.as_mut().and_then(|t| t.follow(pids)) {
                problems.push(format!("system calls: {why}"));
            }
        }
        // The connections open now: what recorded processes do with them
        // from now on counts; a launched program's are all new.
        self.poll_connections();
        if let Some(Target::Launch { program, args, dir }) = &target {
            match self.launch(program, args, dir.as_deref()) {
                Ok(None) => {}
                Ok(Some(why)) => problems.push(format!("system calls are not recorded: {why}")),
                Err(e) => {
                    self.set_state(RecorderState::Failed(e));
                    self.flush(true);
                    return;
                }
            }
        }
        self.set_state(if problems.is_empty() {
            RecorderState::Recording
        } else {
            RecorderState::Failed(capitalized(&problems.join("; ")))
        });
        let (mut last_net, mut last_procs, mut last_flush) =
            (Instant::now(), Instant::now(), Instant::now());
        let mut files = Vec::new();
        let mut events = Vec::new();
        loop {
            let stopping = self.stop.load(Ordering::Relaxed);
            while let Ok(pid) = self.control.try_recv() {
                self.tracked.insert(pid);
                if let Some(t) = &mut self.tracer {
                    t.follow(&[pid]);
                }
            }
            let mut waits = Vec::with_capacity(2);
            if let Some(f) = &self.files {
                waits.push(libc::pollfd {
                    fd: f.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some(e) = &self.events {
                waits.push(libc::pollfd {
                    fd: e.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            if let Some(t) = &self.tracer {
                waits.extend(t.fds().map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                }));
            }
            if !stopping {
                unsafe { libc::poll(waits.as_mut_ptr(), waits.len() as libc::nfds_t, 50) };
            }
            // Processes first: a child's own steps must find it tracked.
            if let Some(source) = &self.events {
                events.clear();
                if source.drain(&mut events) {
                    self.lost_events += 1;
                }
                self.summary.seen += events.len() as u64;
                for event in std::mem::take(&mut events) {
                    self.on_process_event(event);
                }
            } else if last_procs.elapsed() >= PROCS_EVERY {
                last_procs = Instant::now();
                self.poll_processes();
            }
            if let Some(source) = &mut self.files {
                files.clear();
                self.summary.seen += source.read(&mut files);
                for event in std::mem::take(&mut files) {
                    self.on_file(event);
                }
            }
            self.read_calls(stopping);
            if last_net.elapsed() >= NET_EVERY || stopping {
                last_net = Instant::now();
                self.poll_connections();
            }
            if stopping {
                self.flush(true);
                return;
            }
            if last_flush.elapsed() >= FLUSH_EVERY {
                last_flush = Instant::now();
                self.flush(false);
            }
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A command line's arguments, split as a shell would split them: by
/// spaces, with "double" and 'single' quotes and backslashes.
fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = line.chars();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => {
                single = !single;
                started = true;
            }
            '"' if !single => {
                double = !double;
                started = true;
            }
            '\\' if !single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    started = true;
                }
            }
            c if c.is_whitespace() && !single && !double => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_command_line() {
        assert_eq!(
            split_args(r#"-a "two words" 'single \x' back\ slash"#),
            vec!["-a", "two words", r"single \x", "back slash"]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args(r#""""#), vec![""]);
    }

    /// The calls a thread repeats on the same thing with the same result
    /// are one row; the totals count every call.
    #[test]
    fn merges_repeated_system_calls() {
        use crate::syscalls::Outcome;
        let (_control, control) = unbounded();
        let mut r = Recorder::new(
            Target::Attach(Vec::new()),
            Arc::default(),
            control,
            Arc::default(),
        );
        let mut names = Names::default();
        // A process that is not there: its descriptors go unnamed.
        let read = |time: u64, ret: i64| Call {
            pid: 4_000_000_000,
            tid: 2,
            nr: 0,
            time,
            end: time + 1_000,
            args: Some([5, 0, 64, 0, 0, 0]),
            names: Vec::new(),
            outcome: Outcome::Returned(ret),
        };
        for i in 0..3 {
            r.on_call(&read(i * 10_000, 64), &mut names);
        }
        r.on_call(&read(40_000, -i64::from(libc::EAGAIN)), &mut names);
        r.on_call(
            &Call {
                tid: 3,
                ..read(50_000, 64)
            },
            &mut names,
        );
        r.on_call(
            &Call {
                args: None,
                ..read(60_000, 1)
            },
            &mut names,
        );
        r.flush(true);
        let out = std::mem::take(&mut r.shared.lock().unwrap().out);
        let rows: Vec<(u32, u32, u64, Option<u32>)> = out
            .iter()
            .map(|e| (e.tid, e.count, e.bytes, e.status))
            .collect();
        assert_eq!(
            rows,
            [
                (2, 3, 192, Some(0)),
                (2, 1, 0, Some(11)),
                (3, 1, 64, Some(0)),
                (2, 1, 1, Some(0)),
            ]
        );
        let first = &out[0];
        assert_eq!(first.kind.label(), "read");
        assert_eq!(first.kind.group(), Group::Syscall);
        assert_eq!(
            (&*first.path, first.detail.as_str()),
            ("fd 5", "read(5, NULL, 64) = 64")
        );
        assert_eq!(first.took, Duration::from_micros(3));
        assert_eq!(out[1].result_text(), "EAGAIN (try again)");
        assert_eq!(out[1].result_word(), Some("EAGAIN"));
        assert!(out[1].failed());
        assert_eq!(out[3].detail, "read(…) = 1  (began before it was seen)");
        let summary = r.summary.syscalls;
        assert_eq!(summary.len(), 1);
        assert_eq!(
            (summary[0].name, summary[0].calls, summary[0].errors),
            ("read", 6, 1)
        );
        assert_eq!(summary[0].time, Duration::from_micros(5));
    }

    /// A program's system calls from its first, with the rights for them
    /// (root, or the capabilities Anabolic is given); skipped without.
    #[test]
    fn records_system_calls_when_allowed() {
        let mut recorder = ActivityRecorder::launch_with_syscalls(
            "sh",
            "-c 'cat /etc/hostname > /dev/null'",
            None,
        )
        .unwrap();
        let mut events = Vec::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            events.extend(recorder.take());
            if events.iter().any(|e| e.kind == ActivityKind::ProcessExit) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if let RecorderState::Failed(why) = recorder.state()
            && why.contains("system calls are not recorded")
        {
            eprintln!("{why}: skipping");
            return;
        }
        let summary = recorder.summary();
        events.extend(recorder.stop());
        let calls: Vec<&ActivityEvent> = events
            .iter()
            .filter(|e| e.kind.group() == Group::Syscall)
            .collect();
        let named = |name: &str| calls.iter().any(|e| e.kind.label() == name);
        assert!(
            named("openat") && named("read") && named("exit_group"),
            "{calls:#?}"
        );
        assert!(summary.syscalls.iter().any(|s| s.name == "openat"));
        // With a kernel that copies file names, the file read is named.
        eprintln!(
            "/etc/hostname named: {}",
            calls.iter().any(|e| e.path.ends_with("/etc/hostname"))
        );
    }

    #[test]
    fn knows_a_shared_library() {
        assert!(shared_object("/usr/lib/libc.so.6"));
        assert!(shared_object("/usr/lib/libfoo.so"));
        assert!(!shared_object("/home/me/notes.solution"));
    }

    /// A program launched for recording: its start, its connection to a
    /// listener here and its end are recorded (its files too, when
    /// fanotify is allowed).
    #[test]
    fn records_a_launched_program() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            buf.len()
        });
        // bash's /dev/tcp: a connection without curl or nc.
        let script = format!(
            "exec 3<>/dev/tcp/127.0.0.1/{port}; printf hello >&3; sleep 0.6; exec 3>&-; \
             cat /etc/hostname > /dev/null"
        );
        let mut recorder =
            ActivityRecorder::launch("bash", &format!("-c '{script}'"), None).unwrap();
        let mut events = Vec::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            events.extend(recorder.take());
            if events.iter().any(|e| e.kind == ActivityKind::ProcessExit) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        events.extend(recorder.stop());
        assert_eq!(accept.join().unwrap(), 5);
        let kinds: Vec<ActivityKind> = events.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&ActivityKind::ProcessExit), "{kinds:?}");
        let connect = events
            .iter()
            .find(|e| e.kind == ActivityKind::NetConnect)
            .expect("the connection is recorded");
        assert_eq!(&*connect.path, format!("127.0.0.1:{port}"));
        assert!(
            events
                .iter()
                .any(|e| e.kind == ActivityKind::NetSend && e.bytes >= 5),
            "{events:?}"
        );
        assert!(kinds.contains(&ActivityKind::NetDisconnect), "{kinds:?}");
    }
}
